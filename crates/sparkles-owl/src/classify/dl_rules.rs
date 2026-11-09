//! The rules of roadmap item 603's Tier D for ALCHIQ: Hyper, Eq, Ineq, Factor, Succ and Pred
//! of the calculus of Bate, Motik, Cuenca Grau, Tena Cucala, Simančík and Horrocks (JAIR
//! 2018, Table 2), over the saturation's contexts, beside the EL rules.
//!
//! A context's units `→ A(x)` are its subsumers, and the EL rules are Hyper, Succ and Pred
//! on units: told subsumers, conjunctions and disjointness are Hyper with Horn clauses, an
//! existential restriction is Hyper and Succ with the cautious strategy, whose successor is
//! the context of its filler with the role's range, and the propagation of negative
//! existential restrictions over backward links is Hyper and Pred. The rules here are the
//! rest: Hyper on the clauses that are not units and with the clauses that have two or more
//! disjuncts, the universal restrictions over both the successors and the predecessor,
//! the at-most restrictions and the qualified roles that they count by, disjoint roles,
//! the equality rules, Succ for the literals over successor terms, with the markers
//! `A → A` of the atoms that may hold of a successor, and Pred of the clauses over the
//! predecessor.
//!
//! A context's store holds its clauses that are not units, and is allocated with the first
//! one. Its units serve as premises in place, so the store needs no copy of them: a unit
//! existential restriction e holds `R_e(x, f_e(x))` and the atoms of its EL successor's core
//! over `f_e(x)`. An existential restriction e that a context holds as a unit has the EL
//! rules' successor, and the context sends it the markers of the atoms its clauses give
//! `f_e(x)`. A context whose clause has e only conditionally registers with e's
//! hypothetical successor `h_e`, a context with the empty core, and sends it every marker
//! it needs for e. Once it has, `h_e` takes every later marker for e too, so that one edge
//! for `f_e` holds every atom that may hold of the successor, which the Succ rule's
//! condition asks. A context answers its predecessors, those of its backward links and
//! those that registered, with the clauses whose head is over the predecessor alone, which
//! each predecessor resolves with its own clauses over `f_e(x)`.
//!
//! A role atom over the predecessor in a body, `s(y, x)`, is the marker of that atom that
//! the paper's Succ rule would send, resolved in place: the predecessor resolves it with
//! its own role literals over the successor term. The roles of literals are the index's
//! roles, read through the hierarchy, and the qualified roles `S_B` of the at-most
//! restrictions (DL4 of Table 1), numbered above them, which only themselves are below.
//!
//! Every rule reads only its own context's state and sends conclusions to itself, to the
//! successors it registered with or to the predecessors it holds, as the EL rules do.

use super::dl_clause::{
    Clause, EQ, F, Lit, Lits, Made, NE, RL, RTerm, RY, Store, Table, Term, X, XL, Y,
};
use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use super::saturate::{Concl, Sink, State, flatten, table_bytes};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A clause of the axioms with two or more disjuncts: its body, where an empty one is
/// `owl:Thing`, and its head.
pub type AxClause = (Box<[Ex]>, Box<[Ex]>);

/// What Tier D reads in the index.
#[derive(Default)]
pub struct DlIx {
    /// the clauses with two or more disjuncts, by id: the body, where an empty one is
    /// `owl:Thing`, and the head
    pub clauses: Vec<AxClause>,
    /// for each body atom, the clauses whose body has it
    pub by_body: FxHashMap<Ex, Vec<u32>>,
    /// the disjointness sets, by the index's numbering
    pub sets: Vec<Vec<Ex>>,
    /// the hypothetical successor of each existential restriction, and back
    pub hyp: FxHashMap<Ex, Ex>,
    pub of_hyp: FxHashMap<Ex, Ex>,
    /// the core of each existential restriction's successor: its filler and the role's
    /// range, as the sorted expressions that are not intersections
    cores: FxHashMap<Ex, Box<[Ex]>>,
    /// for each successor context, the existential restrictions whose successor it is
    by_succ: FxHashMap<Ex, Vec<Ex>>,
    /// for each role, whether the role of an existential restriction is below it, so that
    /// a universal on it can meet a successor
    reach: Vec<bool>,
    /// for each role, the existential restrictions whose role is below it, by id
    below: Vec<Vec<Ex>>,
    /// whether the run takes a number restriction, so that successor terms can be equal
    pub counting: bool,
    /// the first qualified role; qualified role k is `base + 2k`, its inverse `base + 2k + 1`
    base: Role,
    /// the role and the filler of each qualified role `S_B`
    quals: Vec<(Role, Ex)>,
    /// for each filler B, the qualified roles `S_B`
    quals_of: FxHashMap<Ex, Vec<Role>>,
    /// the at-most restrictions: for each holder, the bound and the role it counts by
    atmost: FxHashMap<Ex, (u32, Role)>,
    /// the holders, bounds and roles of the at-most restrictions
    atmosts: Vec<(Ex, u32, Role)>,
    /// the pairs of disjoint roles, closed under inverses and order
    pub(super) rdis: Vec<(Role, Role)>,
    /// with counting, for each role and qualified role t, whether a role literal below t
    /// can hold over a successor term: the roles of the existential restrictions, the
    /// qualified roles they give, and their inverses once a predecessor can be counted
    carry: Vec<bool>,
    /// in a rooted run, the context of the root, which has the empty core
    pub root: Option<Ex>,
    /// in a rooted run, each constant with its singleton class `N_u`, by constant
    pub consts: Vec<(u32, Ex)>,
    /// in a rooted run with counting, the existential restrictions of each minimum's
    /// holder, whose successor terms differ
    pub ne_of: FxHashMap<Ex, Vec<Ex>>,
}

impl DlIx {
    pub fn new(dl: &super::dl_norm::DlAx, sets: &[Vec<Ex>], ix: &Index, nodes: usize) -> DlIx {
        let mut d = DlIx {
            sets: sets.to_vec(),
            hyp: dl.hyp.clone(),
            of_hyp: dl.hyp.iter().map(|(&e, &h)| (h, e)).collect(),
            counting: dl.counting,
            root: dl.root,
            ne_of: dl.ne.iter().cloned().collect(),
            ..DlIx::default()
        };
        for (b, h) in &dl.clauses {
            let id = d.clauses.len() as u32;
            let mut b = b.clone();
            if b.is_empty() {
                b.push(TOP);
            }
            b.sort_unstable();
            b.dedup();
            for &a in &b {
                d.by_body.entry(a).or_default().push(id);
            }
            d.clauses
                .push((b.into_boxed_slice(), h.clone().into_boxed_slice()));
        }
        let mut roles: Vec<Role> = Vec::new();
        let n = ix.roles.sup.len();
        d.below = vec![Vec::new(); n];
        // the roles whose successors a rule asks for: the universals' and the propagations'
        // (`alls`), and with counting
        // the at-most restrictions' and the inverses of the qualified roles'; only these
        // get lists, so that a role with many superroles costs nothing for the others
        let mut wanted = vec![false; n];
        let mut ps = Vec::new();
        for e in 0..nodes as Ex {
            alls(ix, e, &mut ps);
            for &(r, _) in &ps {
                if let Some(w) = wanted.get_mut(r as usize) {
                    *w = true;
                }
            }
        }
        for &(_, _, s, b) in &dl.atmost {
            let r = if b == TOP { Some(s) } else { ix.roles.inv(s) };
            if let Some(w) = r.and_then(|r| wanted.get_mut(r as usize)) {
                *w = true;
            }
        }
        for e in 0..nodes as Ex {
            let h = ix.head(e);
            if d.root.is_some()
                && let Node::Nominal(k) = h.node
            {
                d.consts.push((k, e));
            }
            if let Node::Exist(r, _) = h.node {
                let mut core = Vec::new();
                flatten(ix, h.succ, &mut core);
                d.cores.insert(e, core.into_boxed_slice());
                d.by_succ.entry(h.succ).or_default().push(e);
                roles.push(r);
                for &s in ix
                    .roles
                    .sup
                    .get(r as usize)
                    .map_or(&[][..], |v| v.as_slice())
                {
                    if wanted.get(s as usize) == Some(&true)
                        && let Some(b) = d.below.get_mut(s as usize)
                    {
                        b.push(e);
                    }
                }
                if wanted.get(r as usize) == Some(&true)
                    && let Some(b) = d.below.get_mut(r as usize)
                    && b.last() != Some(&e)
                {
                    b.push(e);
                }
            }
        }
        roles.sort_unstable();
        roles.dedup();
        d.reach = vec![false; n];
        for &r in &roles {
            for &s in ix
                .roles
                .sup
                .get(r as usize)
                .map_or(&[][..], |v| v.as_slice())
            {
                if let Some(x) = d.reach.get_mut(s as usize) {
                    *x = true;
                }
            }
            if let Some(x) = d.reach.get_mut(r as usize) {
                *x = true;
            }
        }
        // the qualified roles above the index's, an even number so that a role's inverse
        // flips its last bit
        d.base = (n as Role + 1) & !1;
        let mut qual: FxHashMap<(Role, Ex), Role> = FxHashMap::default();
        for &(m, k, s, b) in &dl.atmost {
            let q = if b == TOP {
                s
            } else {
                *qual.entry((s, b)).or_insert_with(|| {
                    let q = d.base + 2 * d.quals.len() as Role;
                    d.quals.push((s, b));
                    d.quals_of.entry(b).or_default().push(q);
                    q
                })
            };
            d.atmost.insert(m, (k, q));
            d.atmosts.push((m, k, q));
        }
        for &(a, b) in &dl.rdis {
            let mut add = |x: Role, y: Role| {
                d.rdis.push((x, y));
                d.rdis.push((y, x));
            };
            add(a, b);
            if let (Some(i), Some(j)) = (ix.roles.inv(a), ix.roles.inv(b)) {
                add(i, j);
            }
        }
        d.rdis.sort_unstable();
        d.rdis.dedup();
        if d.counting {
            d.carry = d.carried(ix, &roles);
        }
        d
    }

    /// The roles whose literals can hold over a successor term, closed upwards. A term's
    /// literals are its existential restriction's role, the qualified roles `S_B` whose S
    /// is above a carried role, their inverses when S's inverse is, and the roles of the
    /// terms it is merged with. Merging the predecessor with a successor term gives the
    /// predecessor the inverses of that term's roles, which only an at-most restriction
    /// that counts the predecessor can do.
    fn carried(&self, ix: &Index, roles: &[Role]) -> Vec<bool> {
        let total = self.base as usize + 2 * self.quals.len();
        let mut g = vec![false; total];
        for &r in roles {
            g[r as usize] = true;
        }
        loop {
            let before = g.iter().filter(|&&b| b).count();
            for t in 0..self.base as usize {
                if g[t] {
                    for &s in ix.roles.sup.get(t).map_or(&[][..], |v| v.as_slice()) {
                        g[s as usize] = true;
                    }
                }
            }
            for (k, &(r, _)) in self.quals.iter().enumerate() {
                let q = self.base as usize + 2 * k;
                if g[r as usize] {
                    g[q] = true;
                }
                if ix.roles.inv(r).is_some_and(|i| g[i as usize]) {
                    g[q + 1] = true;
                }
            }
            if self
                .atmosts
                .iter()
                .any(|&(_, _, q)| self.inv(ix, q).is_some_and(|i| g[i as usize]))
            {
                for t in 0..total {
                    if g[t]
                        && let Some(i) = self.inv(ix, t as Role)
                    {
                        g[i as usize] = true;
                    }
                }
            }
            if g.iter().filter(|&&b| b).count() == before {
                return g;
            }
        }
    }

    /// With counting, whether a role literal below t can hold over a successor term, so
    /// that a clause with the body atom `t(y, x)` can meet a predecessor.
    #[inline]
    fn carries(&self, t: Role) -> bool {
        !self.counting || self.carry.get(t as usize).copied().unwrap_or(false)
    }

    pub fn bytes(&self) -> usize {
        self.clauses
            .iter()
            .map(|(b, h)| 40 + (b.len() + h.len()) * 4)
            .sum::<usize>()
            + self
                .by_body
                .values()
                .map(|v| 48 + v.capacity() * 4)
                .sum::<usize>()
            + self
                .sets
                .iter()
                .map(|s| 24 + s.capacity() * 4)
                .sum::<usize>()
            + table_bytes(self.hyp.capacity(), 8) * 2
            + self.cores.values().map(|c| 24 + c.len() * 4).sum::<usize>()
            + self
                .by_succ
                .values()
                .map(|v| 32 + v.capacity() * 4)
                .sum::<usize>()
            + self.reach.len()
            + self
                .below
                .iter()
                .map(|v| 24 + v.capacity() * 4)
                .sum::<usize>()
            + self.quals.capacity() * 8
            + table_bytes(self.quals_of.capacity(), 32)
            + table_bytes(self.atmost.capacity(), 12)
            + self.atmosts.capacity() * 12
            + self.rdis.capacity() * 8
            + self.carry.capacity()
            + self.consts.capacity() * 8
            + table_bytes(self.ne_of.capacity(), 32)
            + self.ne_of.values().map(|v| v.capacity() * 4).sum::<usize>()
    }

    /// The core of the successor that the EL rules give e.
    pub(super) fn core(&self, e: Ex) -> &[Ex] {
        self.cores.get(&e).map_or(&[][..], |c| c)
    }

    /// Whether `me` is the root of a rooted run.
    #[inline]
    pub fn is_root(&self, me: Ex) -> bool {
        self.root == Some(me)
    }

    /// Whether a universal on `t` can meet a successor.
    #[inline]
    fn reaches(&self, t: Role) -> bool {
        self.reach.get(t as usize).copied().unwrap_or(false)
    }

    /// Whether `a` is in the core of the successor that the EL rules give e.
    #[inline]
    fn in_core(&self, e: Ex, a: Ex) -> bool {
        a == TOP
            || self
                .cores
                .get(&e)
                .is_some_and(|c| c.binary_search(&a).is_ok())
    }

    /// Whether r is below s: through the index's hierarchy, and a qualified role only below
    /// itself.
    #[inline]
    pub(super) fn sub(&self, ix: &Index, r: Role, s: Role) -> bool {
        if r >= self.base || s >= self.base {
            r == s
        } else {
            ix.roles.sub(r, s)
        }
    }

    /// The inverse of a role.
    #[inline]
    pub(super) fn inv(&self, ix: &Index, r: Role) -> Option<Role> {
        if r >= self.base {
            Some(r ^ 1)
        } else {
            ix.roles.inv(r)
        }
    }
}

/// What a saturation with Tier D shares: the table of the clauses that contexts send one
/// another and the bytes of the clause stores.
#[derive(Default)]
pub struct DlRun {
    pub table: Table,
    /// the literals of a rooted run that the packed forms cannot hold
    pub lits: Lits,
    /// whether the Succ rule takes the eager strategy instead of the cautious one
    pub eager: bool,
    stores: AtomicUsize,
    /// the clause with an empty body and head, which an unsatisfiable context sends
    empty: OnceLock<u32>,
    /// the conclusions derived, kept or not, and the most the run may derive before it
    /// passes its budget, zero for no bound
    pub(super) work: AtomicUsize,
    work_limit: AtomicUsize,
    /// set once the run passes its bound on derived conclusions
    worked: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl DlRun {
    pub fn new(eager: bool, worked: std::sync::Arc<std::sync::atomic::AtomicBool>) -> DlRun {
        DlRun {
            eager,
            worked,
            ..DlRun::default()
        }
    }
    pub fn bytes(&self) -> usize {
        self.table.bytes() + self.lits.bytes() + self.stores.load(Ordering::Relaxed)
    }
    pub(super) fn grow(&self, bytes: usize) {
        self.stores.fetch_add(bytes, Ordering::Relaxed);
    }
    /// Bounds the conclusions the run derives, which bounds its time where redundant
    /// conclusions keep its memory flat.
    pub fn set_work_limit(&self, n: usize) {
        self.work_limit.store(n, Ordering::Relaxed);
    }
    /// The conclusions derived so far.
    pub fn work(&self) -> usize {
        self.work.load(Ordering::Relaxed)
    }
    /// Passes the bound on derived conclusions at once, for an inference too wide to make.
    fn exhaust(&self) {
        self.work.fetch_add(usize::MAX / 4, Ordering::Relaxed);
    }
    /// Whether the run derived more conclusions than its bound.
    pub fn over_work(&self) -> bool {
        let l = self.work_limit.load(Ordering::Relaxed);
        let over = l != 0 && self.work.load(Ordering::Relaxed) > l;
        if over {
            self.worked.store(true, Ordering::Relaxed);
        }
        over
    }
    pub(super) fn empty(&self) -> u32 {
        *self
            .empty
            .get_or_init(|| self.table.push(Clause::new(Vec::new(), Vec::new())))
    }
}

/// What a context keeps for Tier D.
#[derive(Default, Clone)]
pub struct DlSt {
    pub store: Store,
    /// the clauses over the predecessor that successors sent, by successor
    pub pred_in: FxHashMap<Ex, Vec<u32>>,
    pub(super) pred_seen: FxHashSet<(Ex, u32)>,
    /// the predecessors that registered, with the existential restriction they reach this
    /// context by
    pub dpreds: Vec<(Ex, Ex)>,
    /// the successors this context registered with, by existential restriction: the
    /// hypothetical successor, or under the eager strategy the context of what holds of
    /// the successor for certain
    pub edges: Vec<(Ex, Ex)>,
    /// the markers this context holds
    hyps: FxHashSet<Ex>,
    /// the markers sent, by target
    sent: FxHashSet<(Ex, Ex)>,
    /// the table ids of the clauses `s(y, x) → c(y)` of the propagations, which the
    /// registered predecessors take
    props: FxHashMap<(Role, Ex), u32>,
    /// in a test context, the named classes that do not hold, sorted
    pub neg: Vec<Ex>,
    /// the choices of products derived since the last check of the run's bounds
    choices: u32,
    /// in the root of a rooted run, its indexes by constant
    pub root: Option<Box<super::dl_root::RootSt>>,
}

impl DlSt {
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<DlSt>()
            + self.store.bytes()
            + self
                .pred_in
                .values()
                .map(|v| 32 + v.capacity() * 4)
                .sum::<usize>()
            + table_bytes(self.pred_seen.capacity(), 8)
            + self.dpreds.capacity() * 8
            + self.edges.capacity() * 8
            + table_bytes(self.hyps.capacity(), 4)
            + table_bytes(self.sent.capacity(), 8)
            + table_bytes(self.props.capacity(), 12)
            + self.root.as_ref().map_or(0, |r| r.bytes())
    }

    /// Whether the context has no clause waiting for the rules.
    pub fn is_idle(&self) -> bool {
        self.store.is_idle()
    }

    /// Whether the context holds the marker of `a`.
    pub fn has_hyp(&self, a: Ex) -> bool {
        self.hyps.contains(&a)
    }
}

/// The bit that marks the class a test context's core denies.
const NEG: Ex = 1 << 31;

/// The test context of whether `x` is below one of `bs`, sorted: its core is `x` and the
/// complements of `bs`.
pub fn test(d: &super::saturate::Dynamic, x: Ex, bs: &[Ex]) -> Ex {
    let mut core = vec![x];
    core.extend(bs.iter().map(|&b| b | NEG));
    d.intern(&core)
}

/// Starts a test context: its core, `owl:Thing` and the class it denies.
pub fn init_test<S: Sink>(st: &mut State, core: &[Ex], out: &mut S) {
    out.me(Concl::Sub(TOP));
    for &m in core {
        if m & NEG != 0 {
            ds(st).neg.push(m & !NEG);
        } else {
            out.me(Concl::Sub(m));
        }
    }
}

/// The candidate subsumers of the classes that Tier D tests: the named classes that the
/// canonical model of each root's context makes true and that are not among its
/// subsumers, nor tested before.
///
/// The root's context is saturated under a total order of atoms, so its clauses have a
/// canonical model, built as in the completeness proof of the calculus (JAIR 2018,
/// Appendix C): the clauses whose body is empty and whose maximal literal is an atom over
/// x, in the order of that literal, each make it true unless another of its literals
/// already is. That model is one of the ontology and the root's core, so a class it leaves
/// false is not a subsumer, and only the classes it makes true need a test.
pub fn candidates(
    ix: &Index,
    ctx: &super::saturate::Contexts,
    roots: &[Ex],
    tested: &FxHashSet<(Ex, Ex)>,
) -> Vec<(Ex, Ex)> {
    let mut out = Vec::new();
    let mut max = Vec::new();
    for &x in roots {
        let Some(c) = ctx.get(x as usize).and_then(|c| c.get()) else {
            continue;
        };
        let st = c.st.lock();
        if st.bottom {
            continue;
        }
        let Some(d) = dl(&st) else { continue };
        let mut cs: Vec<((u8, Ex), &Clause)> = Vec::new();
        for s in &d.store.clauses {
            if s.dead || !s.c.body().is_empty() {
                continue;
            }
            s.c.maximal(ix, &mut max);
            if let [m] = max.as_slice()
                && m.kind() == X
            {
                cs.push((super::dl_clause::rank(ix, m.atom()), &s.c));
            }
        }
        cs.sort_by_key(|&(k, _)| k);
        let mut model: FxHashSet<Ex> = FxHashSet::default();
        let mut found: Vec<Ex> = Vec::new();
        for (k, c) in cs {
            let a = k.1;
            let holds = c.head().iter().any(|l| {
                l.kind() == X && (st.subs.contains(&l.atom()) || model.contains(&l.atom()))
            });
            if holds {
                continue;
            }
            model.insert(a);
            if a != x && matches!(ix.head(a).node, Node::Class(_)) && !tested.contains(&(x, a)) {
                found.push(a);
            }
        }
        found.sort_unstable();
        out.extend(found.into_iter().map(|b| (x, b)));
    }
    out
}

pub(super) fn ds(st: &mut State) -> &mut DlSt {
    st.extra().dl.get_or_insert_with(Box::default)
}

pub(super) fn dl(st: &State) -> Option<&DlSt> {
    st.extra.as_ref().and_then(|x| x.dl.as_deref())
}

#[inline]
pub(super) fn dix(ix: &Index) -> &DlIx {
    ix.dl.as_deref().expect("a run with Tier D")
}

/// The role of an existential restriction.
#[inline]
fn role_of(ix: &Index, e: Ex) -> Option<Role> {
    match ix.head(e).node {
        Node::Exist(r, _) => Some(r),
        _ => None,
    }
}

/// A premise's side: its body, and its head without the literal resolved on.
pub(super) type Side = (Vec<Lit>, Vec<Lit>);

pub(super) fn side(c: &Clause, without: Lit) -> Side {
    (
        c.body().to_vec(),
        c.head().iter().copied().filter(|&l| l != without).collect(),
    )
}

/// The premise of a unit.
#[inline]
pub(super) fn unit() -> Side {
    (Vec::new(), Vec::new())
}

/// The premises whose maximal literal is `X(a)`: the unit, or the stored clauses.
fn partners_x(st: &State, a: Ex) -> Vec<Side> {
    if a == TOP || st.subs.contains(&a) {
        return vec![unit()];
    }
    let Some(d) = dl(st) else {
        return Vec::new();
    };
    d.store
        .with_x(a)
        .map(|i| side(&d.store.clauses[i as usize].c, Lit::x(a)))
        .collect()
}

/// Whether the literal over `f_e(x)` holds: e is a unit, and the literal is a role above
/// e's or an atom of e's successor's core.
#[inline]
fn holds_succ(ix: &Index, dx: &DlIx, st: &State, l: Lit) -> bool {
    let e = l.fun();
    if !st.subs.contains(&e) {
        return false;
    }
    match l.kind() {
        RL => role_of(ix, e).is_some_and(|r| dx.sub(ix, r, l.atom())),
        F => dx.in_core(e, l.atom()),
        _ => false,
    }
}

/// The premises whose maximal literal is `l`, over `f(x)`: the unit when it holds, or the
/// stored clauses.
fn partners_succ(ix: &Index, dx: &DlIx, st: &State, l: Lit) -> Vec<Side> {
    if holds_succ(ix, dx, st, l) {
        return vec![unit()];
    }
    let Some(d) = dl(st) else {
        return Vec::new();
    };
    d.store
        .with_succ(l)
        .map(|i| side(&d.store.clauses[i as usize].c, l))
        .collect()
}

/// The premises of `t(x, f(x))`: the unit when f is a unit whose role is below t, or the
/// stored clauses whose maximal literal is a role below t over f.
fn partners_rl(ix: &Index, dx: &DlIx, st: &State, f: Ex, t: Role) -> Vec<Side> {
    if st.subs.contains(&f) && role_of(ix, f).is_some_and(|r| dx.sub(ix, r, t)) {
        return vec![unit()];
    }
    let Some(d) = dl(st) else {
        return Vec::new();
    };
    d.store
        .over(f)
        .filter(|&(_, l)| l.kind() == RL && dx.sub(ix, l.atom(), t))
        .map(|(i, l)| side(&d.store.clauses[i as usize].c, l))
        .collect()
}

/// The successor terms with a role below `t`, each with its premises: the unit existential
/// restrictions, and the stored clauses whose maximal literal is a role below t.
fn partners_rf(ix: &Index, st: &State, t: Role) -> Vec<(Ex, Vec<Side>)> {
    let dx = dix(ix);
    let mut out: Vec<(Ex, Vec<Side>)> = Vec::new();
    if t < dx.base && !dx.reaches(t) && !dx.counting || !dx.carries(t) {
        return out;
    }
    let mut units: Vec<Ex> = Vec::new();
    if t < dx.base {
        let below = dx.below.get(t as usize).map_or(&[][..], |v| v.as_slice());
        if below.len() <= st.subs.len() {
            units.extend(below.iter().copied().filter(|e| st.subs.contains(e)));
        } else {
            for &e in &st.subs {
                if let Some(r) = role_of(ix, e)
                    && ix.roles.sub(r, t)
                {
                    units.push(e);
                }
            }
            units.sort_unstable();
        }
    }
    out.extend(units.iter().map(|&e| (e, vec![unit()])));
    if let Some(d) = dl(st) {
        let mut ids: Vec<u32> = Vec::new();
        for (&s, v) in &d.store.by_rl {
            if dx.sub(ix, s, t) {
                ids.extend(
                    v.iter()
                        .copied()
                        .filter(|&i| !d.store.clauses[i as usize].dead),
                );
            }
        }
        // by successor term, then in the order the clauses came
        let mut by: Vec<(Ex, u32, Lit)> = ids
            .into_iter()
            .filter_map(|i| {
                let l = d.store.max_of(i, |l| l.kind() == RL)?;
                (units.binary_search(&l.fun()).is_err()).then_some((l.fun(), i, l))
            })
            .collect();
        by.sort_unstable();
        for (f, i, l) in by {
            let p = side(&d.store.clauses[i as usize].c, l);
            match out.last_mut() {
                Some((g, ps)) if *g == f => ps.push(p),
                _ => out.push((f, vec![p])),
            }
        }
        out.sort_by_key(|&(f, _)| f);
    }
    out
}

/// What the rules on one clause or unit give: single conclusions, and choices of one
/// premise from every list of a product, which [`drain`] derives one at a time so that a
/// wide case analysis is never held whole.
#[derive(Default)]
pub(super) struct Found {
    one: Vec<Side>,
    prods: Vec<(Vec<Vec<Side>>, Side)>,
    /// set when an inference has more choices than a context enumerates, which passes the
    /// run's bound on derived conclusions instead
    over: bool,
}

impl Found {
    pub(super) fn push(&mut self, s: Side) {
        self.one.push(s);
    }
    /// Each choice of one premise from every list, merged with `base`.
    pub(super) fn products(&mut self, mut lists: Vec<Vec<Side>>, base: Side) {
        if lists.iter().any(|l| l.is_empty()) {
            return;
        }
        for l in &mut lists {
            prune(l);
        }
        self.prods.push((lists, base));
    }
}

/// Drops from a product's list each premise whose side has every literal of another's: a
/// choice of it gives a conclusion that the other's choice subsumes.
fn prune(l: &mut Vec<Side>) {
    if l.len() < 2 {
        return;
    }
    let within = |a: &Side, b: &Side| {
        a.0.iter().all(|x| b.0.contains(x)) && a.1.iter().all(|x| b.1.contains(x))
    };
    l.sort_by_key(|s| s.0.len() + s.1.len());
    let mut keep: Vec<Side> = Vec::with_capacity(l.len());
    for s in l.drain(..) {
        if !keep.iter().any(|k| within(k, &s)) {
            keep.push(s);
        }
    }
    *l = keep;
}

/// The choices of a product between two checks of the run's bounds.
const PRODUCT_POLL: usize = 256;

/// Derives what the rules found: the single conclusions, then each product's choices in
/// the order of their lists, the last list fastest. The run's bounds are checked after
/// every [`PRODUCT_POLL`] choices the context derives, counted across calls, and a stop of
/// the run, found there or made by another worker, leaves the rest of the inference undone
/// and marks the context's store interrupted, a store made for the mark if the context has
/// none, so that the dependency record of a partial answer counts the context as open.
pub(super) fn drain<S: Sink>(ix: &Index, st: &mut State, found: Found, out: &mut S) {
    drain_with(ix, st, found, out, derive);
}

/// [`drain`], with `derive` adding each conclusion, which the root of a rooted run gives
/// its own.
#[inline(always)]
pub(super) fn drain_with<S: Sink>(
    ix: &Index,
    st: &mut State,
    found: Found,
    out: &mut S,
    derive: impl Fn(&Index, &mut State, Side, &mut S),
) {
    if found.over {
        out.dl().exhaust();
        ds(st).store.interrupted = true;
        return;
    }
    for c in found.one {
        derive(ix, st, c, out);
        let x = ds(st);
        x.choices += 1;
        if x.choices as usize >= PRODUCT_POLL {
            x.choices = 0;
            if !out.go_on() {
                ds(st).store.interrupted = true;
                return;
            }
        }
    }
    for (lists, base) in found.prods {
        let mut at = vec![0usize; lists.len()];
        'choices: loop {
            if out.stopped() {
                ds(st).store.interrupted = true;
                return;
            }
            let mut s = base.clone();
            for (l, &i) in lists.iter().zip(&at) {
                s.0.extend_from_slice(&l[i].0);
                s.1.extend_from_slice(&l[i].1);
            }
            derive(ix, st, s, out);
            let x = ds(st);
            x.choices += 1;
            if x.choices as usize >= PRODUCT_POLL {
                x.choices = 0;
                if !out.go_on() {
                    // the stop can come after the last choice, and the mark then only
                    // keeps the context open
                    ds(st).store.interrupted = true;
                    return;
                }
            }
            let mut k = lists.len();
            loop {
                if k == 0 {
                    break 'choices;
                }
                k -= 1;
                at[k] += 1;
                if at[k] < lists[k].len() {
                    break;
                }
                at[k] = 0;
            }
        }
    }
}

/// Two premises merged.
pub(super) fn merge(a: &Side, b: &Side) -> Side {
    let mut s = a.clone();
    s.0.extend_from_slice(&b.0);
    s.1.extend_from_slice(&b.1);
    s
}

/// A premise with one more head literal.
pub(super) fn with(a: &Side, l: Lit) -> Side {
    let mut s = a.clone();
    s.1.push(l);
    s
}

/// The universal restrictions that a holder `d` stands for, as `(t, c)` for `d ⊑ ∀t.c`:
/// from a universal node, and from the negative existential restrictions `∃s.d ⊑ c`,
/// which are `d ⊑ ∀s⁻.c`.
pub(super) fn alls(ix: &Index, d: Ex, out: &mut Vec<(Role, Ex)>) {
    out.clear();
    let h = ix.head(d);
    if let Node::Forall(t, c) = h.node {
        out.push((t, c));
    }
    for &(s, x) in ix.exist_of(h) {
        if let Some(i) = ix.roles.inv(s) {
            out.push((i, x));
        }
    }
}

/// Whether a head holds both `s ≈ t` and `s ≉ t`.
fn equality_tautology(head: &[Lit]) -> bool {
    head.iter().any(|l| {
        l.kind() == EQ
            && head
                .iter()
                .any(|m| m.kind() == NE && m.fun() == l.fun() && m.atom() == l.atom())
    })
}

/// Adds a clause the rules derived in this context: a unit over x goes to the EL rules, an
/// empty clause makes the context unsatisfiable, and any other clause goes to the store
/// unless it is redundant.
fn derive<S: Sink>(ix: &Index, st: &mut State, (mut body, mut head): Side, out: &mut S) {
    if st.bottom {
        return;
    }
    out.dl().work.fetch_add(1, Ordering::Relaxed);
    let dx = dix(ix);
    // a test context's denied class leaves the head
    if let Some(x) = dl(st)
        && !x.neg.is_empty()
    {
        head.retain(|&l| !(l.kind() == X && x.neg.binary_search(&l.atom()).is_ok()));
    }
    // atoms that hold in the context leave the body
    body.retain(|l| !(l.kind() == X && (l.atom() == TOP || st.subs.contains(&l.atom()))));
    // a literal that holds makes the clause redundant, and one that cannot leaves it
    let mut holds = false;
    head.retain(|&l| match l.kind() {
        X | Y | F if l.atom() == BOTTOM => false,
        X | F if l.atom() == TOP => {
            holds = true;
            true
        }
        X if st.subs.contains(&l.atom()) => {
            holds = true;
            true
        }
        F | RL if holds_succ(ix, dx, st, l) => {
            holds = true;
            true
        }
        _ => true,
    });
    if holds {
        return;
    }
    // a rooted run's interned atoms of owl:Thing and owl:Nothing, over constants
    if dx.root.is_some() {
        use super::dl_clause::XLit;
        let lits = &out.dl().lits;
        let inv = |r: Role| dx.inv(ix, r);
        let is = |l: Lit, a: Ex| {
            l.kind() == XL && matches!(lits.view(l, inv), XLit::Atom(b, _) if b == a)
        };
        body.retain(|&l| !is(l, TOP));
        if head.iter().any(|&l| is(l, TOP)) {
            return;
        }
        head.retain(|&l| !is(l, BOTTOM));
    }
    body.sort_unstable();
    body.dedup();
    head.sort_unstable();
    head.dedup();
    // Γ ∧ A → Δ ∨ A is a consequence of the marker of A
    if head
        .iter()
        .any(|l| matches!(l.kind(), X | RY) && body.binary_search(l).is_ok())
    {
        return;
    }
    if dx.counting && equality_tautology(&head) {
        return;
    }
    // f(x) and g(x) differ under the body when they are in disjoint classes under it, which
    // the clause Γ → f(x) ≉ g(x) states: an f(x) ≈ g(x) leaves the head (negative
    // simplify-reflect), and an f(x) ≉ g(x) makes the clause one that this clause subsumes
    if dx.counting && head.iter().any(|l| matches!(l.kind(), EQ | NE)) {
        let mut a = Vec::new();
        let mut b = Vec::new();
        let mut differ = None;
        // each removal is checked against the head as it stands, without the literal
        let mut k = 0;
        while k < head.len() {
            let l = head[k];
            let Term::Fun(g) = l.term() else {
                k += 1;
                continue;
            };
            if !matches!(l.kind(), EQ | NE) {
                k += 1;
                continue;
            }
            let rest: Vec<Lit> = head.iter().copied().filter(|&m| m != l).collect();
            let read = known(dx, st, &body, &rest, l.fun(), &mut a)
                + known(dx, st, &body, &rest, g, &mut b);
            out.dl()
                .work
                .fetch_add(read / WORK_READS, Ordering::Relaxed);
            let Some((i, j)) = disjoint(ix, &a, &b) else {
                k += 1;
                continue;
            };
            if l.kind() == NE {
                // Γ → Δ₀ ∨ f(x) ≉ g(x), Δ₀ the certificates' other head literals
                let mut h = vec![l];
                if let Some(d) = dl(st) {
                    for c in [i, j] {
                        if let Some(s) = d.store.clauses.get(c as usize) {
                            h.extend(
                                s.c.head()
                                    .iter()
                                    .copied()
                                    .filter(|m| rest.binary_search(m).is_ok()),
                            );
                        }
                    }
                }
                differ = Some(h);
                break;
            }
            head.remove(k);
        }
        if let Some(h) = differ {
            let c = Clause::new(body, h);
            if let Some(bytes) = ds(st).store.add(c) {
                out.dl().grow(bytes);
            }
            return;
        }
    }
    if body.is_empty() {
        match head.as_slice() {
            [] => {
                out.me(Concl::Sub(BOTTOM));
                return;
            }
            [l] if l.kind() == X => {
                out.me(Concl::Sub(l.atom()));
                return;
            }
            _ => {}
        }
    }
    if let Some(bytes) = ds(st).store.add(Clause::new(body, head)) {
        out.dl().grow(bytes);
    }
}

/// The classes in a disjointness set that hold of the successor term f(x) when `body`
/// holds and no literal of `head` does: its core when it is a unit, and the atom over f(x)
/// of each certificate in the store whose body is in `body` and whose other head literals
/// are in `head`. Each class comes with the clause that gives it, `u32::MAX` for the core.
/// The answer is the number of clauses it read, which the work bound counts.
fn known(
    dx: &DlIx,
    st: &State,
    body: &[Lit],
    head: &[Lit],
    f: Ex,
    out: &mut Vec<(Ex, u32)>,
) -> usize {
    out.clear();
    let mut read = 0;
    if st.subs.contains(&f)
        && let Some(core) = dx.cores.get(&f)
    {
        out.extend(core.iter().map(|&a| (a, u32::MAX)));
    }
    if let Some(d) = dl(st) {
        let certs = d.store.by_cert.get(&f).map_or(&[][..], |v| v.as_slice());
        read = certs.len();
        for &i in certs {
            let s = &d.store.clauses[i as usize];
            if s.dead || !s.c.body().iter().all(|m| body.binary_search(m).is_ok()) {
                continue;
            }
            let mut cert = None;
            let mut fits = true;
            for &m in s.c.head() {
                if head.binary_search(&m).is_ok() {
                    continue;
                }
                if cert.is_none() && m.kind() == F && m.fun() == f {
                    cert = Some(m);
                } else {
                    fits = false;
                    break;
                }
            }
            if let (true, Some(m)) = (fits, cert) {
                out.push((m.atom(), i));
            }
        }
    }
    read
}

/// A new clause `id` that certifies terms are in classes of disjointness sets: the stored
/// clauses with an equality or inequality of those terms are derived again, which
/// simplifies them whatever order they came in, and the shorter clause subsumes the old.
fn again(ix: &Index, st: &State, id: u32, c: &Clause, found: &mut Found) {
    let Some(d) = dl(st) else { return };
    let mut keys: Vec<Ex> = c
        .head()
        .iter()
        .filter(|m| m.kind() == F && !ix.disjoint_of(ix.head(m.atom())).is_empty())
        .map(|m| m.fun())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    for k in keys {
        for &j in d.store.by_eq.get(&k).map_or(&[][..], |v| v.as_slice()) {
            let s = &d.store.clauses[j as usize];
            if !s.dead && j != id {
                found.one.push((s.c.body().to_vec(), s.c.head().to_vec()));
            }
        }
    }
}

/// The clauses read that count as one derived conclusion for the work bound.
const WORK_READS: usize = 16;

/// A class of `a` and one of `b` in a disjointness set, with the clauses that give them.
fn disjoint(ix: &Index, a: &[(Ex, u32)], b: &[(Ex, u32)]) -> Option<(u32, u32)> {
    a.iter().find_map(|&(x, i)| {
        let sx = ix.disjoint_of(ix.head(x));
        if sx.is_empty() {
            return None;
        }
        b.iter().find_map(|&(y, j)| {
            (y != x && ix.disjoint_of(ix.head(y)).iter().any(|s| sx.contains(s))).then_some((i, j))
        })
    })
}

/// Hyper with a clause of case analysis, `id`, with `prem` as the premise of its body atom
/// `d`: the other atoms' premises, and the head as literals over x.
fn hyper_clause(ix: &Index, st: &State, id: u32, d: Ex, prem: &Side, found: &mut Found) {
    let (body, head) = &dix(ix).clauses[id as usize];
    let lists: Vec<Vec<Side>> = body
        .iter()
        .map(|&a| {
            if a == d {
                vec![prem.clone()]
            } else {
                partners_x(st, a)
            }
        })
        .collect();
    let base: Side = (Vec::new(), head.iter().map(|&h| Lit::x(h)).collect());
    found.products(lists, base);
}

/// The universals of the holders over the successor term f, which x reaches by r: the
/// units' through the propagations, and the stored holders', each with `prem`, the premise
/// of `r(x, f(x))`.
fn univ_over(ix: &Index, st: &State, f: Ex, r: Role, prem: &Side, found: &mut Found) {
    let roles = &ix.roles;
    let dx = dix(ix);
    if r >= dx.base {
        return;
    }
    for &(s, x) in &st.props {
        if let Some(t) = roles.inv(s)
            && (dx.counting || dx.reaches(t))
            && dx.carries(t)
            && roles.sub(r, t)
        {
            found.push(with(prem, Lit::f(f, x)));
        }
    }
    let Some(d) = dl(st) else { return };
    let mut holders: Vec<Ex> = d.store.by_x.keys().copied().collect();
    holders.sort_unstable();
    let mut ps = Vec::new();
    for b in holders {
        if st.subs.contains(&b) {
            continue;
        }
        alls(ix, b, &mut ps);
        for &(t, c) in &ps {
            if roles.sub(r, t) {
                for p in partners_x(st, b) {
                    found.push(with(&merge(prem, &p), Lit::f(f, c)));
                }
            }
        }
    }
}

/// The at-most restriction `≤n q` of a holder: each choice of n+1 different neighbour
/// terms, among the predecessor and the successor terms that x reaches by a role below q,
/// with a premise for each and for the holder, gives the equalities of every pair (DL4).
/// The predecessor is a neighbour through the body atom `q(x, y)`. `fixed` is a new premise
/// of one successor term, which the inference must use, and `holder` the holder's premises.
fn at_most(
    ix: &Index,
    st: &State,
    n: u32,
    q: Role,
    holder: Vec<Side>,
    fixed: Option<(Ex, &Side)>,
    found: &mut Found,
) {
    let dx = dix(ix);
    if holder.is_empty() {
        return;
    }
    let mut cands: Vec<(Term, Vec<Side>)> = Vec::new();
    if let Some(i) = dx.inv(ix, q)
        && dx.carries(i)
    {
        cands.push((Term::Y, vec![(vec![Lit::ry(i)], Vec::new())]));
    }
    let mut must = None;
    for (f, ps) in partners_rf(ix, st, q) {
        match fixed {
            Some((g, p)) if g == f => {
                must = Some(cands.len());
                cands.push((Term::Fun(f), vec![p.clone()]));
            }
            _ => cands.push((Term::Fun(f), ps)),
        }
    }
    if let Some((g, p)) = fixed
        && must.is_none()
    {
        must = Some(cands.len());
        cands.push((Term::Fun(g), vec![p.clone()]));
    }
    let k = n as usize + 1;
    if cands.len() < k {
        return;
    }
    // the subsets, with the fixed candidate when there is one, made one product each
    let subsets = match must {
        Some(_) => choose(cands.len() - 1, k - 1),
        None => choose(cands.len(), k),
    };
    // each subset's product holds its premise lists and the equalities of every pair
    let payload = subsets.saturating_mul(k * (k - 1) / 2 + k + 1);
    if k > AT_MOST_WIDEST || subsets > AT_MOST_SUBSETS || payload > AT_MOST_PAYLOAD {
        found.over = true;
        return;
    }
    // each subset of k candidates, with the fixed one when there is one
    let mut pick: Vec<usize> = (0..k).collect();
    loop {
        if must.is_none_or(|m| pick.contains(&m)) {
            let mut lists: Vec<Vec<Side>> = vec![holder.clone()];
            let mut eqs: Vec<Lit> = Vec::new();
            for (a, &i) in pick.iter().enumerate() {
                lists.push(cands[i].1.clone());
                for &j in &pick[a + 1..] {
                    if let Made::Lit(l) = Lit::eq(cands[i].0, cands[j].0) {
                        eqs.push(l);
                    }
                }
            }
            found.products(lists, (Vec::new(), eqs));
        }
        // the next subset in lexicographic order
        let m = cands.len();
        let mut i = k;
        loop {
            if i == 0 {
                return;
            }
            i -= 1;
            if pick[i] < m - k + i {
                pick[i] += 1;
                for j in i + 1..k {
                    pick[j] = pick[j - 1] + 1;
                }
                break;
            }
        }
    }
}

/// The most subsets one application of the at-most rule makes before the run passes its
/// bound on derived conclusions instead.
const AT_MOST_SUBSETS: usize = 1 << 14;

/// The most literals and premise lists the products of one application of the at-most
/// rule hold before the run passes its bound on derived conclusions instead.
const AT_MOST_PAYLOAD: usize = 1 << 16;

/// The most terms one application of the at-most rule equates: past it each conclusion has
/// more than 66 equalities, whose Eq and Factor inferences no context finishes, and the
/// run passes its bound on derived conclusions instead.
const AT_MOST_WIDEST: usize = 12;

/// The number of k-element subsets of n, saturating.
fn choose(n: usize, k: usize) -> usize {
    if k > n {
        return 0;
    }
    let k = k.min(n - k);
    let mut c: usize = 1;
    for i in 0..k {
        c = match c.checked_mul(n - i) {
            Some(x) => x / (i + 1),
            None => return usize::MAX,
        };
    }
    c
}

/// The rules of a new premise `prem` of `s(x, f(x))`: the universals over f, the at-most
/// restrictions that count f, the qualified roles' clauses `S(z, x) ∧ B(x) → S_B(z, x)`
/// with z ↦ f(x), and the disjoint roles.
fn on_rl(ix: &Index, st: &State, f: Ex, s: Role, prem: &Side, found: &mut Found) {
    let dx = dix(ix);
    univ_over(ix, st, f, s, prem, found);
    if !dx.counting {
        return;
    }
    for &(m, n, q) in &dx.atmosts {
        if dx.sub(ix, s, q) {
            at_most(ix, st, n, q, partners_x(st, m), Some((f, prem)), found);
        }
    }
    for (k, &(r, b)) in dx.quals.iter().enumerate() {
        let q = dx.base + 2 * k as Role;
        if let Some(i) = dx.inv(ix, r)
            && dx.sub(ix, s, i)
        {
            for p in partners_x(st, b) {
                found.push(with(&merge(prem, &p), Lit::rl(f, q ^ 1)));
            }
        }
    }
    for &(a, b) in &dx.rdis {
        if dx.sub(ix, s, a) {
            for p in partners_rl(ix, dx, st, f, b) {
                found.push(merge(prem, &p));
            }
        }
    }
}

/// The rules of a new premise `prem` of `b(x)` for the qualified roles `S_B` with B = b:
/// `S(z, x) ∧ b(x) → S_B(z, x)` with z ↦ y, which the body atom `S(y, x)` carries, and
/// with z ↦ f(x) for each premise of `S(f(x), x)`.
fn on_filler(ix: &Index, st: &State, b: Ex, prem: &Side, found: &mut Found) {
    let dx = dix(ix);
    let Some(qs) = dx.quals_of.get(&b) else {
        return;
    };
    for &q in qs {
        let r = dx.quals[((q - dx.base) / 2) as usize].0;
        if dx.carries(r) {
            let mut p = prem.clone();
            p.0.push(Lit::ry(r));
            p.1.push(Lit::ry(q));
            found.push(p);
        }
        if let Some(i) = dx.inv(ix, r) {
            for (f, ps) in partners_rf(ix, st, i) {
                for p in ps {
                    found.push(with(&merge(prem, &p), Lit::rl(f, q ^ 1)));
                }
            }
        }
    }
}

/// `l` with its successor term f(x) replaced by t, the Eq rule's conclusion literal.
fn rewrite(ix: &Index, dx: &DlIx, l: Lit, t: Term) -> Option<Made> {
    Some(match (l.kind(), t) {
        (F, Term::Fun(g)) => Made::Lit(Lit::f(g, l.atom())),
        (F, Term::Y) => Made::Lit(Lit::y(l.atom())),
        (RL, Term::Fun(g)) => Made::Lit(Lit::rl(g, l.atom())),
        // s(x, y) is s⁻(y, x)
        (RL, Term::Y) => Made::Lit(Lit::ry(dx.inv(ix, l.atom())?)),
        (EQ, t) => Lit::eq(t, l.term()),
        (NE, t) => Lit::ne(t, l.term()),
        _ => return None,
    })
}

/// The Eq rule of a premise `c1` with maximal `f(x) ≈ t` into a premise `c2` with the
/// maximal literal `l2` over f(x).
fn paramodulate(ix: &Index, dx: &DlIx, c1: &Side, t: Term, c2: &Side, l2: Lit, found: &mut Found) {
    match rewrite(ix, dx, l2, t) {
        Some(Made::Lit(l)) => found.push(with(&merge(c1, c2), l)),
        Some(Made::False) => found.push(merge(c1, c2)),
        Some(Made::True) | None => {}
    }
}

/// The literals over f(x) that a unit existential restriction f holds, its role and its
/// successor's core, as premises in place.
fn unit_lits(ix: &Index, dx: &DlIx, st: &State, f: Ex, out: &mut Vec<Lit>) {
    out.clear();
    if !st.subs.contains(&f) {
        return;
    }
    if let Some(r) = role_of(ix, f) {
        out.push(Lit::rl(f, r));
    }
    if let Some(core) = dx.cores.get(&f) {
        out.extend(core.iter().filter(|&&a| a != TOP).map(|&a| Lit::f(f, a)));
    }
}

/// The Eq and Factor rules of a new premise `prem` with maximal `f(x) ≈ t`: it rewrites
/// every premise whose maximal literal is over f(x), the units in place among them, and
/// factors the other equalities of f(x) in its own head. `me` is its own id, which it
/// does not rewrite.
fn on_eq(ix: &Index, st: &State, me: Option<u32>, l: Lit, prem: &Side, found: &mut Found) {
    let dx = dix(ix);
    let f = l.fun();
    let t = l.term();
    let mut us = Vec::new();
    unit_lits(ix, dx, st, f, &mut us);
    for &u in &us {
        paramodulate(ix, dx, prem, t, &unit(), u, found);
    }
    if let Some(d) = dl(st) {
        for (i, l2) in d.store.over(f) {
            if Some(i) == me {
                continue;
            }
            let c2 = side(&d.store.clauses[i as usize].c, l2);
            paramodulate(ix, dx, prem, t, &c2, l2, found);
        }
    }
    // Factor: Γ → Δ ∨ f ≈ u ∨ f ≈ t gives Γ → Δ ∨ u ≉ t ∨ f ≈ t
    for &o in &prem.1 {
        if o.kind() == EQ && o.fun() == f {
            let mut s: Side = (
                prem.0.clone(),
                prem.1.iter().copied().filter(|&m| m != o).collect(),
            );
            s.1.push(l);
            match Lit::ne(o.term(), t) {
                Made::Lit(n) => s.1.push(n),
                Made::False => {}
                Made::True => continue,
            }
            found.push(s);
        }
    }
}

/// The Eq rule of a new premise `prem` with the maximal literal `l2` over f(x), not an
/// equality: each premise with a maximal equality of f(x) rewrites it.
fn into_eq(ix: &Index, st: &State, me: Option<u32>, l2: Lit, prem: &Side, found: &mut Found) {
    let dx = dix(ix);
    let Some(d) = dl(st) else { return };
    for (i, l) in d.store.over(l2.fun()) {
        if l.kind() != EQ || Some(i) == me {
            continue;
        }
        let c1 = side(&d.store.clauses[i as usize].c, l);
        paramodulate(ix, dx, &c1, l.term(), prem, l2, found);
    }
}

/// A new unit `→ d(x)`, after the EL rules: Hyper with the clauses of case analysis and
/// with the stored clauses, the universals over the successors and the predecessor, the
/// universals over a new successor, the propagations to the registered predecessors, and
/// the counting rules of a holder, a filler or a new successor.
pub fn sub<S: Sink>(ix: &Index, me: Ex, st: &mut State, d: Ex, out: &mut S) {
    if dl(st).is_some_and(|x| x.neg.binary_search(&d).is_ok()) {
        out.me(Concl::Sub(BOTTOM));
        return;
    }
    let dx = dix(ix);
    let roles = &ix.roles;
    let mut found = Found::default();
    let unit = unit();
    if let Some(ids) = dx.by_body.get(&d) {
        for &id in ids {
            hyper_clause(ix, st, id, d, &unit, &mut found);
        }
    }
    let h = ix.head(d);
    if dl(st).is_some() {
        // the stored clauses meet d in conjunctions and disjointness
        for &(p, k) in ix.conj_of(h) {
            if !st.subs.contains(&p) {
                for p in partners_x(st, p) {
                    found.push(with(&p, Lit::x(k)));
                }
            }
        }
        for &set in ix.disjoint_of(h) {
            for &m in &dx.sets[set as usize] {
                if m != d && !st.subs.contains(&m) {
                    found.one.extend(partners_x(st, m));
                }
            }
        }
        // the stored clauses that d, or the successor by d, satisfies are redundant
        let x = ds(st);
        let mut dead = x.store.with_head(Lit::x(d));
        if let Node::Exist(r, _) = h.node {
            dead.extend(x.store.with_head(Lit::rl(d, r)));
            for &s in roles.sup.get(r as usize).map_or(&[][..], |v| v.as_slice()) {
                if s != r {
                    dead.extend(x.store.with_head(Lit::rl(d, s)));
                }
            }
            if let Some(core) = dx.cores.get(&d) {
                for &a in core.iter() {
                    dead.extend(x.store.with_head(Lit::f(d, a)));
                }
            }
        }
        for i in dead {
            x.store.clauses[i as usize].dead = true;
        }
    }
    // a universal: the propagation over the backward links, as for a negative
    // existential restriction
    if let Node::Forall(t, c) = h.node
        && let Some(i) = roles.inv(t)
    {
        st.props.push((i, c));
        for (r, srcs) in &st.back {
            if roles.sub(*r, i) {
                for &e in &srcs.list {
                    out.send(e, Concl::Sub(c));
                }
            }
        }
    }
    let mut ps: Vec<(Role, Ex)> = Vec::new();
    alls(ix, d, &mut ps);
    for &(t, c) in &ps {
        // over the successors
        for (e, prems) in partners_rf(ix, st, t) {
            for p in prems {
                found.push(with(&p, Lit::f(e, c)));
            }
        }
        // over the registered predecessors, and with counting the backward links whose
        // role is not below t⁻, whose sources can hold other roles over this context
        if let Some(i) = roles.inv(t) {
            props_to_dpreds(ix, me, st, i, c, out);
        }
    }
    if dx.counting {
        if let Some(&(n, q)) = dx.atmost.get(&d) {
            at_most(ix, st, n, q, vec![unit.clone()], None, &mut found);
        }
        on_filler(ix, st, d, &unit, &mut found);
    }
    // an existential restriction: its role and core over the new successor term are
    // premises in place
    if let Node::Exist(r, _) = h.node {
        on_rl(ix, st, d, r, &unit, &mut found);
        if dx.counting && dx.root.is_none() {
            let mut us = Vec::new();
            unit_lits(ix, dx, st, d, &mut us);
            for &l in &us {
                into_eq(ix, st, None, l, &unit, &mut found);
            }
        }
    }
    if dx.root.is_some() {
        rooted_unit(ix, st, d, &mut found, &out.dl().lits);
        // the EL successor of a unit existential restriction takes the ground markers
        if let Node::Exist(..) = h.node {
            super::dl_root::new_succ(st, h.succ, out);
        }
    }
    drain(ix, st, found, out);
}

/// The rules of a rooted run on a new premise `prem` whose maximal literal is `l` in a
/// context other than the root: `N_u(x) → x ≈ u`, the differing successor terms of a
/// minimum's holder, and the equality rules.
fn rooted_max(ix: &Index, st: &State, l: Lit, prem: &Side, found: &mut Found, lits: &Lits) {
    let dx = dix(ix);
    if l.kind() == X {
        rooted_hyper(ix, l.atom(), prem, found, lits);
    }
    let q = super::dl_eq::Eqs {
        ix,
        dx,
        lits,
        root: false,
    };
    q.on_max(st, l, prem, found);
}

/// The rules of a rooted run on a new unit `d(x)`, as [`rooted_max`] has them, with the
/// unit's literals in place: the atom, and an existential restriction's role and core.
fn rooted_unit(ix: &Index, st: &State, d: Ex, found: &mut Found, lits: &Lits) {
    use super::dl_clause::{RTerm, XLit};
    let dx = dix(ix);
    rooted_hyper(ix, d, &unit(), found, lits);
    let q = super::dl_eq::Eqs {
        ix,
        dx,
        lits,
        root: false,
    };
    q.on_unit(st, XLit::Atom(d, RTerm::X), found);
    if let Node::Exist(r, _) = ix.head(d).node {
        let f = RTerm::Fx(d);
        q.on_unit(
            st,
            XLit::Role(r, RTerm::X, f).canon(|s| dx.inv(ix, s)),
            found,
        );
        for &a in dx.core(d) {
            if a != TOP {
                q.on_unit(st, XLit::Atom(a, f), found);
            }
        }
    }
}

/// Hyper of a rooted run's own clauses on a premise of `d(x)`: `N_u(x) → x ≈ u`, and
/// `M(x) → f_i(x) ≉ f_j(x)` for a minimum's holder M.
fn rooted_hyper(ix: &Index, d: Ex, prem: &Side, found: &mut Found, lits: &Lits) {
    use super::dl_clause::{RTerm, XLit};
    let dx = dix(ix);
    let inv = |r: Role| dx.inv(ix, r);
    if let Node::Nominal(u) = ix.head(d).node
        && let Made::Lit(e) = lits.lit(XLit::Eq(RTerm::X, RTerm::C(u)), inv)
    {
        found.push(with(prem, e));
    }
    if let Some(es) = dx.ne_of.get(&d) {
        for (i, &a) in es.iter().enumerate() {
            for &b in &es[i + 1..] {
                if let Made::Lit(n) = Lit::ne(Term::Fun(a), Term::Fun(b)) {
                    found.push(with(prem, n));
                }
            }
        }
    }
}

/// The context became unsatisfiable: its registered predecessors learn it.
pub fn bottom<S: Sink>(me: Ex, st: &mut State, out: &mut S) {
    super::dl_root::bottom(st, out);
    let Some(x) = dl(st) else { return };
    if x.dpreds.is_empty() {
        return;
    }
    let id = out.dl().empty();
    for &(u, _) in &x.dpreds {
        out.send(u, Concl::PredC(me, id));
    }
}

/// The table id of the clause `s(y, x) → c(y)` of a propagation.
fn prop_id<S: Sink>(x: &mut DlSt, s: Role, c: Ex, out: &mut S) -> u32 {
    *x.props.entry((s, c)).or_insert_with(|| {
        out.dl()
            .table
            .push(Clause::new(vec![Lit::ry(s)], vec![Lit::y(c)]))
    })
}

/// Sends the registered predecessors whose role is below `s` the clause `s(y, x) → c(y)`
/// of a propagation. With counting, a predecessor can hold other roles over this context
/// than the one it reaches it by, so every registered predecessor and every source of a
/// backward link whose role is not below s takes it.
fn props_to_dpreds<S: Sink>(ix: &Index, me: Ex, st: &mut State, s: Role, c: Ex, out: &mut S) {
    let counting = dix(ix).counting;
    if counting {
        let mut srcs: Vec<Ex> = Vec::new();
        for (r, v) in &st.back {
            if !ix.roles.sub(*r, s) {
                srcs.extend(v.list.iter().copied());
            }
        }
        if !srcs.is_empty() {
            let id = prop_id(ds(st), s, c, out);
            for e in srcs {
                out.send(e, Concl::PredC(me, id));
            }
        }
    }
    let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) else {
        return;
    };
    if x.dpreds.is_empty() {
        return;
    }
    let id = prop_id(x, s, c, out);
    for &(u, e) in &x.dpreds {
        if counting || role_of(ix, e).is_some_and(|r| ix.roles.sub(r, s)) {
            out.send(u, Concl::PredC(me, id));
        }
    }
}

/// Whether a link by `r` meets the role atoms of a clause's body. With counting every
/// clause goes to every predecessor, which resolves the role atoms with its own literals.
fn meets(ix: &Index, c: &Clause, r: Role) -> bool {
    dix(ix).counting
        || c.body()
            .iter()
            .filter(|l| l.kind() == RY)
            .all(|l| ix.roles.sub(r, l.atom()))
}

/// Whether a literal of a rooted run is ground: its terms are constants.
fn is_ground(v: super::dl_clause::XLit) -> bool {
    v.is_ground()
}

/// Whether a clause of a rooted run goes to the predecessors: its body holds atoms over x,
/// role atoms `s(y, x)` and ground literals, and every head literal is a predecessor
/// trigger of the journal's Definition 4: an atom over y or a constant, a role atom between
/// x and y or between two constants, or an equality of x, y and constants. An inequality is
/// none. A role atom between x and a constant goes too, which Table 3 admits for the root
/// as predecessor and [`pred`] refuses for another.
pub(super) fn pred_ready(ix: &Index, dx: &DlIx, lits: &Lits, c: &Clause) -> bool {
    use super::dl_clause::XLit;
    let inv = |r: Role| dx.inv(ix, r);
    let body = c.body().iter().all(|&l| match l.kind() {
        X | RY => true,
        XL => is_ground(lits.view(l, inv)),
        _ => false,
    });
    body && c.head().iter().all(|&l| match l.kind() {
        Y | RY => true,
        XL => match lits.view(l, inv) {
            XLit::Atom(_, t) => matches!(t, RTerm::C(_)),
            XLit::Role(_, a, b) => matches!(
                (a, b),
                (RTerm::C(_), RTerm::C(_)) | (RTerm::X, RTerm::C(_)) | (RTerm::C(_), RTerm::X)
            ),
            XLit::Eq(a, b) => [a, b]
                .iter()
                .all(|t| matches!(t, RTerm::X | RTerm::Y | RTerm::C(_))),
            XLit::Ne(..) => false,
        },
        _ => false,
    })
}

/// The table id of a stored clause over the predecessor, `None` once it is dead.
pub(super) fn pred_id<S: Sink>(x: &mut DlSt, i: u32, out: &mut S) -> Option<u32> {
    let s = &mut x.store.clauses[i as usize];
    if s.dead {
        return None;
    }
    if s.sent.is_none() {
        s.sent = Some(out.dl().table.push(s.c.clone()));
    }
    s.sent
}

/// A new backward link from `e` by `r`: the source takes the clauses over the predecessor
/// that the role meets, and with counting the propagations whose role is not above r.
pub fn back<S: Sink>(ix: &Index, me: Ex, st: &mut State, e: Ex, r: Role, out: &mut S) {
    if dix(ix).counting {
        let props: Vec<(Role, Ex)> = st
            .props
            .iter()
            .copied()
            .filter(|&(s, _)| !ix.roles.sub(r, s))
            .collect();
        if !props.is_empty() {
            let x = ds(st);
            for (s, c) in props {
                let id = prop_id(x, s, c, out);
                out.send(e, Concl::PredC(me, id));
            }
        }
    }
    let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) else {
        return;
    };
    for k in 0..x.store.preds.len() {
        let i = x.store.preds[k];
        if let Some(cid) = pred_id(x, i, out)
            && meets(ix, &x.store.clauses[i as usize].c, r)
        {
            out.send(e, Concl::PredC(me, cid));
        }
    }
}

/// A marker `a → a`: the atom may hold of the context's instance.
pub fn hyp<S: Sink>(st: &mut State, a: Ex, out: &mut S) -> bool {
    if st.bottom || a == TOP || st.subs.contains(&a) {
        return false;
    }
    let x = ds(st);
    if !x.hyps.insert(a) {
        return false;
    }
    if let Some(bytes) = x.store.add(Clause::new(vec![Lit::x(a)], vec![Lit::x(a)])) {
        out.dl().grow(bytes + 4);
    }
    true
}

/// A predecessor registers by its existential restriction e: it takes the clauses over the
/// predecessor that e's role meets, the propagations, and unsatisfiability.
pub fn dedge<S: Sink>(ix: &Index, me: Ex, st: &mut State, u: Ex, e: Ex, out: &mut S) -> bool {
    let bottom = st.bottom;
    let props: Vec<(Role, Ex)> = st.props.clone();
    let counting = dix(ix).counting;
    let x = ds(st);
    if x.dpreds.contains(&(u, e)) {
        return false;
    }
    x.dpreds.push((u, e));
    if bottom {
        let id = out.dl().empty();
        out.send(u, Concl::PredC(me, id));
        return true;
    }
    let Some(r) = role_of(ix, e) else { return true };
    for k in 0..x.store.preds.len() {
        let i = x.store.preds[k];
        if let Some(cid) = pred_id(x, i, out)
            && meets(ix, &x.store.clauses[i as usize].c, r)
        {
            out.send(u, Concl::PredC(me, cid));
        }
    }
    for (s, c) in props {
        if counting || ix.roles.sub(r, s) {
            let id = prop_id(x, s, c, out);
            out.send(u, Concl::PredC(me, id));
        }
    }
    true
}

/// The existential restrictions of this context whose successor is `v`: those it holds as
/// units whose EL successor is v, and the one whose hypothetical successor v is, once the
/// context registered with it.
fn reaching(dx: &DlIx, st: &State, v: Ex, out: &mut Vec<Ex>) {
    out.clear();
    if let Some(x) = dl(st) {
        out.extend(x.edges.iter().filter(|&&(_, w)| w == v).map(|&(e, _)| e));
    }
    if dx.of_hyp.contains_key(&v) {
        return;
    }
    if let Some(es) = dx.by_succ.get(&v) {
        out.extend(es.iter().copied().filter(|e| st.subs.contains(e)));
    }
    out.sort_unstable();
    out.dedup();
}

/// A clause over the predecessor from the successor `v`: Pred with each existential
/// restriction that reaches v.
pub fn pred_c<S: Sink>(ix: &Index, st: &mut State, v: Ex, cid: u32, out: &mut S) -> bool {
    if st.bottom {
        return false;
    }
    let x = ds(st);
    if !x.pred_seen.insert((v, cid)) {
        return false;
    }
    x.pred_in.entry(v).or_default().push(cid);
    let c = out.dl().table.get(cid).clone();
    let mut es = Vec::new();
    reaching(dix(ix), st, v, &mut es);
    let mut found = Found::default();
    for e in es {
        pred(ix, st, e, &c, None, &mut found, &out.dl().lits);
    }
    drain(ix, st, found, out);
    true
}

/// Pred over the successor term f with the clause `c` of f's successor: each body atom `A`
/// takes a premise whose maximal literal is `F(f, A)`, each role atom `s(y, x)` one whose
/// maximal literal is a role below s over f(x), and the head's literals over y become
/// literals over x and f(x). `fixed` is the premise that a new clause gives for one of
/// these literals, which the inference must use at one of the places it fits.
fn pred(
    ix: &Index,
    st: &State,
    f: Ex,
    c: &Clause,
    fixed: Option<(Lit, &Side)>,
    found: &mut Found,
    lits: &Lits,
) {
    let dx = dix(ix);
    // a rooted run's ground body literals stay in the body, and its head literals over y,
    // x and constants are read over x, f(x) and the constants
    let mut ground: Vec<Lit> = Vec::new();
    let mut up: Vec<Lit> = Vec::new();
    if dx.root.is_some() {
        use super::dl_clause::XLit;
        let inv = |r: Role| dx.inv(ix, r);
        for &l in c.body() {
            if l.kind() == XL {
                if !is_ground(lits.view(l, inv)) {
                    return;
                }
                ground.push(l);
            }
        }
        for &l in c.head() {
            if l.kind() != XL {
                continue;
            }
            let v = lits.view(l, inv);
            if is_ground(v) {
                up.push(l);
                continue;
            }
            // a role atom between x and a constant goes only to the root
            if let XLit::Role(_, a, b) = v
                && matches!((a, b), (RTerm::X, RTerm::C(_)) | (RTerm::C(_), RTerm::X))
            {
                return;
            }
            let t = |t: RTerm| match t {
                RTerm::Y => Some(RTerm::X),
                RTerm::X => Some(RTerm::Fx(f)),
                RTerm::C(_) => Some(t),
                _ => None,
            };
            let w = match v {
                XLit::Atom(a, b) => t(b).map(|b| XLit::Atom(a, b)),
                XLit::Role(r, a, b) => t(a).zip(t(b)).map(|(a, b)| XLit::Role(r, a, b)),
                XLit::Eq(a, b) => t(a).zip(t(b)).map(|(a, b)| XLit::Eq(a, b)),
                XLit::Ne(a, b) => t(a).zip(t(b)).map(|(a, b)| XLit::Ne(a, b)),
            };
            match w.map(|w| lits.lit(w, inv)) {
                Some(Made::Lit(m)) => up.push(m),
                Some(Made::False) => {}
                Some(Made::True) | None => return,
            }
        }
    }
    // the premises' literals: an atom over f(x), or a role atom read through the hierarchy;
    // without counting f's one role over f(x) is the only role literal, and one premise of
    // it meets every role atom below it
    let mut need: Vec<Lit> = c
        .body()
        .iter()
        .filter(|l| l.kind() == X)
        .map(|l| Lit::f(f, l.atom()))
        .collect();
    if dx.counting {
        need.extend(
            c.body()
                .iter()
                .filter(|l| l.kind() == RY)
                .map(|l| Lit::rl(f, l.atom())),
        );
    } else if c.body().iter().any(|l| l.kind() == RY) {
        let Some(r) = role_of(ix, f) else { return };
        if !meets(ix, c, r) {
            return;
        }
        need.push(Lit::rl(f, r));
    }
    let fits = |n: Lit, p: Lit| match n.kind() {
        RL if dx.counting => p.kind() == RL && p.fun() == f && dx.sub(ix, p.atom(), n.atom()),
        _ => n == p,
    };
    // a new premise that fits no place of the clause gives no inference
    if let Some((p, _)) = fixed
        && !need.iter().any(|&n| fits(n, p))
    {
        return;
    }
    let mut head: Vec<Lit> = Vec::with_capacity(c.head().len());
    for l in c.head() {
        match l.kind() {
            Y => head.push(Lit::x(l.atom())),
            RY => head.push(Lit::rl(f, l.atom())),
            XL if dx.root.is_some() => {}
            _ => return,
        }
    }
    head.extend(up);
    let base: Side = (ground, head);
    let lists_free: Vec<Vec<Side>> = need
        .iter()
        .map(|&n| match n.kind() {
            RL if dx.counting => partners_rl(ix, dx, st, f, n.atom()),
            _ => partners_succ(ix, dx, st, n),
        })
        .collect();
    match fixed {
        None => found.products(lists_free, base),
        Some((p, side)) => {
            for (k, &n) in need.iter().enumerate() {
                if !fits(n, p) {
                    continue;
                }
                let mut lists = lists_free.clone();
                lists[k] = vec![side.clone()];
                found.products(lists, base.clone());
                // without counting the premise fits one place
                if !dx.counting {
                    break;
                }
            }
        }
    }
}

/// Runs the rules on up to `budget` clauses waiting in the store, and stops early once
/// they gave the context units, which the EL rules take first. The answer is the number
/// of clauses processed.
pub fn flush<S: Sink>(ix: &Index, me: Ex, st: &mut State, out: &mut S, budget: usize) -> usize {
    let mut n = 0;
    let mut max: Vec<Lit> = Vec::new();
    while n < budget {
        if st.bottom {
            break;
        }
        let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) else {
            break;
        };
        let dx = dix(ix);
        let id = if dx.root.is_some() {
            let inv = |r: Role| dx.inv(ix, r);
            let o = super::dl_clause::Rooted {
                ix,
                lits: &out.dl().lits,
                inv: &inv,
            };
            x.store.next(ix, Some(&o), &mut max)
        } else {
            x.store.next(ix, None, &mut max)
        };
        let Some(id) = id else {
            break;
        };
        let grown = x.store.take_grown();
        if grown > 0 {
            out.dl().grow(grown);
        }
        n += 1;
        let c = x.store.clauses[id as usize].c.clone();
        process(ix, me, st, id, &c, &max, out);
        // the units go to the EL rules first, and a stopped run leaves the rest queued
        if out.busy() || out.stopped() {
            break;
        }
    }
    n
}

/// The rules on one clause of the store, whose maximal literals are `max`.
fn process<S: Sink>(
    ix: &Index,
    me: Ex,
    st: &mut State,
    id: u32,
    c: &Clause,
    max: &[Lit],
    out: &mut S,
) {
    let roles = &ix.roles;
    let dx = dix(ix);
    // in a rooted run a clause can go to the predecessors and still have maximal literals
    let rooted = dx.root.is_some();
    let mut found = Found::default();
    if rooted {
        super::dl_root::join_body(ix, st, c, &mut found, &out.dl().lits);
        super::dl_root::register_body(ix, me, st, c, out);
    }
    if if rooted {
        pred_ready(ix, dx, &out.dl().lits, c)
    } else {
        max.is_empty()
    } {
        // Pred: the clause goes to the predecessors whose role meets its role atoms
        let x = ds(st);
        let Some(cid) = pred_id(x, id, out) else {
            drain(ix, st, found, out);
            return;
        };
        for &(u, e) in &x.dpreds {
            if role_of(ix, e).is_some_and(|r| meets(ix, c, r)) {
                out.send(u, Concl::PredC(me, cid));
            }
        }
        for (r, srcs) in &st.back {
            if meets(ix, c, *r) {
                for &e in &srcs.list {
                    out.send(e, Concl::PredC(me, cid));
                }
            }
        }
        // the predecessors that register later replay the clauses over the predecessor,
        // which a rooted run's store leaves to this test
        if rooted {
            ds(st).store.preds.push(id);
        }
        if !rooted || max.is_empty() {
            drain(ix, st, found, out);
            return;
        }
    }
    let mut ps: Vec<(Role, Ex)> = Vec::new();
    let mut es: Vec<Ex> = Vec::new();
    for &l in max {
        let prem = side(c, l);
        if dx.root.is_some() {
            rooted_max(ix, st, l, &prem, &mut found, &out.dl().lits);
        }
        match l.kind() {
            X => {
                let d = l.atom();
                let h = ix.head(d);
                match h.node {
                    Node::Conj(a, b) => {
                        found.push(with(&prem, Lit::x(a)));
                        found.push(with(&prem, Lit::x(b)));
                    }
                    Node::Exist(r, _) => {
                        found.push(with(&prem, Lit::rl(d, r)));
                        found.push(with(&prem, Lit::f(d, h.succ)));
                    }
                    _ => {}
                }
                for &t in ix.told_of(h) {
                    found.push(with(&prem, Lit::x(t)));
                }
                for &(p, k) in ix.conj_of(h) {
                    for q in partners_x(st, p) {
                        found.push(with(&merge(&prem, &q), Lit::x(k)));
                    }
                }
                for &set in ix.disjoint_of(h) {
                    for &m in &dx.sets[set as usize] {
                        if m != d {
                            for q in partners_x(st, m) {
                                found.push(merge(&prem, &q));
                            }
                        }
                    }
                }
                if let Some(ids) = dx.by_body.get(&d) {
                    for &cl in ids {
                        hyper_clause(ix, st, cl, d, &prem, &mut found);
                    }
                }
                // the universals d stands for, over the predecessor and the successors
                alls(ix, d, &mut ps);
                for &(t, x) in &ps {
                    if let Some(i) = roles.inv(t)
                        && dx.carries(i)
                    {
                        let mut p = prem.clone();
                        p.0.push(Lit::ry(i));
                        p.1.push(Lit::y(x));
                        found.push(p);
                    }
                    for (e, qs) in partners_rf(ix, st, t) {
                        for q in qs {
                            found.push(with(&merge(&prem, &q), Lit::f(e, x)));
                        }
                    }
                }
                if dx.counting {
                    if let Some(&(n, q)) = dx.atmost.get(&d) {
                        at_most(ix, st, n, q, vec![prem.clone()], None, &mut found);
                    }
                    on_filler(ix, st, d, &prem, &mut found);
                }
            }
            F | RL | EQ | NE => {
                let e = l.fun();
                // Succ: the successor takes the marker
                let v = target(ix, me, st, e, out);
                match l.kind() {
                    F => send_hyp(ix, st, v, l.atom(), out),
                    RL => on_rl(ix, st, e, l.atom(), &prem, &mut found),
                    _ => {}
                }
                // a new certificate that e(x) is in a class of a disjointness set: the
                // clauses with an equality or inequality of e(x) are derived again, which
                // simplifies them whatever order they came in, and the shorter clause
                // subsumes the old one
                if dx.counting && l.kind() == F {
                    again(ix, st, id, c, &mut found);
                }
                // a rooted run has its own equality rules
                if dx.counting && dx.root.is_none() {
                    if l.kind() == EQ {
                        on_eq(ix, st, Some(id), l, &prem, &mut found);
                    } else {
                        into_eq(ix, st, Some(id), l, &prem, &mut found);
                    }
                }
                if !matches!(l.kind(), F | RL) {
                    continue;
                }
                // Pred with the clauses the successors of e sent: the one registered with,
                // and the EL one when e is a unit
                es.clear();
                if let Some(&(_, w)) = dl(st).and_then(|x| x.edges.iter().find(|&&(f, _)| f == e)) {
                    es.push(w);
                }
                if st.subs.contains(&e) && !es.contains(&ix.head(e).succ) {
                    es.push(ix.head(e).succ);
                }
                let table = &out.dl().table;
                for &v in &es {
                    let cids = dl(st)
                        .and_then(|x| x.pred_in.get(&v))
                        .map_or(&[][..], |v| v.as_slice());
                    for &cid in cids {
                        pred(
                            ix,
                            st,
                            e,
                            table.get(cid),
                            Some((l, &prem)),
                            &mut found,
                            &out.dl().lits,
                        );
                    }
                }
            }
            XL if rooted => super::dl_root::on_max(ix, me, st, c, l, &prem, &mut found, out),
            _ => {}
        }
    }
    drain(ix, st, found, out);
}

/// Sends the successor `v` the marker of `a`, unless the successor's core has it.
pub(super) fn send_hyp<S: Sink>(ix: &Index, st: &mut State, v: Ex, a: Ex, out: &mut S) {
    if a == TOP || ds(st).sent.contains(&(v, a)) {
        return;
    }
    let mut core = Vec::new();
    if !dix(ix).of_hyp.contains_key(&v) {
        out.core(ix, v, &mut core);
    }
    if core.binary_search(&a).is_err() {
        ds(st).sent.insert((v, a));
        out.send(v, Concl::Hyp(a));
    }
}

/// The successor that e's literals go to. The context keeps the one it registered with.
/// Otherwise the cautious strategy takes the EL successor when e is a unit, whose core is
/// e's filler with the role's range, and else the hypothetical successor, with the empty
/// core. The eager strategy takes the context whose core is what holds of the successor
/// for certain: the EL successor when that is all, and else the hypothetical successor
/// for nothing, the context of the one atom, or that of the conjunction. A successor the
/// context registers with takes the markers of the EL core's atoms it lacks.
fn target<S: Sink>(ix: &Index, me: Ex, st: &mut State, e: Ex, out: &mut S) -> Ex {
    let dx = dix(ix);
    let unit = st.subs.contains(&e);
    let h = dx.hyp.get(&e).copied().unwrap_or(TOP);
    let succ = ix.head(e).succ;
    if let Some(&(_, v)) = dl(st).and_then(|x| x.edges.iter().find(|&&(f, _)| f == e)) {
        return v;
    }
    let v = if out.dl().eager {
        let mut k1: Vec<Ex> = Vec::new();
        if unit {
            k1.extend(dx.cores.get(&e).into_iter().flat_map(|c| c.iter().copied()));
        }
        if let Some(x) = dl(st) {
            for i in x.store.with_succ_fun(e) {
                let c = &x.store.clauses[i as usize].c;
                if let ([], [l]) = (c.body(), c.head())
                    && l.kind() == F
                {
                    k1.push(l.atom());
                }
            }
        }
        k1.retain(|&a| a != TOP);
        k1.sort_unstable();
        k1.dedup();
        if unit && dx.cores.get(&e).is_some_and(|c| **c == *k1) {
            return succ;
        }
        match k1.as_slice() {
            [] => h,
            [a] => *a,
            _ => out.intern(&k1),
        }
    } else {
        if unit {
            return succ;
        }
        h
    };
    ds(st).edges.push((e, v));
    out.send(v, Concl::DEdge(me, e));
    if dx.root.is_some() {
        super::dl_root::new_succ(st, v, out);
    }
    if let Some(core) = dx.cores.get(&e) {
        for &a in core.iter() {
            send_hyp(ix, st, v, a, out);
        }
    }
    v
}

/// Whether the ontology has a model, as far as a stopped run of Tier D knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Known {
    Consistent,
    Inconsistent,
    Unknown,
}

/// The dependency record of a run of Tier D that stopped: which roots have their whole
/// answer, and whether the ontology's consistency is decided.
///
/// A context's units and unsatisfiability depend only on the contexts it reaches forward:
/// the successors of its existential restrictions, the hypothetical successors it
/// registered with and the targets of its forward links. A predecessor's later markers
/// make clauses whose bodies only that predecessor resolves. A context is closed when it
/// has started and its to-do list and clause queue are empty, which a stopped worker's
/// returned conclusions and delivered outbox make false where work was left. A root is
/// complete when every context it reaches is closed, no candidate of its tests is left
/// untested, and the ontology is consistent. The ontology is consistent when `owl:Thing`'s
/// context and every context it reaches are closed and it is satisfiable, and
/// inconsistent when that context is unsatisfiable, whatever else is open.
pub fn record(
    ix: &Index,
    ctx: &super::saturate::Contexts,
    dynamic: Option<&super::saturate::Dynamic>,
    roots: &[Ex],
    refuted: &FxHashSet<(Ex, Ex)>,
) -> (Vec<bool>, Known) {
    let n = ctx.len() as Ex;
    let get = |e: Ex| -> Option<&super::saturate::Ctx> {
        if e < n {
            ctx[e as usize].get().map(|c| &**c)
        } else {
            dynamic.map(|d| d.ctx(e))
        }
    };
    let all: Vec<Ex> = (0..n)
        .chain(dynamic.map_or(0..0, |d| n..n + d.len() as Ex))
        .collect();
    // the contexts that are not closed, and the reverse of the forward edges
    let mut open: FxHashSet<Ex> = FxHashSet::default();
    let mut rev: FxHashMap<Ex, Vec<Ex>> = FxHashMap::default();
    let mut reached: Vec<Ex> = Vec::new();
    for &e in &all {
        let Some(c) = get(e) else { continue };
        let st = c.st.lock();
        let started = c.started.load(Ordering::Acquire) && st.init;
        let idle = dl(&st).is_none_or(|d| d.is_idle());
        if !started || !idle || !c.todo.lock().is_empty() {
            open.insert(e);
        }
        let mut edge = |to: Ex| {
            rev.entry(to).or_default().push(e);
            reached.push(to);
        };
        for &d in &st.subs {
            if let Node::Exist(..) = ix.head(d).node {
                edge(ix.head(d).succ);
            }
        }
        if let Some(d) = dl(&st) {
            for &(_, v) in &d.edges {
                edge(v);
            }
        }
        for &(_, f) in &st.fwd.list {
            edge(f);
        }
    }
    // a context reached but never made has not started either, nor has a root's that
    // the run stopped before it made
    for e in reached.into_iter().chain(roots.iter().copied()) {
        if get(e).is_none() {
            open.insert(e);
        }
    }
    // the contexts that reach an open one
    let mut bad = open.clone();
    let mut stack: Vec<Ex> = open.into_iter().collect();
    while let Some(e) = stack.pop() {
        for &p in rev.get(&e).into_iter().flatten() {
            if bad.insert(p) {
                stack.push(p);
            }
        }
    }
    let unsat = |e: Ex| {
        ctx.get(e as usize)
            .and_then(|c| c.get())
            .is_some_and(|c| c.st.lock().bottom)
    };
    let known = if unsat(TOP) {
        Known::Inconsistent
    } else if !bad.contains(&TOP) {
        Known::Consistent
    } else {
        Known::Unknown
    };
    let pending: FxHashSet<Ex> = candidates(ix, ctx, roots, refuted)
        .into_iter()
        .map(|(x, _)| x)
        .collect();
    let complete = roots
        .iter()
        .map(|&r| match known {
            Known::Inconsistent => true,
            Known::Unknown => false,
            Known::Consistent => !bad.contains(&r) && !pending.contains(&r),
        })
        .collect();
    (complete, known)
}
