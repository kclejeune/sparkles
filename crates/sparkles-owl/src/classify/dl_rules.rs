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
    hash_table_bytes, map_vec_bytes, map_vec_push, map_vec_push_bound, next_hash_table_bytes,
    next_vec_cap, set_insert_bound, set_insert_delta, vec_grow_bound, vec_push_delta,
};
use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use super::saturate::{Concl, Guard, Sink, State, flatten, table_bytes};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

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
    /// item 728's clauses of a cardinality in a negative position, without a holder: the
    /// bound the at-most rule counts to, the qualified role and the head's atoms over x
    lazy: Vec<(u32, Role, Box<[Ex]>)>,
    /// for each expression, whether it is an at-least conjunction that the order of a run
    /// without the root ranks below every class, item 728
    low: Vec<bool>,
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
    /// whether the run orders its number restrictions, as [`super::dl_norm::DlAx::order`]
    pub order: bool,
    /// for a minimum's name, the names of the minimums that entail it, as
    /// [`super::dl_norm::DlAx::blockers`]
    pub blockers: FxHashMap<Ex, Box<[Ex]>>,
    /// for a minimum's name that has witnesses of its own, their conjunction
    pub witnesses: FxHashMap<Ex, Ex>,
    /// by expression, whether it is a minimum's name, for the order of atoms
    least: Vec<bool>,
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
        for (s, b) in dl
            .atmost
            .iter()
            .map(|&(_, _, s, b)| (s, b))
            .chain(dl.lazy.iter().map(|&(_, s, b, _)| (s, b)))
        {
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
        let mut qualified = |d: &mut DlIx, s: Role, b: Ex| {
            if b == TOP {
                s
            } else {
                *qual.entry((s, b)).or_insert_with(|| {
                    let q = d.base + 2 * d.quals.len() as Role;
                    d.quals.push((s, b));
                    d.quals_of.entry(b).or_default().push(q);
                    q
                })
            }
        };
        for &(m, k, s, b) in &dl.atmost {
            let q = qualified(&mut d, s, b);
            d.atmost.insert(m, (k, q));
            d.atmosts.push((m, k, q));
        }
        for (k, s, b, head) in &dl.lazy {
            let q = qualified(&mut d, *s, *b);
            d.lazy.push((*k, q, head.clone().into_boxed_slice()));
        }
        d.order = dl.order;
        d.blockers = dl
            .blockers
            .iter()
            .map(|(&h, v)| (h, v.clone().into_boxed_slice()))
            .collect();
        d.witnesses = dl
            .least
            .iter()
            .filter_map(|&(g, w)| w.map(|w| (g, w)))
            .collect();
        if d.order {
            d.least = vec![false; nodes];
            for &(g, _) in &dl.least {
                if let Some(x) = d.least.get_mut(g as usize) {
                    *x = true;
                }
            }
        }
        d.low = vec![false; nodes];
        for &h in &dl.low {
            if let Some(x) = d.low.get_mut(h as usize) {
                *x = true;
            }
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

    /// Whether every table of the run that a kept context reads is keyed by an
    /// expression that the context holds, so that [`DlIx::changed`] finds what an edit
    /// changes for it (roadmap item 753): a run without counting, a root or the order
    /// between number restrictions, whose tables by qualified role, of at-most
    /// restrictions, disjoint roles and minimums are empty.
    pub fn keyed_by_expressions(&self) -> bool {
        !self.counting
            && !self.order
            && self.root.is_none()
            && self.consts.is_empty()
            && self.quals.is_empty()
            && self.atmosts.is_empty()
            && self.lazy.is_empty()
            && self.rdis.is_empty()
            && self.ne_of.is_empty()
            && self.blockers.is_empty()
            && self.witnesses.is_empty()
    }

    /// Marks in `changed`, which covers the expressions of `old`, each expression that
    /// keys other rules here than in `old`: one in the body of a clause that only one of
    /// the two has, one with another hypothetical successor or successor core, and one
    /// that the order of atoms ranks differently.
    /// The bytes that [`DlIx::changed`] allocates for the comparison with `old`,
    /// roughly: a key for each clause of both, with a copy of its head.
    pub fn changed_scratch(&self, old: &DlIx) -> usize {
        let keys = |d: &DlIx| {
            d.clauses
                .iter()
                .map(|(_, h)| 56 + h.len() * 4)
                .sum::<usize>()
        };
        keys(old) + keys(self)
    }

    pub fn changed(&self, old: &DlIx, changed: &mut [bool]) {
        type Key<'a> = (&'a [Ex], Vec<Ex>);
        fn keys(d: &DlIx) -> FxHashSet<Key<'_>> {
            d.clauses
                .iter()
                .map(|(b, h)| {
                    let mut h = h.to_vec();
                    h.sort_unstable();
                    h.dedup();
                    (&**b, h)
                })
                .collect()
        }
        let (a, b) = (keys(old), keys(self));
        for (body, _) in a.symmetric_difference(&b) {
            for &e in body.iter() {
                if let Some(c) = changed.get_mut(e as usize) {
                    *c = true;
                }
            }
        }
        for (e, c) in changed.iter_mut().enumerate() {
            let e = e as Ex;
            *c |= self.hyp.get(&e) != old.hyp.get(&e)
                || self.cores.get(&e) != old.cores.get(&e)
                || self.is_low(e) != old.is_low(e)
                || self.is_least(e) != old.is_least(e);
        }
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
            + table_bytes(self.blockers.capacity(), 24)
            + self.blockers.values().map(|v| v.len() * 4).sum::<usize>()
            + table_bytes(self.witnesses.capacity(), 8)
            + self.least.capacity()
    }

    pub fn logical_bytes(&self) -> usize {
        self.clauses
            .iter()
            .map(|(b, h)| 40 + (b.len() + h.len()) * 4)
            .sum::<usize>()
            + self
                .by_body
                .values()
                .map(|v| 48 + v.len() * 4)
                .sum::<usize>()
            + self.sets.iter().map(|s| 24 + s.len() * 4).sum::<usize>()
            + table_bytes(self.hyp.len(), 8) * 2
            + self.cores.values().map(|c| 24 + c.len() * 4).sum::<usize>()
            + self
                .by_succ
                .values()
                .map(|v| 32 + v.len() * 4)
                .sum::<usize>()
            + self.reach.len()
            + self.below.iter().map(|v| 24 + v.len() * 4).sum::<usize>()
            + self.quals.len() * 8
            + table_bytes(self.quals_of.len(), 32)
            + table_bytes(self.atmost.len(), 12)
            + self.atmosts.len() * 12
            + self.rdis.len() * 8
            + self.carry.len()
            + self.consts.len() * 8
            + table_bytes(self.ne_of.len(), 32)
            + self.ne_of.values().map(|v| v.len() * 4).sum::<usize>()
            + table_bytes(self.blockers.len(), 24)
            + self.blockers.values().map(|v| v.len() * 4).sum::<usize>()
            + table_bytes(self.witnesses.len(), 8)
            + self.least.len()
    }

    /// The core of the successor that the EL rules give e.
    pub(super) fn core(&self, e: Ex) -> &[Ex] {
        self.cores.get(&e).map_or(&[][..], |c| c)
    }

    /// Whether `a` is an at-least conjunction of item 728's clauses that the order of a
    /// run without the root ranks below every class, which `SPARKLES_DL_LOW_H=1` asks for
    /// and is off by default until [`low_h`]'s measurement.
    #[inline]
    pub fn is_low(&self, a: Ex) -> bool {
        self.root.is_none() && low_h() && self.low.get(a as usize).copied().unwrap_or(false)
    }

    /// Whether `a` is the name of a minimum of a run that orders its number restrictions.
    #[inline]
    pub fn is_least(&self, a: Ex) -> bool {
        self.least.get(a as usize).copied().unwrap_or(false)
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

/// Which work-bound guard first requested a stop for a Tier D run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum WorkStopGuard {
    #[default]
    None = 0,
    /// Hard total-work limit (`work > work_limit`, including fixed bounds, Option 2's
    /// `2^25` ceiling, and `past(n)`).
    TotalLimit = 1,
    /// Option 2 root-derivation guard (`root_work > R`, evaluated at a `2^16` sample).
    RootWork = 2,
    /// Option 2 post-warmup absolute queue guard (`work >= W && raw_queue > Qmax`).
    QueueAbs = 3,
    /// Option 2 post-warmup queue-ratio guard (`work >= W && 10 * raw_queue > 3 * raw_pushed`).
    QueueRatio = 4,
    /// Product width-limit exhaustion (`too_wide`).
    WidthExhausted = 5,
    /// Unconditional forced exhaustion (`Found::over`, e.g., late-unit containment or
    /// at-most expansion).
    ForcedExhaust = 6,
}

impl WorkStopGuard {
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::TotalLimit,
            2 => Self::RootWork,
            3 => Self::QueueAbs,
            4 => Self::QueueRatio,
            5 => Self::WidthExhausted,
            6 => Self::ForcedExhaust,
            _ => Self::None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::TotalLimit => "total_limit",
            Self::RootWork => "root_work",
            Self::QueueAbs => "queue_abs",
            Self::QueueRatio => "queue_ratio",
            Self::WidthExhausted => "width_exhausted",
            Self::ForcedExhaust => "forced_exhaust",
        }
    }
}

/// Option 2 (`rawQ`) adaptive rooted stopping parameters (immutable for a run).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdaptivePolicy {
    /// Sample cadence as a bit shift: `16` => every `2^16 = 65,536` total work.
    pub sample_shift: u32,
    /// Rational backlog ratio numerator (`3` in `3 / 10`).
    pub ratio_num: u32,
    /// Rational backlog ratio denominator (`10` in `3 / 10`), tested as
    /// `(raw_queue as u128) * (ratio_den as u128) > (raw_pushed as u128) * (ratio_num as u128)`.
    pub ratio_den: u32,
    /// Maximum root derivations (`dl_root::derive_covered`) before stopping (`2^19`).
    pub root_limit: usize,
    /// Minimum total work before queue backlog guards activate (`2^22`, inclusive).
    pub warmup_work: usize,
    /// Maximum resident clause-heap entries across all stores after warmup (`2^17`).
    pub queue_max: usize,
    /// Hard total-work ceiling (`2^25`), also mirrored into `DlRun::work_limit`.
    pub ceiling: usize,
}

impl AdaptivePolicy {
    pub const PROVISIONAL: Self = Self {
        sample_shift: 16, // 2^16 = 65,536
        ratio_num: 3,
        ratio_den: 10,
        root_limit: 1 << 19,  // 524,288
        warmup_work: 1 << 22, // 4,194,304
        queue_max: 1 << 17,   // 131,072
        ceiling: 1 << 25,     // 33,554,432
    };
}

/// Attempt metadata carried for streamed observe/enforce diagnostic rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AdaptiveMeta {
    pub run_id: u64,
    pub attempt: u32,
    pub after_counting: bool,
    pub policy_name: &'static str,
    pub effective_cap: usize,
    pub mrepair: u8,
    pub threads: usize,
}

/// Boxed per-attempt state for Option 2 (`rawQ`) adaptive stopping and observe-only
/// diagnostics. Keeping this behind `Option<Box<AdaptiveState>>` keeps `DlRun` (and
/// idle `Saturation`) overhead to one pointer with zero heap allocations when off.
pub(super) struct AdaptiveState {
    policy: AdaptivePolicy,
    enforce: bool,
    diag: bool,
    meta: AdaptiveMeta,
    /// Derivations counted strictly at `dl_root::derive_covered` (after `st.bottom`).
    root_work: AtomicUsize,
    /// Resident clause-heap entries across all participating stores (`sum(store.queue.len())`).
    raw_queue: AtomicUsize,
    /// Cumulative successful clause-heap pushes across all participating stores (`P`).
    raw_pushed: AtomicUsize,
    /// Direct marker-repair work charges (`dl_root.rs`), tracked only when `diag` is on.
    mrepair_work: AtomicUsize,
    /// Latched first requesting guard (`WorkStopGuard` as `u8`), written only when `enforce`.
    stop_guard: AtomicU8,
    /// Total `work` at the exact moment `stop_guard` first latched.
    stop_requested_at: AtomicUsize,
    /// Latched first hypothetical guard in observe-only mode (`!enforce`).
    hypothetical_guard: AtomicU8,
    /// Total `work` at the exact moment `hypothetical_guard` first latched.
    hypothetical_at: AtomicUsize,
    /// Number of `2^sample_shift` sample crossings observed.
    samples: AtomicUsize,
    /// Set at the joined saturation boundary so post-saturation rounds (e.g. entangled
    /// individual marking) emit no late `ADAPTIVE-SAMPLE` rows and latch no policy guards.
    sampling_stopped: std::sync::atomic::AtomicBool,
    /// Set ONLY when `enforce` is active and a non-work stop has already won on a
    /// `partial = true` run, disarming adaptive work-bound enforcement for the subsequent
    /// individual-marking round without modifying fixed-limit or non-adaptive paths.
    enforce_disarmed: std::sync::atomic::AtomicBool,
}

impl AdaptiveState {
    fn new(policy: AdaptivePolicy, enforce: bool, diag: bool, meta: AdaptiveMeta) -> Self {
        debug_assert!(policy.sample_shift < usize::BITS);
        debug_assert!(policy.ceiling > 0);
        Self {
            policy,
            enforce,
            diag,
            meta,
            root_work: AtomicUsize::new(0),
            raw_queue: AtomicUsize::new(0),
            raw_pushed: AtomicUsize::new(0),
            mrepair_work: AtomicUsize::new(0),
            stop_guard: AtomicU8::new(WorkStopGuard::None as u8),
            stop_requested_at: AtomicUsize::new(0),
            hypothetical_guard: AtomicU8::new(WorkStopGuard::None as u8),
            hypothetical_at: AtomicUsize::new(0),
            samples: AtomicUsize::new(0),
            sampling_stopped: std::sync::atomic::AtomicBool::new(false),
            enforce_disarmed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[inline]
    fn latch_stop(&self, guard: WorkStopGuard, at_work: usize) {
        if guard == WorkStopGuard::None {
            return;
        }
        if self.stop_guard.load(Ordering::Relaxed) != WorkStopGuard::None as u8 {
            return;
        }
        if self
            .stop_guard
            .compare_exchange(
                WorkStopGuard::None as u8,
                guard as u8,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            self.stop_requested_at.store(at_work, Ordering::Relaxed);
            #[cfg(test)]
            ON_ADAPTIVE_LATCH_TEST.with(|c| {
                if let Some(f) = c.borrow().as_ref() {
                    f(guard, at_work);
                }
            });
        }
    }

    #[inline]
    fn latch_hypothetical(&self, guard: WorkStopGuard, at_work: usize) {
        if guard == WorkStopGuard::None {
            return;
        }
        if self.hypothetical_guard.load(Ordering::Relaxed) != WorkStopGuard::None as u8 {
            return;
        }
        if self
            .hypothetical_guard
            .compare_exchange(
                WorkStopGuard::None as u8,
                guard as u8,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            self.hypothetical_at.store(at_work, Ordering::Relaxed);
        }
    }

    #[inline]
    fn latch_policy_guard(&self, guard: WorkStopGuard, at_work: usize) {
        if self.enforce {
            self.latch_stop(guard, at_work);
        } else {
            self.latch_hypothetical(guard, at_work);
        }
    }
}

#[cfg(test)]
type AdaptiveLatchHook = std::sync::Arc<dyn Fn(WorkStopGuard, usize) + Send + Sync>;
#[cfg(test)]
type AdaptiveSampleHook = std::sync::Arc<dyn Fn(usize, WorkStopGuard) + Send + Sync>;

#[cfg(test)]
thread_local! {
    static ROOTED_ADAPTIVE_BOUND_TEST: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
    static ROOTED_ADAPTIVE_DIAG_TEST: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
    static ROOTED_ADAPTIVE_POLICY_TEST: std::cell::Cell<Option<AdaptivePolicy>> =
        const { std::cell::Cell::new(None) };
    static ON_ADAPTIVE_LATCH_TEST: std::cell::RefCell<Option<AdaptiveLatchHook>> =
        const { std::cell::RefCell::new(None) };
    static ON_ADAPTIVE_SAMPLE_TEST: std::cell::RefCell<Option<AdaptiveSampleHook>> =
        const { std::cell::RefCell::new(None) };
    static ADAPTIVE_ALLOC_COUNT_TEST: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Whether Option 2 (`rawQ`) adaptive rooted stopping is enabled
/// (`SPARKLES_ROOTED_ADAPTIVE_BOUND=1`), off by default.
pub(super) fn rooted_adaptive_bound() -> bool {
    #[cfg(test)]
    if let Some(on) = ROOTED_ADAPTIVE_BOUND_TEST.with(|c| c.get()) {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_ADAPTIVE_BOUND").is_some_and(|v| v == "1"))
}

/// Whether observe-only / streamed adaptive diagnostics are enabled
/// (`SPARKLES_ROOTED_ADAPTIVE_DIAG=1`), off by default.
pub(super) fn rooted_adaptive_diag() -> bool {
    #[cfg(test)]
    if let Some(on) = ROOTED_ADAPTIVE_DIAG_TEST.with(|c| c.get()) {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_ADAPTIVE_DIAG").is_some_and(|v| v == "1"))
}

/// Active [`AdaptivePolicy`] for the current thread (always [`AdaptivePolicy::PROVISIONAL`]
/// outside tests).
pub(super) fn active_adaptive_policy() -> AdaptivePolicy {
    #[cfg(test)]
    if let Some(p) = ROOTED_ADAPTIVE_POLICY_TEST.with(|c| c.get()) {
        return p;
    }
    AdaptivePolicy::PROVISIONAL
}

#[cfg(test)]
pub(super) fn with_rooted_adaptive_bound<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ROOTED_ADAPTIVE_BOUND_TEST.with(|c| c.set(self.0));
        }
    }
    let prev = ROOTED_ADAPTIVE_BOUND_TEST.with(|c| c.replace(Some(on)));
    let _reset = Reset(prev);
    f()
}

#[cfg(test)]
pub(super) fn with_rooted_adaptive_diag<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ROOTED_ADAPTIVE_DIAG_TEST.with(|c| c.set(self.0));
        }
    }
    let prev = ROOTED_ADAPTIVE_DIAG_TEST.with(|c| c.replace(Some(on)));
    let _reset = Reset(prev);
    f()
}

#[cfg(test)]
pub(super) fn with_rooted_adaptive_policy<R>(policy: AdaptivePolicy, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<AdaptivePolicy>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ROOTED_ADAPTIVE_POLICY_TEST.with(|c| c.set(self.0));
        }
    }
    let prev = ROOTED_ADAPTIVE_POLICY_TEST.with(|c| c.replace(Some(policy)));
    let _reset = Reset(prev);
    f()
}

#[cfg(test)]
pub(super) fn with_on_adaptive_latch<R>(
    hook: impl Fn(WorkStopGuard, usize) + Send + Sync + 'static,
    f: impl FnOnce() -> R,
) -> R {
    struct Reset(Option<AdaptiveLatchHook>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ON_ADAPTIVE_LATCH_TEST.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let prev = ON_ADAPTIVE_LATCH_TEST.with(|c| c.borrow_mut().replace(std::sync::Arc::new(hook)));
    let _reset = Reset(prev);
    f()
}

#[cfg(test)]
pub(super) fn with_on_adaptive_sample<R>(
    hook: impl Fn(usize, WorkStopGuard) + Send + Sync + 'static,
    f: impl FnOnce() -> R,
) -> R {
    struct Reset(Option<AdaptiveSampleHook>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ON_ADAPTIVE_SAMPLE_TEST.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let prev = ON_ADAPTIVE_SAMPLE_TEST.with(|c| c.borrow_mut().replace(std::sync::Arc::new(hook)));
    let _reset = Reset(prev);
    f()
}

#[cfg(test)]
pub(super) fn reset_adaptive_alloc_count() {
    ADAPTIVE_ALLOC_COUNT_TEST.with(|c| c.set(0));
}

#[cfg(test)]
pub(super) fn adaptive_alloc_count() -> usize {
    ADAPTIVE_ALLOC_COUNT_TEST.with(|c| c.get())
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
    /// the conclusions that the runs before this one derived since the last run that
    /// started again, whose contexts this run keeps, and which its bound counts with its
    /// own (roadmap item 990)
    carried: usize,
    /// set once the run passes its bound on derived conclusions
    worked: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// set once an inference was too wide to make, which passes the bound whatever the
    /// count is
    exhausted: std::sync::atomic::AtomicBool,
    /// the most choices of one product that the run makes before it passes its bound on
    /// derived conclusions, zero for no limit (roadmap item 1011)
    width_limit: AtomicUsize,
    /// row 1231's marker repair (PLAN v3.1's rule M), which is off unless
    /// [`Limits::rooted_marker_repair`](super::Limits) turns it on: [`MREPAIR_ON`] with
    /// [`MREPAIR_CF`] by default (M-return-cf, item 1250), with [`MREPAIR_HOLDER`],
    /// [`MREPAIR_UNITS`] and [`MREPAIR_WIDE`] for measurements, and [`MREPAIR_NO_RETURN`]
    /// and [`MREPAIR_NO_REPLAY`] for the diagnostic's tests
    pub mrepair: u8,
    /// whether item 1061 phase 1 unit-only provenance-tagged shared root facts and
    /// constants-only successor replay deduplication are enabled (default off, behind
    /// `SPARKLES_ROOTED_SHARED_FACTS=1`)
    pub(super) shared_facts: bool,
    /// the repair's counts, for measurements: the notices of held triggers the root took,
    /// the markers it sent and the clauses it returned
    pub mcount: [AtomicUsize; 3],
    /// Option 2 (`rawQ`) adaptive stopping / observe-only diagnostic state (`None` when off).
    adaptive: Option<Box<AdaptiveState>>,
}

/// [`DlRun::mrepair`]: M-send and M-return are on.
pub const MREPAIR_ON: u8 = 1;
/// [`DlRun::mrepair`]: M-send also sends the marker to the holder itself, which holds it
/// already, so the marker is a tautology there.
pub const MREPAIR_HOLDER: u8 = 2;
/// [`DlRun::mrepair`]: M-send sends the marker of a trigger that is a live unit of the root
/// too, which the registered contexts take as the root's fact already.
pub const MREPAIR_UNITS: u8 = 4;
/// [`DlRun::mrepair`]: M-send sends the marker to every registered context, the wide arm,
/// and not only to those that the root reaches by successors, the pair set P_R of PLAN v3.1.
pub const MREPAIR_WIDE: u8 = 8;
/// [`DlRun::mrepair`], for the deficits' tests only: the root returns no clause (M-return),
/// so that a missing product shows in the diagnostic's second deficit.
pub const MREPAIR_NO_RETURN: u8 = 32;
/// [`DlRun::mrepair`], for the deficits' tests only: a context that the root registers or
/// reaches late takes no marker sent before, so that a missing marker shows in the first.
pub const MREPAIR_NO_REPLAY: u8 = 64;
/// [`DlRun::mrepair`]: M-return-cf. M-return resolves a returned clause only with premises
/// of the holder whose body has no ground literal (no `no_xy` literal over constants or
/// function terms of constants, while permitting `x`-only bodies), at both initial return
/// arrival and late-premise replay. A product with a ground body then never arises, which
/// would go back to the root and out again through each alternative condition of its body
/// atoms. Enabled by default whenever [`Limits::rooted_marker_repair`](super::Limits) is on
/// (`MREPAIR_ON | MREPAIR_CF`, item 1250); `SPARKLES_ROOTED_MARKER_REPAIR=1` selects
/// unfiltered [`MREPAIR_ON`] for comparison.
pub const MREPAIR_CF: u8 = 128;

impl DlRun {
    pub fn new(eager: bool, worked: std::sync::Arc<std::sync::atomic::AtomicBool>) -> DlRun {
        DlRun {
            eager,
            worked,
            shared_facts: shared_facts_switch(),
            ..DlRun::default()
        }
    }
    #[cfg(test)]
    pub(super) fn with_shared_facts(mut self, on: bool) -> DlRun {
        self.shared_facts = on;
        self
    }
    pub fn bytes(&self) -> usize {
        self.table.bytes()
            + self.lits.unadmitted_bytes()
            + self.stores.load(Ordering::Relaxed)
            + self
                .adaptive
                .as_ref()
                .map_or(0, |_| std::mem::size_of::<AdaptiveState>())
    }
    /// Starts another run over the table of the last one, whose kept contexts name its
    /// clauses by id (roadmap item 753). The bytes of the kept contexts' stores count
    /// with their states, so the run counts only what it adds, and it counts its own
    /// conclusions. Its bound on derived conclusions counts those of the runs before it
    /// too, back to the last that started again, which derived the kept contexts' (roadmap
    /// item 990), and a kept run past the bound starts again in full. The sum stands for
    /// what a run that starts again over the same axioms would derive, and the contexts
    /// of removed axioms still count in it, but it is not a bound on that: such a run
    /// derives its redundant conclusions in another order and can derive more, so a kept
    /// run can answer in full where a run that starts again falls back.
    pub fn begin_run(&mut self) {
        *self.stores.get_mut() = 0;
        self.lits.sync_unadmitted_on_begin_run();
        self.carried = self.carried.saturating_add(*self.work.get_mut());
        *self.work.get_mut() = 0;
        *self.work_limit.get_mut() = 0;
        *self.width_limit.get_mut() = 0;
        *self.exhausted.get_mut() = false;
        self.adaptive = None;
    }
    /// Enables Option 2 (`rawQ`) adaptive tracking for this attempt. When `enforce` is
    /// `true`, also sets `work_limit = policy.ceiling` and arms the adaptive stop latch;
    /// when `enforce` is `false` (observe-only diagnostic mode), keeps the caller-selected
    /// fixed `work_limit` and leaves the stop latch untouched.
    pub(super) fn enable_adaptive(
        &mut self,
        policy: AdaptivePolicy,
        enforce: bool,
        diag: bool,
        meta: AdaptiveMeta,
    ) {
        #[cfg(test)]
        ADAPTIVE_ALLOC_COUNT_TEST.with(|c| c.set(c.get() + 1));
        self.adaptive = Some(Box::new(AdaptiveState::new(policy, enforce, diag, meta)));
        if enforce {
            self.set_work_limit(policy.ceiling);
        }
    }
    /// Stops adaptive sampling at the joined saturation boundary so post-saturation
    /// rounds (such as entangled individual marking) emit no late `ADAPTIVE-SAMPLE` rows
    /// and latch no post-saturation policy guards, without affecting fixed-limit or
    /// forced-exhaustion work enforcement.
    pub(super) fn stop_adaptive_sampling(&self) {
        if let Some(a) = self.adaptive.as_deref() {
            a.sampling_stopped.store(true, Ordering::Relaxed);
        }
    }
    /// Disarms adaptive work-bound enforcement once a non-work stop has already won on a
    /// `partial = true` run, keeping `AdaptiveState` allocated so `bytes()` accounting
    /// remains accurate through individual marking and partial result construction.
    /// Non-adaptive (`adaptive.is_none()`) and observe-only (`!a.enforce`) runs are
    /// completely unaffected and keep their verbatim fixed-limit, width-limit, and
    /// forced-exhaustion enforcement.
    pub(super) fn disarm_adaptive_work_bound(&self) {
        if let Some(a) = self.adaptive.as_deref()
            && a.enforce
        {
            a.sampling_stopped.store(true, Ordering::Relaxed);
            a.enforce_disarmed.store(true, Ordering::Relaxed);
        }
    }
    #[inline]
    pub(super) fn tracks_adaptive(&self) -> bool {
        self.adaptive.is_some()
    }
    #[inline]
    pub(super) fn is_adaptive_enforced(&self) -> bool {
        self.adaptive.as_deref().is_some_and(|a| a.enforce)
    }
    #[inline]
    pub(super) fn is_adaptive_diag(&self) -> bool {
        self.adaptive.as_deref().is_some_and(|a| a.diag)
    }
    pub(super) fn adaptive_meta(&self) -> Option<AdaptiveMeta> {
        self.adaptive.as_deref().map(|a| a.meta)
    }
    pub(super) fn root_work(&self) -> usize {
        self.adaptive
            .as_deref()
            .map_or(0, |a| a.root_work.load(Ordering::Relaxed))
    }
    pub(super) fn raw_queue(&self) -> usize {
        self.adaptive
            .as_deref()
            .map_or(0, |a| a.raw_queue.load(Ordering::Relaxed))
    }
    pub(super) fn raw_pushed(&self) -> usize {
        self.adaptive
            .as_deref()
            .map_or(0, |a| a.raw_pushed.load(Ordering::Relaxed))
    }
    pub(super) fn mrepair_work(&self) -> usize {
        self.adaptive
            .as_deref()
            .map_or(0, |a| a.mrepair_work.load(Ordering::Relaxed))
    }
    pub(super) fn stop_guard(&self) -> WorkStopGuard {
        self.adaptive.as_deref().map_or(WorkStopGuard::None, |a| {
            WorkStopGuard::from_u8(a.stop_guard.load(Ordering::Relaxed))
        })
    }
    pub(super) fn stop_requested_at(&self) -> usize {
        self.adaptive
            .as_deref()
            .map_or(0, |a| a.stop_requested_at.load(Ordering::Relaxed))
    }
    pub(super) fn hypothetical_guard(&self) -> WorkStopGuard {
        self.adaptive.as_deref().map_or(WorkStopGuard::None, |a| {
            WorkStopGuard::from_u8(a.hypothetical_guard.load(Ordering::Relaxed))
        })
    }
    pub(super) fn hypothetical_at(&self) -> usize {
        self.adaptive
            .as_deref()
            .map_or(0, |a| a.hypothetical_at.load(Ordering::Relaxed))
    }
    pub(super) fn adaptive_samples(&self) -> usize {
        self.adaptive
            .as_deref()
            .map_or(0, |a| a.samples.load(Ordering::Relaxed))
    }
    /// Counts `n` attempted root derivations in `dl_root::derive_covered`.
    #[inline]
    pub(super) fn count_root(&self, n: usize) {
        if let Some(a) = self.adaptive.as_deref() {
            a.root_work.fetch_add(n, Ordering::Relaxed);
        }
    }
    /// Counts `n` direct marker-repair work units when diagnostic mode is active.
    #[inline]
    pub(super) fn count_mrepair(&self, n: usize) {
        if n > 0
            && let Some(a) = self.adaptive.as_deref()
            && a.diag
        {
            a.mrepair_work.fetch_add(n, Ordering::Relaxed);
        }
    }
    /// Records one successful insertion into a participating store's processing heap.
    #[inline]
    pub(super) fn on_queue_push(&self) {
        if let Some(a) = self.adaptive.as_deref() {
            a.raw_pushed.fetch_add(1, Ordering::Relaxed);
            a.raw_queue.fetch_add(1, Ordering::Relaxed);
        }
    }
    /// Records `n` pops (dead or live) from a participating store's processing heap.
    #[inline]
    pub(super) fn on_queue_pop(&self, n: usize) {
        if n == 0 {
            return;
        }
        if let Some(a) = self.adaptive.as_deref() {
            let prev = a.raw_queue.fetch_sub(n, Ordering::Relaxed);
            assert!(
                prev >= n,
                "raw_queue underflow: attempted to pop {n} with raw_queue={prev}"
            );
        }
    }
    pub(super) fn grow(&self, bytes: usize) {
        self.stores.fetch_add(bytes, Ordering::Relaxed);
    }
    pub(super) fn shrink(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let _ = self
            .stores
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                Some(m.saturating_sub(bytes))
            });
    }
    /// Bounds the conclusions the run derives, which bounds its time where redundant
    /// conclusions keep its memory flat.
    pub fn set_work_limit(&self, n: usize) {
        self.work_limit.store(n, Ordering::Relaxed);
    }
    /// Limits the choices of one product of the run, past which it passes its bound on
    /// derived conclusions (roadmap item 1011).
    pub fn set_width_limit(&self, n: usize) {
        self.width_limit.store(n, Ordering::Relaxed);
    }
    /// Whether a product of these lists has more choices than the run's limit.
    pub(super) fn too_wide(&self, lists: &[Vec<Side>]) -> bool {
        let l = self.width_limit.load(Ordering::Relaxed);
        l != 0
            && lists
                .iter()
                .try_fold(1usize, |n, x| n.checked_mul(x.len()).filter(|&n| n <= l))
                .is_none()
    }
    /// Whether the run has a bound on the conclusions it derives.
    pub fn bounded(&self) -> bool {
        self.work_limit.load(Ordering::Relaxed) != 0
    }
    /// Counts `n` derived conclusions.
    pub(super) fn count(&self, n: usize) {
        let Some(a) = self.adaptive.as_deref() else {
            self.work.fetch_add(n, Ordering::Relaxed);
            return;
        };
        self.count_adaptive(a, n);
    }
    #[inline]
    fn count_adaptive(&self, a: &AdaptiveState, n: usize) {
        if n == 0 {
            return;
        }
        if a.sampling_stopped.load(Ordering::Relaxed) {
            self.work.fetch_add(n, Ordering::Relaxed);
            return;
        }
        let old = self
            .work
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |w| {
                Some(w.saturating_add(n))
            })
            .unwrap_or(usize::MAX);
        let new = old.saturating_add(n);
        let old_total = self.carried.saturating_add(old);
        let new_total = self.carried.saturating_add(new);
        let crossed = (old_total >> a.policy.sample_shift) != (new_total >> a.policy.sample_shift);
        if crossed {
            self.evaluate_adaptive_sample(a, new_total);
        } else if new_total > a.policy.ceiling {
            a.latch_policy_guard(WorkStopGuard::TotalLimit, new_total);
        }
    }
    #[cold]
    #[inline(never)]
    fn evaluate_adaptive_sample(&self, a: &AdaptiveState, work: usize) {
        a.samples.fetch_add(1, Ordering::Relaxed);
        if a.enforce && !a.diag && a.stop_guard.load(Ordering::Relaxed) != WorkStopGuard::None as u8
        {
            return;
        }
        let root_work = a.root_work.load(Ordering::Relaxed);
        let raw_q = a.raw_queue.load(Ordering::Relaxed);
        let raw_p = a.raw_pushed.load(Ordering::Relaxed);
        assert!(
            raw_q <= raw_p,
            "raw_queue ({raw_q}) exceeded raw_pushed ({raw_p})"
        );
        let p = &a.policy;
        // Declared simultaneous-crossing priority: total, root, absolute queue, ratio.
        let guard = if work > p.ceiling {
            WorkStopGuard::TotalLimit
        } else if root_work > p.root_limit {
            WorkStopGuard::RootWork
        } else if work >= p.warmup_work {
            if raw_q > p.queue_max {
                WorkStopGuard::QueueAbs
            } else if raw_p > 0
                && (raw_q as u128) * (p.ratio_den as u128) > (raw_p as u128) * (p.ratio_num as u128)
            {
                WorkStopGuard::QueueRatio
            } else {
                WorkStopGuard::None
            }
        } else {
            WorkStopGuard::None
        };
        if guard != WorkStopGuard::None {
            a.latch_policy_guard(guard, work);
        }
        #[cfg(test)]
        ON_ADAPTIVE_SAMPLE_TEST.with(|c| {
            if let Some(f) = c.borrow().as_ref() {
                f(work, guard);
            }
        });
        if a.diag {
            let first = if a.enforce {
                WorkStopGuard::from_u8(a.stop_guard.load(Ordering::Relaxed))
            } else {
                WorkStopGuard::from_u8(a.hypothetical_guard.load(Ordering::Relaxed))
            };
            let mw = a.mrepair_work.load(Ordering::Relaxed);
            eprintln!(
                "ADAPTIVE-SAMPLE run={} attempt={} mode={} policy={} cap={} after_counting={} mrepair={} threads={} work={} root_work={} raw_q={} raw_p={} mrepair_work={} trigger={} first={}",
                a.meta.run_id,
                a.meta.attempt,
                if a.enforce { "enforce" } else { "observe" },
                a.meta.policy_name,
                a.meta.effective_cap,
                usize::from(a.meta.after_counting),
                a.meta.mrepair,
                a.meta.threads,
                work,
                root_work,
                raw_q,
                raw_p,
                mw,
                guard.as_str(),
                first.as_str(),
            );
        }
    }
    /// Whether `n` conclusions alone, with those of the runs whose contexts this one
    /// keeps, are more than the run's bound, and marks the run past its bound when they
    /// are.
    pub(super) fn past(&self, n: usize) -> bool {
        if self
            .adaptive
            .as_deref()
            .is_some_and(|a| a.enforce && a.enforce_disarmed.load(Ordering::Relaxed))
        {
            return false;
        }
        let l = self.work_limit.load(Ordering::Relaxed);
        let past = l != 0 && self.carried.saturating_add(n) > l;
        if past {
            self.exhaust_with(WorkStopGuard::TotalLimit);
        }
        past
    }
    /// The conclusions derived so far.
    pub fn work(&self) -> usize {
        self.work.load(Ordering::Relaxed)
    }
    /// Passes the bound on derived conclusions at once, recording `guard` when adaptive
    /// enforcement is active.
    fn exhaust_with(&self, guard: WorkStopGuard) {
        if let Some(a) = self.adaptive.as_deref()
            && a.enforce
        {
            if a.enforce_disarmed.load(Ordering::Relaxed) {
                return;
            }
            let w = self
                .carried
                .saturating_add(self.work.load(Ordering::Relaxed));
            a.latch_stop(guard, w);
        }
        self.exhausted.store(true, Ordering::Relaxed);
    }
    /// Passes the bound on derived conclusions at once, for an inference too wide to make.
    /// The mark stays, where a sum would wrap once four inferences added a quarter of
    /// the range each before a check read it, and the run would then end as complete
    /// without them.
    #[cfg(test)]
    pub(super) fn exhaust(&self) {
        self.exhaust_with(WorkStopGuard::ForcedExhaust);
    }
    #[inline]
    fn over_work_adaptive(&self, a: &AdaptiveState) -> bool {
        if self.exhausted.load(Ordering::Relaxed) {
            let w = self
                .carried
                .saturating_add(self.work.load(Ordering::Relaxed));
            a.latch_stop(WorkStopGuard::ForcedExhaust, w);
            self.worked.store(true, Ordering::Relaxed);
            return true;
        }
        if a.stop_guard.load(Ordering::Relaxed) != WorkStopGuard::None as u8 {
            self.worked.store(true, Ordering::Relaxed);
            return true;
        }
        let l = self.work_limit.load(Ordering::Relaxed);
        let w = self
            .carried
            .saturating_add(self.work.load(Ordering::Relaxed));
        let over = l != 0 && w > l;
        if over {
            a.latch_stop(WorkStopGuard::TotalLimit, w);
            self.worked.store(true, Ordering::Relaxed);
        }
        over
    }
    /// Whether the run derived more conclusions than its bound, with those of the runs
    /// whose contexts it keeps, or left out an inference too wide to make, which passes
    /// the bound even when the run has none, since the run is incomplete without it.
    pub fn over_work(&self) -> bool {
        if let Some(a) = self.adaptive.as_deref()
            && a.enforce
        {
            if a.enforce_disarmed.load(Ordering::Relaxed) {
                return false;
            }
            return self.over_work_adaptive(a);
        }
        let l = self.work_limit.load(Ordering::Relaxed);
        let over = self.exhausted.load(Ordering::Relaxed)
            || l != 0
                && self
                    .carried
                    .saturating_add(self.work.load(Ordering::Relaxed))
                    > l;
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
    /// in a rooted run, whether the context is a hypothetical successor, whose core is
    /// empty, so that its clauses hold of every element under no assumption
    pub empty_core: bool,
    /// in a rooted run, whether the context is taking a fact of the root, which it keeps
    pub taking_fact: bool,
    /// in a rooted run, whether the clause currently being processed by [`process`] is an
    /// exact empty-body unit delivered from the root (`Stored::fact == true`), cleared on
    /// every exit from `process`
    pub given_fact: bool,
    /// in a rooted run, the heads of the facts a hypothetical successor sent the root
    pub(super) promoted: FxHashSet<Box<[Lit]>>,
    /// the clauses over the predecessor that successors sent, by successor
    pub pred_in: FxHashMap<Ex, Vec<u32>>,
    pub(super) pred_seen: FxHashSet<(Ex, u32)>,
    /// the predecessors that registered, with the existential restriction they reach this
    /// context by
    pub dpreds: Vec<(Ex, Ex)>,
    /// the successors this context registered with, by existential restriction: the
    /// hypothetical successor, or under the eager strategy the context of what holds of
    /// the successor for certain. Kept sorted by `(target, ex_id)` with no duplicates so
    /// [`Self::edges_to`] slices a successor's explicit edges in O(log E) without scanning.
    edges: Vec<(Ex, Ex)>,
    /// the markers this context holds
    hyps: FxHashSet<Ex>,
    /// the markers sent, by target
    sent: FxHashSet<(Ex, Ex)>,
    /// the table ids of the clauses `s(y, x) → c(y)` of the propagations, which the
    /// registered predecessors take
    props: FxHashMap<(Role, Ex), u32>,
    /// in a test context, the named classes that do not hold, sorted
    pub neg: Vec<Ex>,
    /// in the root of a rooted run, its indexes by constant
    pub root: Option<Box<super::dl_root::RootSt>>,
    /// with the order between number restrictions, the names of the minimums that hold
    /// and wait for the context's units before they make their witnesses, since a minimum
    /// that entails one of them makes its own; [`flush`] releases them
    pending: Vec<Ex>,
}

impl DlSt {
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<DlSt>()
            + self.store.bytes()
            + map_vec_bytes(&self.pred_in)
            + hash_table_bytes(self.pred_seen.capacity(), std::mem::size_of::<(Ex, u32)>())
            + self.dpreds.capacity() * std::mem::size_of::<(Ex, Ex)>()
            + self.edges.capacity() * std::mem::size_of::<(Ex, Ex)>()
            + hash_table_bytes(self.hyps.capacity(), std::mem::size_of::<Ex>())
            + hash_table_bytes(self.sent.capacity(), std::mem::size_of::<(Ex, Ex)>())
            + hash_table_bytes(
                self.props.capacity(),
                std::mem::size_of::<((Role, Ex), u32)>(),
            )
            + hash_table_bytes(self.promoted.capacity(), std::mem::size_of::<Box<[Lit]>>())
            + self
                .promoted
                .iter()
                .map(|h| h.len() * std::mem::size_of::<Lit>())
                .sum::<usize>()
            + self.neg.capacity() * std::mem::size_of::<Ex>()
            + self.root.as_ref().map_or(0, |r| r.bytes())
            + self.pending.capacity() * std::mem::size_of::<Ex>()
    }

    /// Whether the context has no clause waiting for the rules and no minimum waiting to
    /// make its witnesses.
    pub fn is_idle(&self) -> bool {
        self.store.is_idle() && self.pending.is_empty()
    }

    /// Whether the context holds the marker of `a`.
    pub fn has_hyp(&self, a: Ex) -> bool {
        self.hyps.contains(&a)
    }

    /// All explicit `(ex_id, target)` edges registered by this context, sorted by
    /// `(target, ex_id)` with no duplicates.
    #[inline]
    pub(super) fn edges(&self) -> &[(Ex, Ex)] {
        &self.edges
    }

    /// The contiguous slice of explicit `(ex_id, v)` edges reaching successor `v`, sorted
    /// in strictly ascending order of restriction id `ex_id`.
    #[inline]
    pub(super) fn edges_to(&self, v: Ex) -> &[(Ex, Ex)] {
        let start = self.edges.partition_point(|&(_, w)| w < v);
        let end = start + self.edges[start..].partition_point(|&(_, w)| w == v);
        &self.edges[start..end]
    }

    /// Returns the registered successor of existential restriction `e`, if any.
    #[inline]
    pub(super) fn edge_target(&self, e: Ex) -> Option<Ex> {
        self.edges.iter().find(|&&(f, _)| f == e).map(|&(_, w)| w)
    }

    /// Inserts `(e, v)` into `self.edges` in ascending `(v, e)` order (deduplicated),
    /// pre-admitting any vector growth against `out`.
    pub(super) fn insert_edge_admit<S: Sink>(&mut self, e: Ex, v: Ex, out: &mut S) -> bool {
        let pos = self.edges.partition_point(|&(pe, pw)| (pw, pe) < (v, e));
        if self.edges.get(pos) == Some(&(e, v)) {
            return true;
        }
        let Some(bound) = vec_grow_bound(&self.edges) else {
            out.data_overflow();
            return false;
        };
        if bound > 0 && !out.admit_dl(bound) {
            return false;
        }
        let before = self.edges.capacity();
        self.edges.insert(pos, (e, v));
        let delta = (self.edges.capacity() - before) * std::mem::size_of::<(Ex, Ex)>();
        if bound > 0 {
            out.commit_dl(bound, delta);
        }
        true
    }

    /// Test helper that inserts `(e, v)` while preserving the `(v, e)` sort invariant.
    #[cfg(test)]
    pub(super) fn insert_edge(&mut self, e: Ex, v: Ex) {
        let pos = self.edges.partition_point(|&(pe, pw)| (pw, pe) < (v, e));
        if self.edges.get(pos) != Some(&(e, v)) {
            self.edges.insert(pos, (e, v));
        }
    }

    /// Whether a marker or a stored clause of the context, dead or not, names an
    /// expression that `changed` marks, or one it does not cover (roadmap item 753).
    /// Every rule reads the index by the atoms and the successor terms of its premises,
    /// and a premise is a unit of the context, a marker or a clause its store took, so a
    /// context that names no marked expression here or among its units made no inference
    /// that the changed rules give or take. A clause the store subsumed on arrival was a
    /// premise of none. The equalities and the interned literals of runs that count or
    /// have a root are not read, and count as marked.
    pub fn names_changed(&self, changed: &[bool]) -> bool {
        let hit = |e: Ex| changed.get(e as usize).copied().unwrap_or(true);
        self.hyps.iter().any(|&a| hit(a))
            || self.store.clauses.iter().any(|s| {
                s.c.body().iter().chain(s.c.head()).any(|l| match l.kind() {
                    X | Y => hit(l.atom()),
                    F => hit(l.fun()) || hit(l.atom()),
                    RL => hit(l.fun()),
                    RY => false,
                    _ => true,
                })
            })
    }

    /// Removes the contexts in `gone` from the predecessors that registered, which
    /// register again when a run saturates them again (roadmap item 753).
    pub fn drop_predecessors(&mut self, gone: &FxHashSet<Ex>) {
        self.dpreds.retain(|(u, _)| !gone.contains(u));
    }

    /// Adds `delta` to every context id from `from` upward that the state names, as
    /// [`State::shift_contexts`] does for the rest of the state.
    pub fn shift_contexts(&mut self, from: Ex, delta: Ex) {
        let up = |e: Ex| if e >= from { e + delta } else { e };
        for (u, _) in &mut self.dpreds {
            *u = up(*u);
        }
        for (_, v) in &mut self.edges {
            *v = up(*v);
        }
        debug_assert!(
            self.edges
                .windows(2)
                .all(|w| (w[0].1, w[0].0) < (w[1].1, w[1].0))
        );
        if self.pred_in.keys().any(|&v| v >= from) {
            self.pred_in = std::mem::take(&mut self.pred_in)
                .into_iter()
                .map(|(v, c)| (up(v), c))
                .collect();
            self.pred_seen = std::mem::take(&mut self.pred_seen)
                .into_iter()
                .map(|(v, c)| (up(v), c))
                .collect();
        }
        if self.sent.iter().any(|&(v, _)| v >= from) {
            self.sent = std::mem::take(&mut self.sent)
                .into_iter()
                .map(|(v, a)| (up(v), a))
                .collect();
        }
        if let Some(r) = self.root.as_deref_mut()
            && r.pred_pending.keys().any(|&v| v >= from)
        {
            r.pred_pending = std::mem::take(&mut r.pred_pending)
                .into_iter()
                .map(|(v, c)| (up(v), c))
                .collect();
        }
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

/// Whether the run has made the test context of whether `x` is below one of `bs`.
pub fn has_test(d: &super::saturate::Dynamic, x: Ex, bs: &[Ex]) -> bool {
    let mut core = vec![x];
    core.extend(bs.iter().map(|&b| b | NEG));
    d.find(&core).is_some()
}

/// The class that a test context with the core `core` tests.
pub fn tested_class(core: &[Ex]) -> Option<Ex> {
    core.iter().copied().find(|&m| m & NEG == 0)
}

/// Starts a test context: its core, `owl:Thing` and the class it denies.
pub fn init_test<S: Sink>(st: &mut State, core: &[Ex], out: &mut S) {
    out.me(Concl::Sub(TOP));
    for &m in core {
        if m & NEG != 0 {
            let Some(x) = ds_admit(st, out) else {
                return;
            };
            if !vec_push_admit(&mut x.neg, m & !NEG, out) {
                mark_interrupted(st);
                return;
            }
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

/// Whether `x` has an untested candidate subsumer in the canonical model of its
/// disjunctive clauses, admitting its scratch against `admit` before allocation.
/// Returns `true` (conservatively treating `x` as incomplete) if admission or allocation
/// fails.
pub(super) fn has_pending_candidate(
    ix: &Index,
    ctx: &super::saturate::Contexts,
    x: Ex,
    tested: &FxHashSet<(Ex, Ex)>,
    admit: &mut dyn FnMut(usize) -> bool,
) -> bool {
    let Some(c) = ctx.get(x as usize).and_then(|c| c.get()) else {
        return false;
    };
    let st = c.st.lock();
    if st.bottom {
        return false;
    }
    let Some(d) = dl(&st) else {
        return false;
    };
    let mut eligible = 0usize;
    let mut max_head = 0usize;
    for s in &d.store.clauses {
        if !s.dead && s.c.body().is_empty() {
            eligible += 1;
            max_head = max_head.max(s.c.head().len());
        }
    }
    if eligible == 0 {
        return false;
    }
    let cs_bytes = eligible.checked_mul(std::mem::size_of::<((u8, Ex), &Clause)>());
    let max_bytes = max_head.checked_mul(std::mem::size_of::<Lit>());
    // Conservative hash-table estimate for `FxHashSet<Ex>` holding up to `eligible` entries,
    // clamping hashbrown's 4-bucket minimum for small non-empty tables.
    let model_bytes = eligible
        .checked_mul(8)
        .and_then(|n| n.checked_div(7))
        .and_then(|n| n.checked_next_power_of_two())
        .map(|b| b.max(4))
        .and_then(|b| b.checked_mul(std::mem::size_of::<Ex>() + 1))
        .and_then(|b| b.checked_add(16));
    let Some(scratch) = cs_bytes
        .and_then(|a| a.checked_add(max_bytes?))
        .and_then(|a| a.checked_add(model_bytes?))
    else {
        return true;
    };
    if !admit(scratch) {
        return true;
    }
    let mut max = Vec::new();
    if max.try_reserve_exact(max_head).is_err() {
        return true;
    }
    let mut cs: Vec<((u8, Ex), &Clause)> = Vec::new();
    if cs.try_reserve_exact(eligible).is_err() {
        return true;
    }
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
    // Allocation-free sort: every clause in `cs` has a single strictly maximal literal
    // `a = k.1`, so all other head literals have strictly smaller rank and equal-key
    // clauses share the same `a`, making tie order irrelevant to `model` and the result.
    cs.sort_unstable_by_key(|&(k, _)| k);
    let mut model: FxHashSet<Ex> = FxHashSet::default();
    if model.try_reserve(cs.len()).is_err() {
        return true;
    }
    for (k, c) in cs {
        let a = k.1;
        let holds = c
            .head()
            .iter()
            .any(|l| l.kind() == X && (st.subs.contains(&l.atom()) || model.contains(&l.atom())));
        if holds {
            continue;
        }
        model.insert(a);
        if a != x && matches!(ix.head(a).node, Node::Class(_)) && !tested.contains(&(x, a)) {
            return true;
        }
    }
    false
}

pub(super) fn ds(st: &mut State) -> &mut DlSt {
    st.extra().dl.get_or_insert_with(Box::default)
}

/// Marks `st` interrupted without allocating a `DlSt` if it does not yet have one, so a
/// partial answer never reports an interrupted context complete.
#[inline]
pub(super) fn mark_interrupted(st: &mut State) {
    st.interrupted = true;
    if let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) {
        x.store.interrupted = true;
    }
}

/// Ensures `st` has its `Extra` and `DlSt`, pre-admitting any newly boxed structs against
/// `out` before allocation. Returns `None` when `out` refuses the allocation.
pub(super) fn ds_admit<'a, S: Sink>(st: &'a mut State, out: &mut S) -> Option<&'a mut DlSt> {
    if st.extra.is_some() && dl(st).is_some() {
        return Some(ds(st));
    }
    let mut need = 0usize;
    if st.extra.is_none() {
        need += std::mem::size_of::<super::saturate::Extra>();
    }
    if dl(st).is_none() {
        need += std::mem::size_of::<DlSt>();
    }
    if !out.admit_dl(need) {
        mark_interrupted(st);
        return None;
    }
    let x = ds(st);
    out.commit_dl(need, need);
    Some(x)
}

/// Pushes `v` to `list`, pre-admitting reallocation overlap against `out`.
pub(super) fn vec_push_admit<T, S: Sink>(list: &mut Vec<T>, v: T, out: &mut S) -> bool {
    let Some(bound) = vec_grow_bound(list) else {
        out.data_overflow();
        return false;
    };
    if bound > 0 && !out.admit_dl(bound) {
        return false;
    }
    let delta = vec_push_delta(list, v);
    if bound > 0 {
        out.commit_dl(bound, delta);
    }
    true
}

/// Inserts `v` into `set`, pre-admitting rehash overlap against `out`. Returns `Some(true)`
/// when inserted, `Some(false)` when already present, and `None` when refused.
pub(super) fn set_insert_admit<T: Eq + std::hash::Hash, S: Sink>(
    set: &mut FxHashSet<T>,
    v: T,
    out: &mut S,
) -> Option<bool> {
    if set.contains(&v) {
        return Some(false);
    }
    let Some(bound) = set_insert_bound(set, &v) else {
        out.data_overflow();
        return None;
    };
    if bound > 0 && !out.admit_dl(bound) {
        return None;
    }
    let delta = set_insert_delta(set, v).unwrap_or(0);
    if bound > 0 {
        out.commit_dl(bound, delta);
    }
    Some(true)
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
    let h_len = c.head().iter().filter(|&&l| l != without).count();
    let mut head = Vec::with_capacity(h_len);
    head.extend(c.head().iter().copied().filter(|&l| l != without));
    (c.body().to_vec(), head)
}

/// The premise of a unit.
#[inline]
pub(super) fn unit() -> Side {
    (Vec::new(), Vec::new())
}

/// Visits each live premise whose maximal literal is `a(x)` directly from the borrowed
/// store without allocating an intermediate `Vec<Side>`, stopping early when `f` returns
/// `false`. Passes `(body, head, without)`.
fn for_each_partner_x(
    st: &State,
    a: Ex,
    mut f: impl FnMut(&[Lit], &[Lit], Option<Lit>) -> bool,
) -> bool {
    if a == TOP || st.subs.contains(&a) {
        return f(&[], &[], None);
    }
    let Some(d) = dl(st) else {
        return true;
    };
    let lit = Lit::x(a);
    let allow_sel = sel_partners_x();
    for i in d.store.with_x(a) {
        let s = &d.store.clauses[i as usize];
        if (allow_sel || !s.sel) && !f(s.c.body(), s.c.head(), Some(lit)) {
            return false;
        }
    }
    true
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

/// Visits each live premise whose maximal literal is `l`, over `f(x)`, directly from the
/// borrowed store without allocating an intermediate `Vec<Side>`, stopping early when `cb`
/// returns `false`. Passes `(body, head, without)`.
fn for_each_partner_succ(
    ix: &Index,
    dx: &DlIx,
    st: &State,
    l: Lit,
    mut cb: impl FnMut(&[Lit], &[Lit], Option<Lit>) -> bool,
) -> bool {
    if holds_succ(ix, dx, st, l) {
        return cb(&[], &[], None);
    }
    let Some(d) = dl(st) else {
        return true;
    };
    for i in d.store.with_succ(l) {
        let cl = &d.store.clauses[i as usize].c;
        if !cb(cl.body(), cl.head(), Some(l)) {
            return false;
        }
    }
    true
}

/// Visits each live premise of `t(x, f(x))` directly from the borrowed store without
/// allocating an intermediate `Vec<Side>`, stopping early when `cb` returns `false`.
/// Passes `(body, head, without)`.
fn for_each_partner_rl(
    ix: &Index,
    dx: &DlIx,
    st: &State,
    f: Ex,
    t: Role,
    mut cb: impl FnMut(&[Lit], &[Lit], Option<Lit>) -> bool,
) -> bool {
    if st.subs.contains(&f) && role_of(ix, f).is_some_and(|r| dx.sub(ix, r, t)) {
        return cb(&[], &[], None);
    }
    let Some(d) = dl(st) else {
        return true;
    };
    for (i, l) in d.store.over(f) {
        if l.kind() == RL && dx.sub(ix, l.atom(), t) {
            let cl = &d.store.clauses[i as usize].c;
            if !cb(cl.body(), cl.head(), Some(l)) {
                return false;
            }
        }
    }
    true
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
        // a unit into a singleton has its constant in place of a successor term (item 822)
        units.retain(|&e| into_nominal(ix, e).is_none());
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

/// Whether a ground inference makes no clause that holds neither x nor y and has a body,
/// item 804's first departure's restriction (F24). `SPARKLES_ROOTED_GROUND_RESTRICT=0`
/// turns it off, for measurement.
pub fn ground_restrict() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_GROUND_RESTRICT").is_none_or(|v| v != "0"))
}

/// Whether the restriction on ground inferences leaves out the conclusion `s` of one: it
/// has a body and holds neither x nor y, so that every literal is over constants and
/// successor terms `f_e(u)` of constants.
pub(super) fn restricted(lits: &Lits, inv: impl Fn(Role) -> Option<Role> + Copy, s: &Side) -> bool {
    ground_restrict()
        && !s.0.is_empty()
        && s.0
            .iter()
            .chain(&s.1)
            .all(|&l| l.kind() == XL && no_xy(lits.view(l, inv)))
}

/// Whether a literal holds neither x nor y, so that its terms are constants and successor
/// terms of constants.
pub(super) fn no_xy(v: super::dl_clause::XLit) -> bool {
    use super::dl_clause::terms_of;
    let (ts, n) = terms_of(v);
    ts[..n]
        .iter()
        .all(|t| matches!(t, RTerm::C(_) | RTerm::Fc(..)))
}

/// Whether an existential restriction into a singleton class names its constant in place of
/// a successor term, item 822's departure (F24). `SPARKLES_ROOTED_NOMINAL_SUCC=0` turns it
/// off, for measurement.
pub fn nominal_succ() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_NOMINAL_SUCC").is_none_or(|v| v != "0"))
}

/// For an existential restriction `d = ∃r.N_v` into a singleton class, in a rooted run, its
/// role and the constant v, so that `d(t)` gives `r(t, v)` and the successor's context
/// over v in place of a successor term of t.
pub(super) fn into_nominal(ix: &Index, d: Ex) -> Option<(Role, u32)> {
    let dx = dix(ix);
    // a maximum must still merge r(x, v) with the other r-successors, which the counting
    // rules do over successor terms
    if !nominal_succ() || dx.root.is_none() || dx.counting {
        return None;
    }
    let h = ix.head(d);
    let Node::Exist(r, _) = h.node else {
        return None;
    };
    singleton_of(ix, h.succ).map(|v| (r, v))
}

/// The constant v when the class `a` is below the singleton `N_v` through told subsumers
/// and conjuncts alone, so that `a(t)` gives `t ≈ v` under no other premise. The search
/// looks at no more than [`SINGLETON_SEEN`] classes and answers `None` past them.
///
/// It runs on every existential the unfolding and the rules at constants meet, so it holds
/// its classes in arrays on the stack and allocates nothing. A search whose classes to
/// visit outgrow [`SINGLETON_OPEN`] starts again in [`singleton_of_on_heap`], which gives
/// the same answer with vectors.
pub(super) fn singleton_of(ix: &Index, a: Ex) -> Option<u32> {
    let mut todo = [a; SINGLETON_OPEN];
    let mut open = 1;
    let mut seen = [a; SINGLETON_SEEN];
    let mut met = 0;
    while open > 0 {
        open -= 1;
        let b = todo[open];
        if seen[..met].contains(&b) {
            continue;
        }
        if met == SINGLETON_SEEN {
            return None;
        }
        seen[met] = b;
        met += 1;
        let h = ix.head(b);
        let told = ix.told_of(h);
        let conj = match h.node {
            Node::Nominal(v) => return Some(v),
            Node::Conj(p, q) => Some((p, q)),
            _ => None,
        };
        if open + 2 + told.len() > SINGLETON_OPEN {
            return singleton_of_on_heap(ix, a);
        }
        if let Some((p, q)) = conj {
            todo[open] = p;
            todo[open + 1] = q;
            open += 2;
        }
        todo[open..open + told.len()].copy_from_slice(told);
        open += told.len();
    }
    None
}

/// [`singleton_of`] with vectors, for a search that outgrows its arrays.
fn singleton_of_on_heap(ix: &Index, a: Ex) -> Option<u32> {
    let mut todo = vec![a];
    let mut seen: Vec<Ex> = Vec::new();
    while let Some(b) = todo.pop() {
        if seen.contains(&b) || seen.len() == SINGLETON_SEEN {
            continue;
        }
        seen.push(b);
        let h = ix.head(b);
        match h.node {
            Node::Nominal(v) => return Some(v),
            Node::Conj(p, q) => todo.extend([p, q]),
            _ => {}
        }
        todo.extend_from_slice(ix.told_of(h));
    }
    None
}

/// The classes [`singleton_of`] looks at for one existential, at most.
const SINGLETON_SEEN: usize = 65;

/// The classes [`singleton_of`] holds on the stack to visit.
const SINGLETON_OPEN: usize = 256;

/// Whether a hypothetical successor sends its conclusions with an empty body over constants
/// and their successor terms to the root as facts in place of keeping them, item 804's
/// promotion (F24). `SPARKLES_ROOTED_PROMOTE=0` turns it off, for measurement.
pub fn promote() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_PROMOTE").is_none_or(|v| v != "0"))
}

/// Whether the at-least conjunctions of item 728's clauses rank below every class, which
/// `SPARKLES_DL_LOW_H=1` turns on. It is off until its measurement, since with it
/// `sriq-010` of the counting fixtures passes a bound of 8 million conclusions on one
/// thread where the present rank finishes it.
pub fn low_h() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_DL_LOW_H").is_some_and(|v| v == "1"))
}

/// For the unit `d`, the name of a minimum that another can entail, the conjunction of
/// its witnesses, which then waits in the context until the units are taken, [`flush`]
/// releasing it (F24, "The order between number restrictions").
pub fn defer<S: Sink>(ix: &Index, st: &mut State, d: Ex, out: &mut S) -> Option<Ex> {
    let dx = ix.dl.as_deref()?;
    if dx.blockers.is_empty() || !dx.blockers.contains_key(&d) {
        return None;
    }
    let w = dx.witnesses.get(&d).copied()?;
    let x = ds_admit(st, out)?;
    if !vec_push_admit(&mut x.pending, d, out) {
        x.store.interrupted = true;
        return None;
    }
    Some(w)
}

/// Whether the context holds, as a unit, a minimum that entails the minimum `d`.
fn blocked(dx: &DlIx, st: &State, d: Ex) -> bool {
    dx.blockers
        .get(&d)
        .is_some_and(|hs| hs.iter().any(|h| st.subs.contains(h)))
}

/// What a run that counts or has the root does when an existential restriction reaches a
/// successor after the successor sent clauses over the predecessor, item 1229 (F24). Pred
/// takes each such clause once, with the restrictions that reach the successor when it
/// comes, and drops it when the successor sends it again over a later link. A restriction
/// reaches a successor late as a unit derived later, as a unit that had an edge to another
/// successor before, or under the eager strategy as a second edge into a shared successor,
/// whose replay takes only the literal that made the edge. In these runs Pred's conclusion
/// can name the restriction's own successor term, in a role atom over it with counting or
/// in an equality with a constant in a rooted run, so the late restriction could miss what
/// the clause gives it. By default the context replays every clause the successor sent
/// with the late restriction in Pred, item 1229's repair, which is adopted on a measured
/// gate and not proved. `SPARKLES_COUNTING_LATE_UNIT=1` gives the containment instead,
/// in which a rooted run passes its bound on derived conclusions and classifies again
/// without counting or without the rooted rules, and `=0` turns both off, for
/// measurement.
pub fn late_unit_mode() -> u8 {
    #[cfg(test)]
    if let Some(m) = LATE_UNIT_MODE_TEST.with(|c| c.get()) {
        return m;
    }
    static ON: OnceLock<u8> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("SPARKLES_COUNTING_LATE_UNIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2)
    })
}

#[cfg(test)]
thread_local! {
    static LATE_UNIT_MODE_TEST: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn with_late_unit_mode_override<R>(mode: u8, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<u8>);
    impl Drop for Reset {
        fn drop(&mut self) {
            LATE_UNIT_MODE_TEST.with(|c| c.set(self.0));
        }
    }
    let prev = LATE_UNIT_MODE_TEST.with(|c| c.replace(Some(mode)));
    let _reset = Reset(prev);
    f()
}

/// Whether item 1061 phase 1 unit-only shared root facts and constants-only successor
/// deduplication are enabled (`SPARKLES_ROOTED_SHARED_FACTS=1`). Off by default until
/// measured on `ore_ont_14924`, `ore_ont_15803`, `dl1061` and merged `shoi-022`.
pub fn shared_facts_switch() -> bool {
    #[cfg(test)]
    if let Some(on) = SHARED_FACTS_TEST.with(|c| c.get()) {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_SHARED_FACTS").is_some_and(|v| v == "1"))
}

#[cfg(test)]
thread_local! {
    static SHARED_FACTS_TEST: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn with_shared_facts_override<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            SHARED_FACTS_TEST.with(|c| c.set(self.0));
        }
    }
    let prev = SHARED_FACTS_TEST.with(|c| c.replace(Some(on)));
    let _reset = Reset(prev);
    f()
}

#[cfg(test)]
thread_local! {
    /// Test-only failing-control bitmask for G6: bit 0 omits live non-root unit `PredC`,
    /// bit 1 omits late non-root unit `PredC` replay in `dedge`/`back`.
    static G6_SKIP_PREDC_TEST: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn with_g6_skip_predc_override<R>(mask: u8, f: impl FnOnce() -> R) -> R {
    struct Reset(u8);
    impl Drop for Reset {
        fn drop(&mut self) {
            G6_SKIP_PREDC_TEST.with(|c| c.set(self.0));
        }
    }
    let prev = G6_SKIP_PREDC_TEST.with(|c| c.replace(mask));
    let _reset = Reset(prev);
    f()
}

#[inline]
fn skip_non_root_live_predc() -> bool {
    #[cfg(test)]
    {
        G6_SKIP_PREDC_TEST.with(|c| c.get() & 1 != 0)
    }
    #[cfg(not(test))]
    {
        false
    }
}

#[inline]
fn skip_non_root_late_predc() -> bool {
    #[cfg(test)]
    {
        G6_SKIP_PREDC_TEST.with(|c| c.get() & 2 != 0)
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// Whether `st` (a non-root context) currently has at least one eligible ordinary edge
/// reaching successor `v` (an existential restriction with `into_nominal(ix, e).is_none()`).
pub(super) fn has_eligible_ordinary_edge(ix: &Index, dx: &DlIx, st: &State, v: Ex) -> bool {
    if let Some(x) = dl(st)
        && x.edges_to(v)
            .iter()
            .any(|&(e, _)| into_nominal(ix, e).is_none())
    {
        return true;
    }
    if dx.of_hyp.contains_key(&v) {
        return false;
    }
    dx.by_succ.get(&v).is_some_and(|es| {
        es.iter()
            .any(|&e| st.subs.contains(&e) && into_nominal(ix, e).is_none())
    })
}

/// Whether a run takes the rule of [`late_unit_mode`]: one that counts or has the root,
/// in which Pred's conclusion can name the restriction's successor term.
fn late_rules(dx: &DlIx) -> bool {
    dx.counting || dx.root.is_some()
}

/// The rule of [`late_unit_mode`] on a restriction `e` of the context `me` that newly
/// reaches the successor `v`, as a new unit whose EL successor is v, as a unit that had an
/// edge to v before, or as a new edge to v. Under the eager strategy two restrictions can
/// share a successor, so the second one's edge can come after the clauses did, and the
/// context replays on the edge only Pred with the literal that made it. The answer is
/// whether the rule replayed the clauses, which then include the products with that
/// literal.
#[allow(clippy::too_many_arguments)]
pub(super) fn late<S: Sink>(
    ix: &Index,
    me: Ex,
    st: &mut State,
    e: Ex,
    v: Ex,
    kind: &str,
    found: &mut Found,
    out: &mut Guard<'_, S>,
) -> bool {
    let dx = dix(ix);
    // the root takes no Pred over the successor terms of x, and a restriction into a
    // singleton has no successor term, as in pred_c
    if dx.is_root(me) || into_nominal(ix, e).is_some() {
        return false;
    }
    let has_cids = dl(st)
        .and_then(|x| x.pred_in.get(&v))
        .is_some_and(|cids| !cids.is_empty());
    if !has_cids {
        return false;
    }
    match late_unit_mode() {
        0 => false,
        // the containment holds only for a rooted run, which classifies again without
        // the rooted rules or counting
        1 if dx.root.is_none() => false,
        1 => {
            if tracing() {
                eprintln!("TR LATE {kind} {} e={e} succ={v}", tr_ctx(st, false));
            }
            found.over = true;
            false
        }
        _ => {
            let share = dx.root.is_some() && out.dl().shared_facts;
            if share {
                let mut failed = false;
                if let Some(pend) = st
                    .extra
                    .as_mut()
                    .and_then(|x| x.dl.as_deref_mut())
                    .and_then(|x| x.root.as_deref_mut())
                    .and_then(|r| r.pred_pending.get_mut(&v))
                {
                    let mut idx = 0;
                    while idx < pend.len() {
                        let cid = pend[idx];
                        let bytes = {
                            let c = out.dl().table.get(cid);
                            side_exact_bytes(c.body().len(), c.head().len())
                        };
                        let Some(bytes) = bytes else {
                            out.data_overflow();
                            failed = true;
                            break;
                        };
                        found.rule = "predg";
                        if !found.reserve_one_admitted(1, bytes, out) {
                            failed = true;
                            break;
                        }
                        let side = {
                            let c = out.dl().table.get(cid);
                            (c.body().to_vec(), c.head().to_vec())
                        };
                        found.push(side);
                        out.release_to(found.held_bytes());
                        idx += 1;
                    }
                    pend.drain(..idx);
                }
                if failed {
                    mark_interrupted(st);
                    return false;
                }
            }
            if let Some(cids) = dl(st).and_then(|x| x.pred_in.get(&v)) {
                for &cid in cids {
                    if share && constants_only(ix, dx, &out.dl().lits, out.dl().table.get(cid)) {
                        continue;
                    }
                    if !pred(ix, st, e, cid, None, found, out) {
                        mark_interrupted(st);
                        return false;
                    }
                }
            }
            true
        }
    }
}

/// Whether a context other than the root selects the ground literals in its bodies, item
/// 804's selection (F24). It is on, with item 1220's repair [`sel_partners_x`], since
/// without that repair two selected clauses of one context never meet in Hyper over x, so
/// that a run could report itself complete and miss an entailed subsumption. The repair is
/// adopted on a measured gate and its completeness is not proved.
/// `SPARKLES_ROOTED_SELECT_GROUND=0` turns the selection off, for measurement.
pub fn select_ground() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| !std::env::var_os("SPARKLES_ROOTED_SELECT_GROUND").is_some_and(|v| v == "0"))
}

/// Whether a context other than the root cuts clauses with a ground or `f_e(u)` body
/// literal from non-local ground Hyper, head-side Join, and purely ground/Fc Eq/Factor
/// even when [`select_ground`] is off (item 1250). On by default; setting both
/// `SPARKLES_ROOTED_SELECT_GROUND=0` and `SPARKLES_ROOTED_GROUND_BODY_CUT=0` reproduces
/// the original uncut containment arm for measurement.
pub fn ground_body_cut() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        !std::env::var_os("SPARKLES_ROOTED_GROUND_BODY_CUT").is_some_and(|v| v == "0")
    })
}

/// Whether Hyper over x takes selected clauses as partners, item 1220's repair R1 of the
/// selection: the partners of an atom over x, and in Pred from a successor over `f_e(u)`
/// the partners of an atom over `f_e(u)` ([`super::dl_root::root_pred`]). Without it two
/// selected clauses of one context never meet in such an inference. It matters only while
/// [`select_ground`] is on. It is on by default, and `SPARKLES_ROOTED_SEL_PARTNERS_X=0`
/// turns it off, for measurement.
pub fn sel_partners_x() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        !std::env::var_os("SPARKLES_ROOTED_SEL_PARTNERS_X").is_some_and(|v| v == "0")
    })
}

/// Which Joins the restriction on ground inferences exempts, part of item 1220's repair.
/// When the partners are facts that hold only under the context's core, no ancestor holds
/// them, and one Join, which resolves a single literal, leaves a ground clause with a body
/// that the restriction would drop before the next Join could discharge it. A Join whose
/// premise on the side of the head has no ground literal in its body is exempt, whatever
/// the residual head. `SPARKLES_ROOTED_JOIN_EXEMPT=0` restricts
/// every Join and `=1` exempts every Join, for measurement.
pub fn join_exempt() -> u8 {
    static ON: OnceLock<u8> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("SPARKLES_ROOTED_JOIN_EXEMPT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2)
    })
}

/// Whether the restriction leaves out the conclusion `c` of a Join whose premise on the
/// side of the head has the body `hb`.
/// Zero-allocation equivalent of `join_restricted` over borrowed body/head slices.
#[allow(clippy::too_many_arguments)]
pub(super) fn join_restricted_parts(
    lits: &Lits,
    inv: impl Fn(Role) -> Option<Role> + Copy,
    hb: &[Lit],
    b1: &[Lit],
    b2: &[Lit],
    without_b2: Option<Lit>,
    h1: &[Lit],
    without_h1: Option<Lit>,
    h2: &[Lit],
) -> bool {
    match join_exempt() {
        1 => false,
        2 if !hb
            .iter()
            .any(|&l| l.kind() == XL && no_xy(lits.view(l, inv))) =>
        {
            false
        }
        _ => {
            if !ground_restrict() {
                return false;
            }
            let has_body = !b1.is_empty() || b2.iter().any(|&l| Some(l) != without_b2);
            if !has_body {
                return false;
            }
            b1.iter()
                .copied()
                .chain(b2.iter().copied().filter(|&l| Some(l) != without_b2))
                .chain(h1.iter().copied().filter(|&l| Some(l) != without_h1))
                .chain(h2.iter().copied())
                .all(|l| l.kind() == XL && no_xy(lits.view(l, inv)))
        }
    }
}

/// Computes `(sel, gbody)` for a clause of a context other than the root. A clause whose
/// body holds a literal without x or y (a ground or `f_e(u)` literal, a ground marker
/// among them) gets `sel = select_ground()` and `gbody = select_ground() || ground_body_cut()`.
/// `gbody` cuts non-local ground Hyper, head-side Join, and purely ground/Fc Eq/Factor
/// even when `select_ground` is off (item 1250), while `sel` gates mixed non-local
/// Eq/Factor and (without [`sel_partners_x`]) Hyper-over-x / Fc-Pred partners.
pub(super) fn selection_flags(
    lits: &Lits,
    inv: impl Fn(Role) -> Option<Role> + Copy,
    c: &Clause,
) -> (bool, bool) {
    let has_gbody = selected_always(lits, inv, c);
    (
        select_ground() && has_gbody,
        (select_ground() || ground_body_cut()) && has_gbody,
    )
}

/// Whether `c`'s body holds a literal mentioning neither x nor y (plain ground or `f_e(u)`).
pub(super) fn selected_always(
    lits: &Lits,
    inv: impl Fn(Role) -> Option<Role> + Copy,
    c: &Clause,
) -> bool {
    c.body()
        .iter()
        .any(|&l| l.kind() == XL && no_xy(lits.view(l, inv)))
}

/// Whether an inference on the literal `v` is local to its context, so that a selected
/// clause and a marker may be its premises: the literal holds a successor term of the
/// context, or it is a role atom between x or y and a constant or a successor term of a
/// constant. No predecessor sees such a literal, so no other context can make the
/// inference for it.
pub(super) fn local(v: super::dl_clause::XLit) -> bool {
    use super::dl_clause::{XLit, terms_of};
    let (ts, n) = terms_of(v);
    let ts = &ts[..n];
    ts.iter().any(|t| matches!(t, RTerm::Fx(_)))
        || matches!(v, XLit::Role(..))
            && ts.iter().any(|t| matches!(t, RTerm::X | RTerm::Y))
            && ts.iter().any(|t| matches!(t, RTerm::C(_) | RTerm::Fc(..)))
}

/// Scratch bytes per element needed by [`prune`] on a product premise list with length >= 2:
/// covers both `sort_by_key` scratch (`size_of::<Side>()`) and `keep`
/// (`size_of::<(Side, u64, u64, bool)>()`).
pub(super) const PRUNE_SCRATCH_ELEM: usize = std::mem::size_of::<(Side, u64, u64, bool)>();

#[inline]
pub(super) fn side_cap_bytes(s: &Side) -> usize {
    (s.0.capacity() + s.1.capacity()) * std::mem::size_of::<Lit>()
}

/// Checked byte size of [`side(c, without)`] before allocation.
#[inline]
pub(super) fn side_bytes(c: &Clause, without: Lit) -> Option<usize> {
    let h_len = c.head().iter().filter(|&&l| l != without).count();
    side_exact_bytes(c.body().len(), h_len)
}

#[inline]
pub(super) fn prod_inner_bytes(lists_cap: usize, lists: &[Vec<Side>], base: &Side) -> usize {
    use std::mem::size_of;
    side_cap_bytes(base)
        + lists_cap * size_of::<Vec<Side>>()
        + lists
            .iter()
            .map(|l| l.capacity() * size_of::<Side>() + l.iter().map(side_cap_bytes).sum::<usize>())
            .sum::<usize>()
}

/// Checked byte size of an exact-capacity `(body, head)` `Side`.
#[inline]
pub(super) fn side_exact_bytes(b_len: usize, h_len: usize) -> Option<usize> {
    b_len
        .checked_add(h_len)?
        .checked_mul(std::mem::size_of::<Lit>())
}

/// Checked byte size of [`merge_parts`] before allocating the `Side`.
#[inline]
pub(super) fn side_parts_bytes(
    prem: &Side,
    part_body: &[Lit],
    part_head: &[Lit],
    without: Option<Lit>,
    add: Option<Lit>,
) -> Option<usize> {
    let b_len = prem.0.len().checked_add(part_body.len())?;
    let extra = part_head.iter().filter(|&&l| Some(l) != without).count();
    let h_len = prem
        .1
        .len()
        .checked_add(extra)?
        .checked_add(usize::from(add.is_some()))?;
    side_exact_bytes(b_len, h_len)
}

/// Builds the merged conclusion `(body, head)` with exact capacity directly from `prem`
/// and a borrowed partner slice without intermediate allocations or reallocations.
pub(super) fn merge_parts(
    prem: &Side,
    part_body: &[Lit],
    part_head: &[Lit],
    without: Option<Lit>,
    add: Option<Lit>,
) -> Side {
    let b_len = prem.0.len() + part_body.len();
    let mut b = Vec::with_capacity(b_len);
    b.extend_from_slice(&prem.0);
    b.extend_from_slice(part_body);
    let extra = part_head.iter().filter(|&&l| Some(l) != without).count();
    let h_len = prem.1.len() + extra + usize::from(add.is_some());
    let mut h = Vec::with_capacity(h_len);
    h.extend_from_slice(&prem.1);
    h.extend(part_head.iter().copied().filter(|&l| Some(l) != without));
    if let Some(a) = add {
        h.push(a);
    }
    (b, h)
}

/// Builds `(a.0, a.1 + [l])` with exact capacity and no reallocation.
#[inline]
pub(super) fn with_exact(a: &Side, l: Lit) -> Side {
    merge_parts(a, &[], &[], None, Some(l))
}

/// Builds `(a.0 + b.0, a.1 + b.1)` with exact capacity and no reallocation.
#[inline]
pub(super) fn merge_exact(a: &Side, b: &Side) -> Side {
    merge_parts(a, &b.0, &b.1, None, None)
}

/// Builds `(a.0 + b.0, a.1 + b.1 + [l])` with exact capacity and no reallocation.
#[inline]
pub(super) fn merge_with_exact(a: &Side, b: &Side, l: Lit) -> Side {
    merge_parts(a, &b.0, &b.1, None, Some(l))
}

/// What the rules on one clause or unit give: single conclusions, and choices of one
/// premise from every list of a product, which [`drain`] derives one at a time so that a
/// wide case analysis is never held whole. A product of a ground inference is marked, and
/// the choices that the restriction on ground inferences leaves out are not derived.
#[derive(Default)]
pub(super) struct Found {
    one: Vec<(Side, &'static str)>,
    one_side_bytes: usize,
    prods: Vec<(Vec<Vec<Side>>, Side, bool, &'static str)>,
    prods_bytes: usize,
    /// caller-owned live working bytes (such as the active `prem = side(c, l)` in
    /// `process`) currently covered by the caller's `Found` guard alongside `one` and
    /// `prods`; must be cleared back to `0` after the buffer drops and before `drain`
    extra_bytes: usize,
    /// set when an inference has more choices than a context enumerates, which passes the
    /// run's bound on derived conclusions instead
    over: bool,
    /// the rule that makes the conclusions pushed next, for the derivation trace
    pub(super) rule: &'static str,
    /// whether the conclusions are the root's, for the derivation trace
    pub(super) root: bool,
}

#[cfg(test)]
impl Found {
    /// The number of products, for tests.
    pub(super) fn products_len(&self) -> usize {
        self.prods.len()
    }
    /// The direct (non-product) conclusions collected so far, for tests.
    pub(super) fn one_sides(&self) -> Vec<Side> {
        self.one.iter().map(|(s, _)| s.clone()).collect()
    }
    /// Marks this batch as unconditionally exhausted, for tests.
    pub(super) fn mark_over(&mut self) {
        self.over = true;
    }
}

impl Found {
    pub(super) fn push(&mut self, s: Side) {
        self.one_side_bytes += side_cap_bytes(&s);
        self.one.push((s, self.rule));
    }
    /// Pre-admits `Found::one` geometric growth (including realloc overlap) plus `side_bytes`
    /// for `add_count` forthcoming single conclusions through `guard`, then reserves capacity
    /// in `self.one`. Returns `false` (leaving `self.one` untouched) on overflow or refusal.
    pub(super) fn reserve_one_admitted<S: Sink>(
        &mut self,
        add_count: usize,
        side_bytes: usize,
        guard: &mut Guard<'_, S>,
    ) -> bool {
        if add_count == 0 {
            return true;
        }
        let Some(need_len) = self.one.len().checked_add(add_count) else {
            guard.data_overflow();
            return false;
        };
        let elem = std::mem::size_of::<(Side, &'static str)>();
        let cap = self.one.capacity();
        let held = self.held_bytes();
        if need_len > cap {
            let target_cap = match cap.checked_mul(2) {
                Some(d) => need_len.max(d.max(4)),
                None => need_len,
            };
            let Some(new_cap_bytes) = target_cap.checked_mul(elem) else {
                guard.data_overflow();
                return false;
            };
            let old_cap_bytes = cap * elem;
            let Some(realloc_peak) = held.checked_add(new_cap_bytes) else {
                guard.data_overflow();
                return false;
            };
            let Some(post_peak) = (held - old_cap_bytes)
                .checked_add(new_cap_bytes)
                .and_then(|b| b.checked_add(side_bytes))
            else {
                guard.data_overflow();
                return false;
            };
            if !guard.ensure_admitted(realloc_peak.max(post_peak)) {
                return false;
            }
            self.one.reserve_exact(target_cap - self.one.len());
        } else {
            let Some(peak) = held.checked_add(side_bytes) else {
                guard.data_overflow();
                return false;
            };
            if !guard.ensure_admitted(peak) {
                return false;
            }
        }
        true
    }
    /// Pre-admits one single conclusion of `side_bytes` (plus any `Found::one` realloc
    /// overlap) through `guard` before invoking `make()`, pushes it into `self.one`, and
    /// releases any temporary realloc overlap while keeping `held_bytes()` admitted.
    pub(super) fn push_admitted<S: Sink>(
        &mut self,
        side_bytes: usize,
        make: impl FnOnce() -> Side,
        guard: &mut Guard<'_, S>,
    ) -> bool {
        if !self.reserve_one_admitted(1, side_bytes, guard) {
            return false;
        }
        let s = make();
        debug_assert_eq!(side_cap_bytes(&s), side_bytes);
        self.push(s);
        guard.release_to(self.held_bytes());
        true
    }
    /// Pre-admits `Found::prods` geometric growth (including realloc overlap) plus
    /// `prod_bytes` (the forthcoming `lists` and `base` payloads built after admission)
    /// plus `prune_scratch` on top of `self.held_bytes() + extra_live` (any live caller
    /// setup buffers already covered by `guard`), then reserves capacity in `self.prods`.
    /// Returns `false` (leaving `self.prods` untouched) on overflow or refusal.
    pub(super) fn reserve_prod_admitted<S: Sink>(
        &mut self,
        prod_bytes: usize,
        prune_scratch: usize,
        extra_live: usize,
        guard: &mut Guard<'_, S>,
    ) -> bool {
        let Some(need_len) = self.prods.len().checked_add(1) else {
            guard.data_overflow();
            return false;
        };
        let elem = std::mem::size_of::<(Vec<Vec<Side>>, Side, bool, &'static str)>();
        let cap = self.prods.capacity();
        let Some(live) = self.held_bytes().checked_add(extra_live) else {
            guard.data_overflow();
            return false;
        };
        let Some(total_new) = prod_bytes.checked_add(prune_scratch) else {
            guard.data_overflow();
            return false;
        };
        if need_len > cap {
            let target_cap = match cap.checked_mul(2) {
                Some(d) => need_len.max(d.max(4)),
                None => need_len,
            };
            let Some(new_cap_bytes) = target_cap.checked_mul(elem) else {
                guard.data_overflow();
                return false;
            };
            let old_cap_bytes = cap * elem;
            let Some(realloc_peak) = live.checked_add(new_cap_bytes) else {
                guard.data_overflow();
                return false;
            };
            let Some(post_peak) = (live - old_cap_bytes)
                .checked_add(new_cap_bytes)
                .and_then(|b| b.checked_add(total_new))
            else {
                guard.data_overflow();
                return false;
            };
            if !guard.ensure_admitted(realloc_peak.max(post_peak)) {
                return false;
            }
            self.prods.reserve_exact(target_cap - self.prods.len());
        } else {
            let Some(peak) = live.checked_add(total_new) else {
                guard.data_overflow();
                return false;
            };
            if !guard.ensure_admitted(peak) {
                return false;
            }
        }
        true
    }
    /// Pre-admits `Found::prods` growth plus `prod_bytes` and `prune_scratch` on top of
    /// `self.held_bytes() + extra_live`, constructs `(lists, base)` via `make()` only
    /// AFTER slot admission (so incoming payloads never overlap a moving realloc of
    /// `self.prods`), pushes the pruned product, and releases temporary scratch/overlap
    /// down to `self.held_bytes() + extra_live`.
    pub(super) fn push_product_admitted<S: Sink>(
        &mut self,
        prod_bytes: usize,
        prune_scratch: usize,
        extra_live: usize,
        ground: bool,
        make: impl FnOnce() -> (Vec<Vec<Side>>, Side),
        guard: &mut Guard<'_, S>,
    ) -> bool {
        if !self.reserve_prod_admitted(prod_bytes, prune_scratch, extra_live, guard) {
            return false;
        }
        let (lists, base) = make();
        debug_assert_eq!(
            prod_inner_bytes(lists.capacity(), &lists, &base),
            prod_bytes
        );
        self.products_of(lists, base, ground);
        let want = self
            .held_bytes()
            .checked_add(extra_live)
            .expect("extra_live admitted");
        guard.release_to(want);
        true
    }
    /// Each choice of one premise from every list, merged with `base`.
    pub(super) fn products(&mut self, lists: Vec<Vec<Side>>, base: Side) {
        self.products_of(lists, base, false);
    }
    /// A bound on the bytes that [`fold`] holds without a charge while it derives the
    /// products that are not ground, for a caller that reserves them before it drains:
    /// the largest over the products, since fold drops a product's tables before the next.
    /// fold charges each union it keeps, but not the slots of its `seen` table beyond the
    /// union itself, nor its step list's spare capacity, each with the buffer it grew from.
    /// The bound takes each step to make one union per choice, the product of the lengths
    /// of the lists so far, which can far exceed the unions a step keeps. Any overflow of
    /// a nonempty product's choice count or table model saturates cleanly to `usize::MAX`.
    pub(super) fn fold_slack(&self) -> usize {
        const SIDE: u128 = std::mem::size_of::<Side>() as u128;
        const MAX: u128 = usize::MAX as u128;
        self.prods
            .iter()
            .filter(|p| !p.2)
            .map(|p| fold_slack_lens(p.0.iter().map(Vec::len), SIDE, MAX) as usize)
            .max()
            .unwrap_or(0)
    }
    /// [`Found::products`] for a ground inference, whose choices the restriction filters.
    pub(super) fn ground_products(&mut self, lists: Vec<Vec<Side>>, base: Side) {
        self.products_of(lists, base, true);
    }
    /// Sets the caller-owned working buffer bytes currently live alongside `one` and
    /// `prods` under the same `Found` guard.
    #[inline]
    pub(super) fn set_extra_bytes(&mut self, bytes: usize) {
        self.extra_bytes = bytes;
    }
    /// Returns the caller-owned live working buffer bytes tracked on `Found`.
    #[inline]
    pub(super) fn extra_bytes(&self) -> usize {
        self.extra_bytes
    }
    /// The bytes that the conclusions and products waiting here hold, their vectors'
    /// capacities included, tracked incrementally in O(1).
    #[inline]
    pub(super) fn held_bytes(&self) -> usize {
        use std::mem::size_of;
        self.one.capacity() * size_of::<(Side, &str)>()
            + self.one_side_bytes
            + self.prods.capacity() * size_of::<(Vec<Vec<Side>>, Side, bool, &str)>()
            + self.prods_bytes
            + self.extra_bytes
    }
    fn products_of(&mut self, mut lists: Vec<Vec<Side>>, base: Side, ground: bool) {
        if lists.iter().any(|l| l.is_empty()) {
            return;
        }
        for l in &mut lists {
            prune(l);
        }
        self.prods_bytes += prod_inner_bytes(lists.capacity(), &lists, &base);
        self.prods.push((lists, base, ground, self.rule));
    }
}

#[cfg(test)]
std::thread_local! {
    static TEST_TRACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) struct TestTraceGuard(bool);

#[cfg(test)]
impl Drop for TestTraceGuard {
    fn drop(&mut self) {
        TEST_TRACE.with(|c| c.set(self.0));
    }
}

/// Forces [`tracing`] on and redirects `trace_shared_parts` to `std::io::sink()` on the
/// current thread for zero-allocation tracing tests.
#[cfg(test)]
pub(super) fn force_test_trace() -> TestTraceGuard {
    let prev = TEST_TRACE.with(|c| c.replace(true));
    TestTraceGuard(prev)
}

#[cfg(test)]
pub(super) fn test_trace_active() -> bool {
    TEST_TRACE.with(|c| c.get())
}

/// Whether the derivation trace is on, which `DLTRACE` in the environment turns on: each
/// conclusion a context derives, before normalization, goes to standard error as
/// `TR <context> <rule> [body] -> [head]`, the root as `R` and another context by the
/// address of its state, after a line `TR P <context> <id> [body] -> [head]` for each
/// clause processed, so that the conclusions between two such lines are the inferences
/// on the processed clause.
pub(super) fn tracing() -> bool {
    #[cfg(test)]
    if test_trace_active() {
        return true;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DLTRACE").is_some())
}

/// Zero-allocation formatter for a context's name in the derivation trace.
pub(super) struct FmtCtx(usize, bool);

impl std::fmt::Display for FmtCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.1 {
            f.write_str("R")
        } else {
            write!(f, "C{:x}", self.0 & 0xffffff)
        }
    }
}

#[inline]
pub(super) fn fmt_ctx(st: &State, root: bool) -> FmtCtx {
    FmtCtx(st as *const State as usize, root)
}

/// The context's name in the derivation trace.
pub(super) fn tr_ctx(st: &State, root: bool) -> String {
    fmt_ctx(st, root).to_string()
}

/// Drops from a product's list each premise whose side has every literal of another's: a
/// choice of it gives a conclusion that the other's choice subsumes. The sides are taken
/// shortest first, in their order among sides of one length, and each is compared with
/// the sides kept before it, first by a signature of its literals, then by a merge when
/// both sides are sorted without repeats, as the sides of [`fold`] and of stored clauses
/// are, and by a search of each literal otherwise.
fn prune(l: &mut Vec<Side>) {
    if l.len() < 2 {
        return;
    }
    let sig = |x: &[Lit]| x.iter().fold(0u64, |s, l| s | l.sig_bit());
    let strict = |x: &[Lit]| x.windows(2).all(|w| w[0] < w[1]);
    let within = |a: &(Side, u64, u64, bool), b: &(Side, u64, u64, bool)| {
        if a.1 & !b.1 != 0 || a.2 & !b.2 != 0 {
            return false;
        }
        let ((a0, a1), (b0, b1)) = (&a.0, &b.0);
        if a.3 && b.3 {
            super::dl_clause::subset(a0, b0) && super::dl_clause::subset(a1, b1)
        } else {
            a0.iter().all(|x| b0.contains(x)) && a1.iter().all(|x| b1.contains(x))
        }
    };
    l.sort_by_key(|s| s.0.len() + s.1.len());
    let mut keep: Vec<(Side, u64, u64, bool)> = Vec::with_capacity(l.len());
    for s in l.drain(..) {
        let (s0, s1) = (sig(&s.0), sig(&s.1));
        let sorted = strict(&s.0) && strict(&s.1);
        let s = (s, s0, s1, sorted);
        if !keep.iter().any(|k| within(k, &s)) {
            keep.push(s);
        }
    }
    l.extend(keep.into_iter().map(|k| k.0));
}

/// The choices of a product between two checks of the run's bounds.
const PRODUCT_POLL: usize = 256;

/// The most bytes of a product's record of its conclusions that may go uncharged when the
/// product ends between two checks of the bounds; a larger tail is charged at the end.
pub(super) const RECORD_SLACK: usize = 64 * 1024;

/// The most sides of one step of a product that [`fold`] keeps only where no other side
/// of the step has a subset of their literals, which costs a comparison of each side with
/// the sides kept before it.
const FOLD_PRUNE: usize = 4096;

/// The [`step_slack`] model in `u128` for a `Side` of `side` bytes, clamped once at the
/// end to `max`. Computing in `u128` avoids subtracting saturated `usize` values when
/// `unions * side` and the table size both exceed `max`.
fn step_slack_model(unions: u128, listed: u128, side: u128, max: u128) -> u128 {
    debug_assert!(unions <= u64::MAX as u128 && listed <= u64::MAX as u128);
    let table = |n: u128| -> u128 {
        let buckets = match n {
            0 => 0,
            1..4 => 4,
            4..8 => 8,
            _ => (n * 8 / 7).next_power_of_two(),
        };
        buckets * (side + 1) + 16
    };
    let grown = table(unions)
        + if unions >= 4 {
            table(unions / 2 + 1)
        } else {
            0
        };
    let cap = if listed == 0 {
        0
    } else {
        listed.next_power_of_two()
    };
    let list = (cap + cap / 2) * side;
    let slack = (grown - unions * side) + (list - listed * side);
    slack.min(max)
}

/// The [`Found::fold_slack`] bound for one product's list lengths, returning `max` when
/// a nonempty product's choice count overflows `max`.
fn fold_slack_lens(
    lens: impl ExactSizeIterator<Item = usize> + Clone,
    side: u128,
    max: u128,
) -> u128 {
    if lens.clone().any(|n| n == 0) {
        return 0;
    }
    let last = lens.len().saturating_sub(1);
    let mut choices = 1u128;
    let mut most = 0u128;
    for (i, len) in lens.enumerate() {
        let Some(next) = choices.checked_mul(len as u128).filter(|&c| c <= max) else {
            return max;
        };
        choices = next;
        // a last step past FOLD_PRUNE unions derives them as it makes them
        let listed = if i < last {
            choices
        } else {
            choices.min((FOLD_PRUNE + 1) as u128)
        };
        most = most.max(step_slack_model(choices, listed, side, max));
    }
    most
}

/// The bytes of one step of [`fold`] that its charge leaves out, for `unions` unions of
/// which `listed` stay in the step's list: the `seen` table's slots and control bytes
/// beyond the size of a union, which fold charges for each, together with the table of
/// half the buckets it grew from, and the list's capacity beyond its length, together with
/// the buffer of half that capacity it grew from. The buckets follow the hash table's
/// growth, a power of two at least 8/7 of the entries. All arithmetic runs in `u128` and
/// clamps once to `usize::MAX` at the end.
#[cfg(test)]
fn step_slack(unions: usize, listed: usize) -> usize {
    step_slack_model(
        unions as u128,
        listed as u128,
        std::mem::size_of::<Side>() as u128,
        usize::MAX as u128,
    ) as usize
}

/// Derives the conclusions of a product that is not ground (item 969): the union of one
/// premise from every list with `base`, list after list. Each step keeps the unions it
/// makes once, and, up to [`FOLD_PRUNE`] of them, only those that no other union of the
/// step contains. A union that contains another gives only conclusions that the other's
/// conclusions subsume, since the unions of both with the premises of the later lists
/// contain each other in the same way, and the store keeps no clause that another
/// subsumes. The products of the Pred rule on `ore_ont_14817` of ORE 2015 have up to a
/// million choices whose unions are a few thousand sets of literals, of which a few dozen
/// are least. Past [`FOLD_PRUNE`] unions of the steps before the last, each further one
/// counts as a derived conclusion for the run's bound, as each choice the product derives
/// does, and a last step past that many derives its unions as it makes them, so that a
/// bounded run does not build a wide product's steps unbounded. Each step's sets are charged to
/// the memory budget as they grow, and what a step drops is released when it ends. The
/// answer is whether the run goes on.
/// Target callback for [`drain`] / [`fold`], polymorphic over any [`Sink`] so internal RAII
/// reservation [`Guard`] wrappers pass through without locking the closure to one sink type.
pub(super) trait Deriver {
    fn derive<S: Sink>(&self, ix: &Index, st: &mut State, s: Side, out: &mut S);
}

struct ContextDerive;
impl Deriver for ContextDerive {
    #[inline]
    fn derive<S: Sink>(&self, ix: &Index, st: &mut State, s: Side, out: &mut S) {
        derive(ix, st, s, out);
    }
}

pub(super) struct RootDerive;
impl Deriver for RootDerive {
    #[inline]
    fn derive<S: Sink>(&self, ix: &Index, st: &mut State, s: Side, out: &mut S) {
        super::dl_root::derive(ix, st, s, out);
    }
}

#[cfg(test)]
pub(super) struct NoopDerive;
#[cfg(test)]
impl Deriver for NoopDerive {
    #[inline]
    fn derive<S: Sink>(&self, _ix: &Index, _st: &mut State, _s: Side, _out: &mut S) {}
}

#[allow(clippy::too_many_arguments)]
fn fold<S: Sink, D: Deriver>(
    ix: &Index,
    st: &mut State,
    lists: &[Vec<Side>],
    mut base: Side,
    rule: &'static str,
    root: bool,
    trace: bool,
    slack_covered: usize,
    out: &mut S,
    deriver: &D,
) -> bool {
    const SIDE: usize = std::mem::size_of::<Side>();
    const PRUNE_ELEM: usize = std::mem::size_of::<(Side, u64, u64, bool)>();
    let norm = |s: &mut Side| {
        s.0.sort_unstable();
        s.0.dedup();
        s.1.sort_unstable();
        s.1.dedup();
    };
    let lits_bytes = |s: &Side| (s.0.capacity() + s.1.capacity()) * std::mem::size_of::<Lit>();
    norm(&mut base);
    // Guard is declared before `acc` (and before every step's `seen`/`next`/`drained`) so
    // LIFO drop always frees the buffers before releasing their reservation, including on
    // panic unwind from a caller callback.
    let mut guard = Guard::new(out);
    let mut acc_lits = lits_bytes(&base);
    let init_need = SIDE.saturating_add(acc_lits);
    if !guard.ensure_admitted(init_need) {
        mark_interrupted(st);
        return false;
    }
    let mut acc: Vec<Side> = vec![base];
    let last = lists.len().saturating_sub(1);
    let mut built = 0usize;
    let mut streaming = false;
    for (i, l) in lists.iter().enumerate() {
        let mut seen: FxHashSet<Side> = FxHashSet::default();
        let mut next: Vec<Side> = Vec::new();
        let mut seen_lits = 0usize;
        let mut next_lits = 0usize;
        for a in &acc {
            for t in l {
                if guard.stopped() {
                    mark_interrupted(st);
                    return false;
                }
                let Some((cand_b, cand_h, cand_bytes)) = (|| {
                    let cb = a.0.len().checked_add(t.0.len())?;
                    let ch = a.1.len().checked_add(t.1.len())?;
                    let bytes = cb
                        .checked_add(ch)?
                        .checked_mul(std::mem::size_of::<Lit>())?;
                    Some((cb, ch, bytes))
                })() else {
                    guard.data_overflow();
                    mark_interrupted(st);
                    return false;
                };
                let acc_live = acc.capacity() * SIDE + acc_lits;
                let seen_table = hash_table_bytes(seen.capacity(), SIDE);
                let next_live = next.capacity() * SIDE + next_lits;
                let Some(cand_peak) = acc_live
                    .checked_add(seen_table)
                    .and_then(|n| n.checked_add(seen_lits))
                    .and_then(|n| n.checked_add(next_live))
                    .and_then(|n| n.checked_add(cand_bytes))
                else {
                    guard.data_overflow();
                    mark_interrupted(st);
                    return false;
                };
                let cand_need = if slack_covered == 0 {
                    cand_peak
                } else {
                    let union_cand = (acc.len() + seen.len() + next.len()) * SIDE
                        + acc_lits
                        + seen_lits
                        + next_lits
                        + cand_bytes;
                    let slack_cand = cand_peak.saturating_sub(union_cand);
                    union_cand.saturating_add(slack_cand.saturating_sub(slack_covered))
                };
                if !guard.ensure_admitted(cand_need) {
                    mark_interrupted(st);
                    return false;
                }
                let mut body = Vec::with_capacity(cand_b);
                body.extend_from_slice(&a.0);
                body.extend_from_slice(&t.0);
                let mut head = Vec::with_capacity(cand_h);
                head.extend_from_slice(&a.1);
                head.extend_from_slice(&t.1);
                let mut s = (body, head);
                norm(&mut s);
                if !seen.contains(&s) {
                    let s_lits = lits_bytes(&s);
                    let cloned_lits = (s.0.len() + s.1.len()) * std::mem::size_of::<Lit>();
                    let seen_rehash = if seen.len() >= seen.capacity() {
                        let Some(b) = next_hash_table_bytes(seen.capacity(), SIDE) else {
                            guard.data_overflow();
                            mark_interrupted(st);
                            return false;
                        };
                        b
                    } else {
                        0
                    };
                    let will_push_next = i < last || (!streaming && next.len() < FOLD_PRUNE);
                    let next_grow = if will_push_next && next.len() >= next.capacity() {
                        let Some(b) =
                            next_vec_cap(next.capacity(), SIDE).and_then(|c| c.checked_mul(SIDE))
                        else {
                            guard.data_overflow();
                            mark_interrupted(st);
                            return false;
                        };
                        b
                    } else {
                        0
                    };
                    // Pre-admit both the cloned union payload and any container growth
                    // (including rehash/doubling overlap) before cloning or inserting, even
                    // when `seen` and `next` have spare capacity. `seen` rehashes before
                    // `next` grows, so their temporary table/list buffers do not overlap.
                    // When the caller already pre-reserved `fold_slack` (M-return), `slack_covered`
                    // covers table/list spare-capacity slack only, never live union bytes.
                    let seen_after = if seen_rehash > 0 {
                        seen_rehash
                    } else {
                        seen_table
                    };
                    let Some(pair_lits) = s_lits.checked_add(cloned_lits) else {
                        guard.data_overflow();
                        mark_interrupted(st);
                        return false;
                    };
                    let Some(during_seen) = acc_live
                        .checked_add(seen_table)
                        .and_then(|n| n.checked_add(seen_rehash))
                        .and_then(|n| n.checked_add(seen_lits))
                        .and_then(|n| n.checked_add(next_live))
                        .and_then(|n| n.checked_add(pair_lits))
                    else {
                        guard.data_overflow();
                        mark_interrupted(st);
                        return false;
                    };
                    let Some(during_next) = acc_live
                        .checked_add(seen_after)
                        .and_then(|n| n.checked_add(seen_lits))
                        .and_then(|n| n.checked_add(next_live))
                        .and_then(|n| n.checked_add(next_grow))
                        .and_then(|n| n.checked_add(pair_lits))
                    else {
                        guard.data_overflow();
                        mark_interrupted(st);
                        return false;
                    };
                    let peak_total = during_seen.max(during_next);
                    let peak_need = if slack_covered == 0 {
                        peak_total
                    } else {
                        let union_total = (acc.len() + seen.len() + next.len()) * SIDE
                            + acc_lits
                            + seen_lits
                            + next_lits
                            + pair_lits;
                        let peak_slack = peak_total.saturating_sub(union_total);
                        union_total.saturating_add(peak_slack.saturating_sub(slack_covered))
                    };
                    if !guard.ensure_admitted(peak_need) {
                        mark_interrupted(st);
                        return false;
                    }
                    let cloned = s.clone();
                    let actual_cloned_lits = lits_bytes(&cloned);
                    seen.insert(cloned);
                    seen_lits += actual_cloned_lits;
                    if i < last {
                        next.push(s);
                        next_lits += s_lits;
                        built += 1;
                        if built > FOLD_PRUNE {
                            guard.worked(1);
                        }
                    } else if streaming {
                        emit(ix, st, s, rule, root, trace, &mut guard, deriver);
                        if guard.stopped() {
                            mark_interrupted(st);
                            return false;
                        }
                    } else if next.len() >= FOLD_PRUNE {
                        // Switch to streaming before doubling `next` past FOLD_PRUNE: keep
                        // `drained` charged until every buffered union and `s` have been
                        // emitted and dropped, polling bounds periodically as the final
                        // emission loop does.
                        streaming = true;
                        let drained = std::mem::take(&mut next);
                        next_lits = 0;
                        for u in drained {
                            emit(ix, st, u, rule, root, trace, &mut guard, deriver);
                            if guard.stopped() {
                                mark_interrupted(st);
                                return false;
                            }
                            st.dl_choices += 1;
                            if st.dl_choices as usize >= PRODUCT_POLL {
                                st.dl_choices = 0;
                                if !guard.go_on() {
                                    mark_interrupted(st);
                                    return false;
                                }
                            }
                        }
                        emit(ix, st, s, rule, root, trace, &mut guard, deriver);
                        if guard.stopped() {
                            mark_interrupted(st);
                            return false;
                        }
                        let live_after = (acc.capacity() * SIDE + acc_lits)
                            + (hash_table_bytes(seen.capacity(), SIDE) + seen_lits);
                        let want = if slack_covered == 0 {
                            live_after
                        } else {
                            let union_after =
                                (acc.len() + seen.len()) * SIDE + acc_lits + seen_lits;
                            let slack_after = live_after.saturating_sub(union_after);
                            union_after.saturating_add(slack_after.saturating_sub(slack_covered))
                        };
                        guard.release_to(want);
                    } else {
                        next.push(s);
                        next_lits += s_lits;
                    }
                }
                st.dl_choices += 1;
                if st.dl_choices as usize >= PRODUCT_POLL {
                    st.dl_choices = 0;
                    let live_now = (acc.capacity() * SIDE + acc_lits)
                        + (hash_table_bytes(seen.capacity(), SIDE) + seen_lits)
                        + (next.capacity() * SIDE + next_lits);
                    let want = if slack_covered == 0 {
                        live_now
                    } else {
                        let union_now = (acc.len() + seen.len() + next.len()) * SIDE
                            + acc_lits
                            + seen_lits
                            + next_lits;
                        let slack_now = live_now.saturating_sub(union_now);
                        union_now.saturating_add(slack_now.saturating_sub(slack_covered))
                    };
                    let go = guard.ensure_admitted(want) && guard.go_on();
                    if !go {
                        mark_interrupted(st);
                        return false;
                    }
                }
            }
        }
        drop(seen);
        // Reserve `prune`'s temporary scratch before running `prune(&mut next)`: stable
        // `sort_by_key` uses up to `next.len() * SIDE` bytes and frees it before `keep`
        // allocates `next.len() * PRUNE_ELEM` bytes.
        if next.len() >= 2 && next.len() <= FOLD_PRUNE {
            let sort_bytes = next.len().saturating_mul(SIDE);
            let keep_bytes = next.len().saturating_mul(PRUNE_ELEM);
            let prune_scratch = sort_bytes.max(keep_bytes);
            let live_before_prune =
                (acc.capacity() * SIDE + acc_lits) + (next.capacity() * SIDE + next_lits);
            let need_prune = if slack_covered == 0 {
                live_before_prune.saturating_add(prune_scratch)
            } else {
                let union_now = (acc.len() + next.len()) * SIDE + acc_lits + next_lits;
                let extra = live_before_prune
                    .saturating_sub(union_now)
                    .saturating_add(prune_scratch);
                union_now.saturating_add(extra.saturating_sub(slack_covered))
            };
            if !guard.ensure_admitted(need_prune) {
                mark_interrupted(st);
                return false;
            }
            prune(&mut next);
        }
        acc = next;
        acc_lits = acc.iter().map(lits_bytes).sum();
        let live = acc.capacity() * SIDE + acc_lits;
        let want = if slack_covered == 0 {
            live
        } else {
            let union_now = acc.len() * SIDE + acc_lits;
            let slack_now = live.saturating_sub(union_now);
            union_now.saturating_add(slack_now.saturating_sub(slack_covered))
        };
        guard.release_to(want);
        if want.saturating_sub(guard.bytes()) >= RECORD_SLACK && !guard.ensure_admitted(want) {
            mark_interrupted(st);
            return false;
        }
    }
    for s in acc {
        emit(ix, st, s, rule, root, trace, &mut guard, deriver);
        if guard.stopped() {
            mark_interrupted(st);
            return false;
        }
        st.dl_choices += 1;
        if st.dl_choices as usize >= PRODUCT_POLL {
            st.dl_choices = 0;
            if !guard.go_on() {
                mark_interrupted(st);
                return false;
            }
        }
    }
    // `acc` has been consumed and dropped; `guard` drops here and releases its reservation.
    true
}

/// Derives one conclusion of [`fold`], with its line of the derivation trace.
#[allow(clippy::too_many_arguments)]
fn emit<S: Sink, D: Deriver>(
    ix: &Index,
    st: &mut State,
    s: Side,
    rule: &'static str,
    root: bool,
    trace: bool,
    out: &mut S,
    deriver: &D,
) {
    if trace {
        let lits = &out.dl().lits;
        eprintln!(
            "TR {} {rule} {} -> {}",
            tr_ctx(st, root),
            tr(ix, lits, &s.0),
            tr(ix, lits, &s.1)
        );
    }
    deriver.derive(ix, st, s, out);
}

/// Derives what the rules found: the single conclusions, then each product's choices in
/// the order of their lists, the last list fastest. The run's bounds are checked after
/// every [`PRODUCT_POLL`] choices the context derives, counted across calls, and a stop of
/// the run, found there or made by another worker, leaves the rest of the inference undone
/// and marks the context's store interrupted, a store made for the mark if the context has
/// none, so that the dependency record of a partial answer counts the context as open.
pub(super) fn drain<S: Sink>(ix: &Index, st: &mut State, found: Found, out: &mut S) {
    drain_with_covered(ix, st, found, out, &ContextDerive, false, 0, 0);
}

/// [`drain`], when `guard` already holds `guard.bytes()` of `found`'s `held_bytes()` (e.g.
/// pre-admitted inside `hyper_at`), so only the remaining delta is admitted here.
pub(super) fn drain_admitted<S: Sink>(
    ix: &Index,
    st: &mut State,
    found: Found,
    guard: &mut Guard<'_, S>,
) {
    let already = guard.bytes();
    drain_with_covered(ix, st, found, guard, &ContextDerive, false, 0, already);
}

/// [`drain`], when the caller has already reserved `held` and/or `slack_covered` bytes
/// (such as M-return's `m_flush` and `rret`).
pub(super) fn drain_covered<S: Sink>(
    ix: &Index,
    st: &mut State,
    found: Found,
    out: &mut S,
    held_covered: bool,
    slack_covered: usize,
) {
    drain_with_covered(
        ix,
        st,
        found,
        out,
        &ContextDerive,
        held_covered,
        slack_covered,
        0,
    );
}

/// [`drain`], with `deriver` adding each conclusion, which the root of a rooted run gives
/// its own.
#[inline(always)]
pub(super) fn drain_with<S: Sink, D: Deriver>(
    ix: &Index,
    st: &mut State,
    found: Found,
    out: &mut S,
    deriver: &D,
) {
    drain_with_covered(ix, st, found, out, deriver, false, 0, 0);
}

/// [`drain_with`], when `guard` already holds `guard.bytes()` of `found`'s `held_bytes()`.
#[inline(always)]
pub(super) fn drain_with_admitted<S: Sink, D: Deriver>(
    ix: &Index,
    st: &mut State,
    found: Found,
    guard: &mut Guard<'_, S>,
    deriver: &D,
) {
    let already = guard.bytes();
    drain_with_covered(ix, st, found, guard, deriver, false, 0, already);
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn drain_with_covered<S: Sink, D: Deriver>(
    ix: &Index,
    st: &mut State,
    found: Found,
    out: &mut S,
    deriver: &D,
    held_covered: bool,
    slack_covered: usize,
    held_admitted: usize,
) {
    debug_assert_eq!(found.extra_bytes, 0);
    if found.over {
        out.dl().exhaust_with(WorkStopGuard::ForcedExhaust);
        mark_interrupted(st);
        return;
    }
    let h = if held_covered {
        0
    } else {
        found.held_bytes().saturating_sub(held_admitted)
    };
    // Declare `held_guard` before moving `found`'s collections into locals so LIFO drop
    // always frees both `one` and `prods` before releasing their reservation, including on
    // early stop or callback panic unwind during single-conclusion delivery.
    let mut held_guard = Guard::new(out);
    let Found {
        one, prods, root, ..
    } = found;
    if h > 0 {
        if !held_guard.admit_add(h) {
            mark_interrupted(st);
            return;
        }
    } else if !held_covered && held_guard.stopped() {
        mark_interrupted(st);
        return;
    }
    let trace = tracing();
    for (c, rule) in one {
        if trace {
            let lits = &held_guard.dl().lits;
            eprintln!(
                "TR {} {rule} {} -> {}",
                tr_ctx(st, root),
                tr(ix, lits, &c.0),
                tr(ix, lits, &c.1)
            );
        }
        deriver.derive(ix, st, c, &mut held_guard);
        if held_guard.stopped() {
            mark_interrupted(st);
            return;
        }
        st.dl_choices += 1;
        if st.dl_choices as usize >= PRODUCT_POLL {
            st.dl_choices = 0;
            if !held_guard.go_on() {
                mark_interrupted(st);
                return;
            }
        }
    }
    let dx = ix.dl.as_deref();
    let inv = |r: Role| dx.and_then(|d| d.inv(ix, r));
    for (lists, base, ground, rule) in prods {
        // a product past the run's limit on choices ends the run as its bound does, before
        // the product spends its work (roadmap item 1011)
        if held_guard.dl().too_wide(&lists) {
            held_guard.dl().exhaust_with(WorkStopGuard::WidthExhausted);
            mark_interrupted(st);
            return;
        }
        if !ground {
            if !fold(
                ix,
                st,
                &lists,
                base,
                rule,
                root,
                trace,
                slack_covered,
                &mut held_guard,
                deriver,
            ) {
                return;
            }
            continue;
        }
        // Declare `guard` before `at` and `seen` so LIFO drop always frees the hash table
        // and index vector before releasing their reservation, including on panic unwind.
        let mut guard = Guard::new(&mut held_guard);
        let Some(at_bytes) = lists.len().checked_mul(std::mem::size_of::<usize>()) else {
            guard.data_overflow();
            mark_interrupted(st);
            return;
        };
        if at_bytes > 0 && !guard.ensure_admitted(at_bytes) {
            mark_interrupted(st);
            return;
        }
        let mut at = vec![0usize; lists.len()];
        // two choices whose premises share their literals merge to one conclusion, as
        // when one role premise fits two places of a Pred body or two needs share their
        // premise lists, and the product derives each conclusion once. Its record of the
        // conclusions, kept or redundant, lives until the product ends, so it is charged
        // to the memory budget as it grows and released after `seen` drops, and a
        // conclusion the ground restriction rejects is not recorded.
        let mut seen: FxHashSet<(Vec<Lit>, Vec<Lit>)> = FxHashSet::default();
        let key_size = std::mem::size_of::<(Vec<Lit>, Vec<Lit>)>();
        let mut seen_lits = 0usize;
        'choices: loop {
            if guard.stopped() {
                mark_interrupted(st);
                return;
            }
            let Some((cand_b, cand_h, cand_bytes)) = (|| {
                let mut cb = base.0.len();
                let mut ch = base.1.len();
                for (l, &i) in lists.iter().zip(&at) {
                    cb = cb.checked_add(l[i].0.len())?;
                    ch = ch.checked_add(l[i].1.len())?;
                }
                let bytes = cb
                    .checked_add(ch)?
                    .checked_mul(std::mem::size_of::<Lit>())?;
                Some((cb, ch, bytes))
            })() else {
                guard.data_overflow();
                mark_interrupted(st);
                return;
            };
            let seen_table = hash_table_bytes(seen.capacity(), key_size);
            let Some(cand_need) = at_bytes
                .checked_add(seen_table)
                .and_then(|n| n.checked_add(seen_lits))
                .and_then(|n| n.checked_add(cand_bytes))
            else {
                guard.data_overflow();
                mark_interrupted(st);
                return;
            };
            if !guard.ensure_admitted(cand_need) {
                mark_interrupted(st);
                return;
            }
            let mut body = Vec::with_capacity(cand_b);
            body.extend_from_slice(&base.0);
            let mut head = Vec::with_capacity(cand_h);
            head.extend_from_slice(&base.1);
            for (l, &i) in lists.iter().zip(&at) {
                body.extend_from_slice(&l[i].0);
                head.extend_from_slice(&l[i].1);
            }
            let mut s = (body, head);
            let mut fresh = !ground || !restricted(&guard.dl().lits, inv, &s);
            if fresh {
                s.0.sort_unstable();
                s.0.dedup();
                s.1.sort_unstable();
                s.1.dedup();
                if !seen.contains(&s) {
                    let s_lits = (s.0.capacity() + s.1.capacity()) * std::mem::size_of::<Lit>();
                    let key_lits = (s.0.len() + s.1.len()) * std::mem::size_of::<Lit>();
                    let next_table = if seen.len() >= seen.capacity() {
                        let Some(b) = next_hash_table_bytes(seen.capacity(), key_size) else {
                            guard.data_overflow();
                            mark_interrupted(st);
                            return;
                        };
                        b
                    } else {
                        0
                    };
                    // Pre-admit both the retained key payload and any rehash table overlap
                    // unconditionally, even when `seen` has spare capacity.
                    let Some(need) = at_bytes
                        .checked_add(seen_table)
                        .and_then(|n| n.checked_add(next_table))
                        .and_then(|n| n.checked_add(seen_lits))
                        .and_then(|n| n.checked_add(s_lits))
                        .and_then(|n| n.checked_add(key_lits))
                    else {
                        guard.data_overflow();
                        mark_interrupted(st);
                        return;
                    };
                    if !guard.ensure_admitted(need) {
                        mark_interrupted(st);
                        return;
                    }
                    let key = s.clone();
                    let actual_key_lits =
                        (key.0.capacity() + key.1.capacity()) * std::mem::size_of::<Lit>();
                    seen.insert(key);
                    seen_lits += actual_key_lits;
                } else {
                    fresh = false;
                }
            }
            if fresh {
                if trace {
                    let lits = &guard.dl().lits;
                    eprintln!(
                        "TR {} {rule} {} -> {}",
                        tr_ctx(st, root),
                        tr(ix, lits, &s.0),
                        tr(ix, lits, &s.1)
                    );
                }
                deriver.derive(ix, st, s, &mut guard);
                if guard.stopped() {
                    mark_interrupted(st);
                    return;
                }
            }
            st.dl_choices += 1;
            if st.dl_choices as usize >= PRODUCT_POLL {
                st.dl_choices = 0;
                let live = at_bytes + hash_table_bytes(seen.capacity(), key_size) + seen_lits;
                let go = if live > guard.bytes() {
                    guard.ensure_reserved(live)
                } else {
                    guard.go_on()
                };
                if !go {
                    // the stop can come after the last choice, and the mark then only
                    // keeps the context open
                    mark_interrupted(st);
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
        let live = at_bytes + hash_table_bytes(seen.capacity(), key_size) + seen_lits;
        if live.saturating_sub(guard.bytes()) >= RECORD_SLACK && !guard.ensure_reserved(live) {
            mark_interrupted(st);
            return;
        }
        // `seen` and `at` drop here, before `guard` drops and releases its reservation.
    }
}

struct FmtTerm(RTerm);

impl std::fmt::Display for FmtTerm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            RTerm::Fx(e) => write!(f, "f{e}(x)"),
            RTerm::Fc(e, u) => write!(f, "f{e}({u})"),
            RTerm::C(u) => write!(f, "{u}"),
            RTerm::X => f.write_str("x"),
            RTerm::Y => f.write_str("y"),
        }
    }
}

/// Formats one literal directly into `f` without heap allocation, matching [`tr`].
pub(super) fn write_lit(
    f: &mut std::fmt::Formatter<'_>,
    ix: &Index,
    lits: &Lits,
    l: Lit,
) -> std::fmt::Result {
    use super::dl_clause::XLit;
    let dx = dix(ix);
    let inv = |r| dx.inv(ix, r);
    match lits.view(l, inv) {
        XLit::Atom(a, b) => write!(f, "{:?}#{a}({})", ix.head(a).node, FmtTerm(b)),
        XLit::Role(r, a, b) => write!(f, "r{r}({}, {})", FmtTerm(a), FmtTerm(b)),
        XLit::Eq(a, b) => write!(f, "{} = {}", FmtTerm(a), FmtTerm(b)),
        XLit::Ne(a, b) => write!(f, "{} != {}", FmtTerm(a), FmtTerm(b)),
    }
}

struct FmtOneLit<'a>(&'a Index, &'a Lits, Lit);

impl std::fmt::Display for FmtOneLit<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_lit(f, self.0, self.1, self.2)
    }
}

/// The literals `ls` for the derivation trace, with each atom's node in place of its id.
pub(super) fn tr(ix: &Index, lits: &Lits, ls: &[Lit]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("[");
    for (i, &l) in ls.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{}", FmtOneLit(ix, lits, l));
    }
    out.push(']');
    out
}

/// Visits each universal restriction `(t, c)` that `d` stands for (`d ⊑ ∀t.c`) without
/// allocating a temporary vector, stopping early if `f` returns `false`.
pub(super) fn for_each_all(ix: &Index, d: Ex, mut f: impl FnMut(Role, Ex) -> bool) -> bool {
    let h = ix.head(d);
    if let Node::Forall(t, c) = h.node
        && !f(t, c)
    {
        return false;
    }
    for &(s, x) in ix.exist_of(h) {
        if let Some(i) = ix.roles.inv(s)
            && !f(i, x)
        {
            return false;
        }
    }
    true
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
pub(super) fn derive<S: Sink>(ix: &Index, st: &mut State, (mut body, mut head): Side, out: &mut S) {
    if st.bottom {
        return;
    }
    out.worked(1);
    // Only an exact empty-body unit delivered from the root via `rpred_c` in this run
    // acquires `Stored::fact = true`; a local strengthening or conditional clause does not.
    let incoming_unit_fact = out.dl().shared_facts
        && dl(st).is_some_and(|x| x.taking_fact)
        && body.is_empty()
        && head.len() == 1;
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
    let cut = dx.order;
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
        // with the order between number restrictions, an atom disjoint with a unit of
        // the context cannot hold: the Hyper conclusion with the unit and the
        // disjointness, which subsumes the clause
        X if cut
            && ix
                .disjoint_of(ix.head(l.atom()))
                .iter()
                .any(|set| st.disj.get(set).is_some_and(|&m| m != l.atom())) =>
        {
            false
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
    // a rooted run's bodies also hold ground literals, and its equalities and inequalities
    // are tautologies with counting on or off
    if dx.root.is_some()
        && (head
            .iter()
            .any(|l| l.kind() == XL && body.binary_search(l).is_ok())
            || out.dl().lits.complementary(&head, |r| dx.inv(ix, r)))
    {
        return;
    }
    if dx.counting && equality_tautology(&head) {
        return;
    }
    // a hypothetical successor's conclusion with an empty body whose literals hold neither
    // x nor y holds in every model, since the context's clauses hold of every element
    // under no assumption, so it goes to the root as a fact, once, and comes back as the
    // root's facts do, to the contexts registered for its constants; the root's own facts
    // stay here, and the empty clause takes its own path below, which makes this context
    // unsatisfiable and sends the empty clause to its predecessors
    if let Some(root) = dx.root
        && promote()
        && body.is_empty()
        && !head.is_empty()
        && dl(st).is_some_and(|x| x.empty_core && !x.taking_fact && x.neg.is_empty())
        && head
            .iter()
            .all(|&l| l.kind() == XL && no_xy(out.dl().lits.view(l, |r| dx.inv(ix, r))))
    {
        let Some(x) = ds_admit(st, out) else {
            return;
        };
        if !x.promoted.contains(head.as_slice()) {
            let entry = std::mem::size_of::<Box<[Lit]>>();
            let map_grow = if x.promoted.len() >= x.promoted.capacity() {
                next_hash_table_bytes(x.promoted.capacity(), entry)
            } else {
                Some(0)
            };
            let payload = head.len().checked_mul(std::mem::size_of::<Lit>());
            let Some(bound) = map_grow.and_then(|m| payload.and_then(|p| m.checked_add(p))) else {
                out.data_overflow();
                x.store.interrupted = true;
                return;
            };
            if bound > 0 && !out.admit_dl(bound) {
                x.store.interrupted = true;
                return;
            }
            let before = x.promoted.capacity();
            if x.promoted.insert(head.clone().into_boxed_slice()) {
                let delta = hash_table_bytes(x.promoted.capacity(), entry)
                    .saturating_sub(hash_table_bytes(before, entry))
                    + head.len() * std::mem::size_of::<Lit>();
                if bound > 0 {
                    out.commit_dl(bound, delta);
                }
                let cid = out.dl().table.push(Clause::new(Vec::new(), head));
                out.send(root, Concl::RFact(cid));
            } else if bound > 0 {
                out.commit_dl(bound, 0);
            }
        }
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
            out.worked(read / WORK_READS);
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
            if let Some(x) = ds_admit(st, out) {
                let _ = x.store.add_parts_admitted(body, h, out);
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
    if let Some(x) = ds_admit(st, out) {
        let unit_fact = incoming_unit_fact && body.is_empty() && head.len() == 1;
        let _ = x
            .store
            .add_parts_admitted_with_fact(body, head, unit_fact, out);
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
fn again<S: Sink>(
    ix: &Index,
    st: &State,
    id: u32,
    c: &Clause,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    found.rule = "again";
    let Some(d) = dl(st) else { return true };
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
                let Some(bytes) = side_exact_bytes(s.c.body().len(), s.c.head().len()) else {
                    guard.data_overflow();
                    return false;
                };
                if !found.push_admitted(bytes, || (s.c.body().to_vec(), s.c.head().to_vec()), guard)
                {
                    return false;
                }
            }
        }
    }
    true
}

/// The clauses read that count as one derived conclusion for the work bound.
pub(super) const WORK_READS: usize = 16;

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
/// `d`: the other atoms' premises, and the head as literals over x. Pre-admits `Found::prods`
/// growth, `lists`/`base` payloads, and `prune` scratch before constructing any partner list.
fn hyper_clause<S: Sink>(
    ix: &Index,
    st: &State,
    id: u32,
    d: Ex,
    prem: &Side,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    found.rule = "hyper";
    let (body, head) = &dix(ix).clauses[id as usize];
    let empty_side = unit();
    let Some(mut lists_bytes) = body.len().checked_mul(std::mem::size_of::<Vec<Side>>()) else {
        guard.data_overflow();
        return false;
    };
    let mut max_list_len = 0usize;
    let mut overflow = false;
    for &a in body.iter() {
        let mut count = 0usize;
        let mut sides_bytes = 0usize;
        if a == d {
            count = 1;
            let Some(b) = side_exact_bytes(prem.0.len(), prem.1.len()) else {
                overflow = true;
                break;
            };
            sides_bytes = b;
        } else {
            for_each_partner_x(st, a, |pb, ph, without| {
                let Some(sb) = side_parts_bytes(&empty_side, pb, ph, without, None)
                    .and_then(|b| sides_bytes.checked_add(b))
                else {
                    overflow = true;
                    return false;
                };
                let Some(nc) = count.checked_add(1) else {
                    overflow = true;
                    return false;
                };
                count = nc;
                sides_bytes = sb;
                true
            });
            if overflow {
                break;
            }
        }
        if count == 0 {
            return true;
        }
        max_list_len = max_list_len.max(count);
        let Some(pos_bytes) = count
            .checked_mul(std::mem::size_of::<Side>())
            .and_then(|b| b.checked_add(sides_bytes))
            .and_then(|b| lists_bytes.checked_add(b))
        else {
            overflow = true;
            break;
        };
        lists_bytes = pos_bytes;
    }
    if overflow {
        guard.data_overflow();
        return false;
    }
    let Some(base_bytes) = side_exact_bytes(0, head.len()) else {
        guard.data_overflow();
        return false;
    };
    let Some(prod_bytes) = lists_bytes.checked_add(base_bytes) else {
        guard.data_overflow();
        return false;
    };
    let prune_scratch = if max_list_len < 2 {
        0
    } else {
        let Some(ps) = max_list_len.checked_mul(PRUNE_SCRATCH_ELEM) else {
            guard.data_overflow();
            return false;
        };
        ps
    };
    found.push_product_admitted(
        prod_bytes,
        prune_scratch,
        0,
        false,
        || {
            let mut lists = Vec::with_capacity(body.len());
            for &a in body.iter() {
                if a == d {
                    lists.push(vec![(prem.0.clone(), prem.1.clone())]);
                } else {
                    let mut count = 0usize;
                    for_each_partner_x(st, a, |_, _, _| {
                        count += 1;
                        true
                    });
                    let mut v = Vec::with_capacity(count);
                    for_each_partner_x(st, a, |pb, ph, without| {
                        v.push(merge_parts(&empty_side, pb, ph, without, None));
                        true
                    });
                    lists.push(v);
                }
            }
            let mut h_lits = Vec::with_capacity(head.len());
            for &h in head.iter() {
                h_lits.push(Lit::x(h));
            }
            (lists, (Vec::new(), h_lits))
        },
        guard,
    )
}

/// The universals of the holders over the successor term f, which x reaches by r: the
/// units' through the propagations, and the stored holders', each with `prem`, the premise
/// of `r(x, f(x))`.
fn univ_over<S: Sink>(
    ix: &Index,
    st: &State,
    f: Ex,
    r: Role,
    prem: &Side,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    found.rule = "univ";
    let roles = &ix.roles;
    let dx = dix(ix);
    if r >= dx.base {
        return true;
    }
    for &(s, x) in &st.props {
        if let Some(t) = roles.inv(s)
            && (dx.counting || dx.reaches(t))
            && dx.carries(t)
            && roles.sub(r, t)
        {
            let Some(bytes) = side_parts_bytes(prem, &[], &[], None, Some(Lit::f(f, x))) else {
                guard.data_overflow();
                return false;
            };
            if !found.push_admitted(bytes, || with_exact(prem, Lit::f(f, x)), guard) {
                return false;
            }
        }
    }
    let Some(d) = dl(st) else { return true };
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
                // the inference is local to the context, so a selected holder takes part
                for i in d.store.with_x(b) {
                    let cl = &d.store.clauses[i as usize].c;
                    let w = Some(Lit::x(b));
                    let add = Some(Lit::f(f, c));
                    let Some(bytes) = side_parts_bytes(prem, cl.body(), cl.head(), w, add) else {
                        guard.data_overflow();
                        return false;
                    };
                    if !found.push_admitted(
                        bytes,
                        || merge_parts(prem, cl.body(), cl.head(), w, add),
                        guard,
                    ) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// Borrowed holder premise source for [`at_most`], avoiding eager `Vec<Side>` allocation
/// before admission.
#[derive(Clone, Copy)]
enum Holder<'a> {
    Class(Ex),
    Unit,
    Prem(&'a Side),
}

fn for_each_holder(
    st: &State,
    holder: Holder<'_>,
    mut cb: impl FnMut(&[Lit], &[Lit], Option<Lit>) -> bool,
) -> bool {
    match holder {
        Holder::Class(m) => for_each_partner_x(st, m, cb),
        Holder::Unit => cb(&[], &[], None),
        Holder::Prem(p) => cb(&p.0, &p.1, None),
    }
}

/// The at-most restriction `≤n q` of a holder: each choice of n+1 different neighbour
/// terms, among the predecessor and the successor terms that x reaches by a role below q,
/// with a premise for each and for the holder, gives the equalities of every pair (DL4).
/// The predecessor is a neighbour through the body atom `q(x, y)`. `fixed` is a new premise
/// of one successor term, which the inference must use, and `holder` the holder's borrowed
/// premise source. Compact candidate metadata buffers (`refs`, `cands`) are pre-admitted
/// before allocation and remain covered for their full lifetime across product reservations
/// and prune.
#[allow(clippy::too_many_arguments)]
fn at_most<S: Sink>(
    ix: &Index,
    st: &State,
    n: u32,
    q: Role,
    holder: Holder<'_>,
    fixed: Option<(Ex, &Side)>,
    extra: &[Ex],
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    use std::mem::size_of;
    enum CandKind {
        Y(Role),
        Unit,
        Fixed,
        Refs(usize, usize),
    }
    struct AtMostCand {
        term: Term,
        kind: CandKind,
        count: usize,
        bytes: usize,
    }

    found.rule = if extra.is_empty() { "atmost" } else { "lazy" };
    let dx = dix(ix);
    let empty_side = unit();

    // 1. Count holder premises directly from borrowed storage with zero heap allocations.
    let mut holder_count = 0usize;
    let mut holder_sides_bytes = 0usize;
    let mut overflow = false;
    for_each_holder(st, holder, |pb, ph, without| {
        let Some(sb) = side_parts_bytes(&empty_side, pb, ph, without, None)
            .and_then(|b| holder_sides_bytes.checked_add(b))
        else {
            overflow = true;
            return false;
        };
        let Some(nc) = holder_count.checked_add(1) else {
            overflow = true;
            return false;
        };
        holder_count = nc;
        holder_sides_bytes = sb;
        true
    });
    if overflow {
        guard.data_overflow();
        return false;
    }
    if holder_count == 0 {
        return true;
    }
    let Some(holder_bytes) = holder_count
        .checked_mul(size_of::<Side>())
        .and_then(|b| b.checked_add(holder_sides_bytes))
    else {
        guard.data_overflow();
        return false;
    };

    let fixed_bytes = if let Some((_, p)) = fixed {
        let Some(b) =
            side_exact_bytes(p.0.len(), p.1.len()).and_then(|sb| sb.checked_add(size_of::<Side>()))
        else {
            guard.data_overflow();
            return false;
        };
        b
    } else {
        0
    };

    // 2. Count unit successor terms and non-unit stored role clause references (0 allocs).
    let has_y = dx.inv(ix, q).filter(|&i| dx.carries(i));
    let rf_possible = !((q < dx.base && !dx.reaches(q) && !dx.counting) || !dx.carries(q));
    let is_unit_rf = |e: Ex| -> bool {
        q < dx.base
            && st.subs.contains(&e)
            && role_of(ix, e).is_some_and(|r| ix.roles.sub(r, q))
            && into_nominal(ix, e).is_none()
    };

    let mut units_count = 0usize;
    let mut fixed_in_units = false;
    if rf_possible && q < dx.base {
        let below = dx.below.get(q as usize).map_or(&[][..], |v| v.as_slice());
        if below.len() <= st.subs.len() {
            for &e in below {
                if st.subs.contains(&e) && into_nominal(ix, e).is_none() {
                    units_count += 1;
                    if fixed.is_some_and(|(g, _)| g == e) {
                        fixed_in_units = true;
                    }
                }
            }
        } else {
            for &e in &st.subs {
                if role_of(ix, e).is_some_and(|r| ix.roles.sub(r, q))
                    && into_nominal(ix, e).is_none()
                {
                    units_count += 1;
                    if fixed.is_some_and(|(g, _)| g == e) {
                        fixed_in_units = true;
                    }
                }
            }
        }
    }

    let mut refs_len = 0usize;
    let mut fixed_in_refs = false;
    if rf_possible && let Some(d) = dl(st) {
        for (&s, v) in &d.store.by_rl {
            if dx.sub(ix, s, q) {
                for &i in v {
                    if d.store.clauses[i as usize].dead {
                        continue;
                    }
                    let Some(l) = d.store.max_of(i, |l| l.kind() == RL) else {
                        continue;
                    };
                    let f = l.fun();
                    if is_unit_rf(f) {
                        continue;
                    }
                    if fixed.is_some_and(|(g, _)| g == f) {
                        fixed_in_refs = true;
                        continue;
                    }
                    let Some(nr) = refs_len.checked_add(1) else {
                        guard.data_overflow();
                        return false;
                    };
                    refs_len = nr;
                }
            }
        }
    }

    // 3. Admit and build exact-capacity `refs` metadata buffer (12/16 B per clause, no `Side`s).
    let Some(refs_bytes) = refs_len.checked_mul(size_of::<(Ex, u32, Lit)>()) else {
        guard.data_overflow();
        return false;
    };
    let Some(need_refs) = found.held_bytes().checked_add(refs_bytes) else {
        guard.data_overflow();
        return false;
    };
    if !guard.ensure_admitted(need_refs) {
        return false;
    }
    let mut refs: Vec<(Ex, u32, Lit)> = Vec::with_capacity(refs_len);
    if refs_len > 0
        && let Some(d) = dl(st)
    {
        for (&s, v) in &d.store.by_rl {
            if dx.sub(ix, s, q) {
                for &i in v {
                    if d.store.clauses[i as usize].dead {
                        continue;
                    }
                    let Some(l) = d.store.max_of(i, |l| l.kind() == RL) else {
                        continue;
                    };
                    let f = l.fun();
                    if is_unit_rf(f) || fixed.is_some_and(|(g, _)| g == f) {
                        continue;
                    }
                    refs.push((f, i, l));
                }
            }
        }
        refs.sort_unstable();
    }
    debug_assert_eq!(refs.len(), refs_len);
    debug_assert_eq!(refs.capacity(), refs_len);

    // 4. Count exact candidate terms and check bounds BEFORE allocating `cands`.
    let mut distinct_ref_fs = 0usize;
    let mut prev_f: Option<Ex> = None;
    for &(f, _, _) in &refs {
        if prev_f != Some(f) {
            distinct_ref_fs += 1;
            prev_f = Some(f);
        }
    }
    let fixed_extra = usize::from(fixed.is_some() && !fixed_in_units);
    let cand_len = usize::from(has_y.is_some()) + units_count + distinct_ref_fs + fixed_extra;
    let k = n as usize + 1;
    if cand_len < k {
        drop(refs);
        guard.release_to(found.held_bytes());
        return true;
    }
    let subsets = match fixed {
        Some(_) => choose(cand_len - 1, k - 1),
        None => choose(cand_len, k),
    };
    let payload = subsets.saturating_mul(k * (k - 1) / 2 + k + 1);
    if k > AT_MOST_WIDEST || subsets > AT_MOST_SUBSETS || payload > AT_MOST_PAYLOAD {
        if tracing() {
            eprintln!(
                "TR OVER {} q={q} k={k} cands={cand_len} subsets={subsets} payload={payload}",
                tr_ctx(st, false),
            );
        }
        found.over = true;
        drop(refs);
        guard.release_to(found.held_bytes());
        return true;
    }

    // 5. Admit and build exact-capacity `cands` metadata buffer; keep `setup_bytes` admitted
    //    until both `cands` and `refs` are dropped.
    let Some(cands_bytes) = cand_len.checked_mul(size_of::<AtMostCand>()) else {
        drop(refs);
        guard.release_to(found.held_bytes());
        guard.data_overflow();
        return false;
    };
    let Some(setup_bytes) = refs_bytes.checked_add(cands_bytes) else {
        drop(refs);
        guard.release_to(found.held_bytes());
        guard.data_overflow();
        return false;
    };
    let Some(need_setup) = found.held_bytes().checked_add(setup_bytes) else {
        drop(refs);
        guard.release_to(found.held_bytes());
        guard.data_overflow();
        return false;
    };
    if !guard.ensure_admitted(need_setup) {
        drop(refs);
        guard.release_to(found.held_bytes());
        return false;
    }

    let mut cands: Vec<AtMostCand> = Vec::with_capacity(cand_len);
    if let Some(i) = has_y {
        cands.push(AtMostCand {
            term: Term::Y,
            kind: CandKind::Y(i),
            count: 1,
            bytes: size_of::<Side>() + size_of::<Lit>(),
        });
    }
    let fun_start = cands.len();
    if rf_possible && q < dx.base {
        let below = dx.below.get(q as usize).map_or(&[][..], |v| v.as_slice());
        let mut push_unit = |e: Ex| {
            if fixed.is_some_and(|(g, _)| g == e) {
                cands.push(AtMostCand {
                    term: Term::Fun(e),
                    kind: CandKind::Fixed,
                    count: 1,
                    bytes: fixed_bytes,
                });
            } else {
                cands.push(AtMostCand {
                    term: Term::Fun(e),
                    kind: CandKind::Unit,
                    count: 1,
                    bytes: size_of::<Side>(),
                });
            }
        };
        if below.len() <= st.subs.len() {
            for &e in below {
                if st.subs.contains(&e) && into_nominal(ix, e).is_none() {
                    push_unit(e);
                }
            }
        } else {
            for &e in &st.subs {
                if role_of(ix, e).is_some_and(|r| ix.roles.sub(r, q))
                    && into_nominal(ix, e).is_none()
                {
                    push_unit(e);
                }
            }
        }
    }
    if let Some((g, _)) = fixed
        && fixed_in_refs
        && !fixed_in_units
    {
        cands.push(AtMostCand {
            term: Term::Fun(g),
            kind: CandKind::Fixed,
            count: 1,
            bytes: fixed_bytes,
        });
    }
    if !refs.is_empty()
        && let Some(d) = dl(st)
    {
        let mut start = 0usize;
        while start < refs.len() {
            let f = refs[start].0;
            let mut end = start + 1;
            while end < refs.len() && refs[end].0 == f {
                end += 1;
            }
            let count = end - start;
            let mut sides_bytes = 0usize;
            let mut group_overflow = false;
            for &(_, ci, l) in &refs[start..end] {
                let cl = &d.store.clauses[ci as usize].c;
                let Some(sb) = side_parts_bytes(&empty_side, cl.body(), cl.head(), Some(l), None)
                    .and_then(|b| sides_bytes.checked_add(b))
                else {
                    group_overflow = true;
                    break;
                };
                sides_bytes = sb;
            }
            let group_bytes = if group_overflow {
                None
            } else {
                count
                    .checked_mul(size_of::<Side>())
                    .and_then(|b| b.checked_add(sides_bytes))
            };
            let Some(bytes) = group_bytes else {
                drop(cands);
                drop(refs);
                guard.release_to(found.held_bytes());
                guard.data_overflow();
                return false;
            };
            cands.push(AtMostCand {
                term: Term::Fun(f),
                kind: CandKind::Refs(start, end),
                count,
                bytes,
            });
            start = end;
        }
    }
    cands[fun_start..].sort_unstable_by_key(|c| match c.term {
        Term::Fun(f) => f,
        Term::Y => Ex::MAX,
    });
    let must = if let Some((g, _)) = fixed {
        if let Some(pos) = cands[fun_start..]
            .iter()
            .position(|c| c.term == Term::Fun(g))
        {
            Some(fun_start + pos)
        } else {
            cands.push(AtMostCand {
                term: Term::Fun(g),
                kind: CandKind::Fixed,
                count: 1,
                bytes: fixed_bytes,
            });
            Some(cands.len() - 1)
        }
    } else {
        None
    };
    debug_assert_eq!(cands.len(), cand_len);
    debug_assert_eq!(cands.capacity(), cand_len);

    // 6. Enumerate k-subsets with stack-allocated `pick`, admitting each product with
    //    `setup_bytes` live and materializing premise lists ONLY after slot admission.
    let mut pick = [0usize; AT_MOST_WIDEST];
    for (idx, slot) in pick[..k].iter_mut().enumerate() {
        *slot = idx;
    }
    let mut ok = true;
    loop {
        let cur = &pick[..k];
        if must.is_none_or(|m| cur.contains(&m)) {
            let empty_cand = cur.iter().any(|&i| cands[i].count == 0);
            if !empty_cand {
                let mut eqs_len = extra.len();
                let mut max_len = holder_count;
                let mut prod_bytes = (1 + k)
                    .checked_mul(size_of::<Vec<Side>>())
                    .and_then(|b| b.checked_add(holder_bytes));
                for (a, &i) in cur.iter().enumerate() {
                    max_len = max_len.max(cands[i].count);
                    prod_bytes = prod_bytes.and_then(|b| b.checked_add(cands[i].bytes));
                    for &j in &cur[a + 1..] {
                        if let Made::Lit(_) = Lit::eq(cands[i].term, cands[j].term) {
                            eqs_len += 1;
                        }
                    }
                }
                let Some(prod_bytes) =
                    prod_bytes.and_then(|b| b.checked_add(side_exact_bytes(0, eqs_len)?))
                else {
                    guard.data_overflow();
                    ok = false;
                    break;
                };
                let prune_scratch = if max_len < 2 {
                    0
                } else {
                    let Some(ps) = max_len.checked_mul(PRUNE_SCRATCH_ELEM) else {
                        guard.data_overflow();
                        ok = false;
                        break;
                    };
                    ps
                };
                if !found.push_product_admitted(
                    prod_bytes,
                    prune_scratch,
                    setup_bytes,
                    false,
                    || {
                        let mut lists: Vec<Vec<Side>> = Vec::with_capacity(1 + k);
                        let mut h_list = Vec::with_capacity(holder_count);
                        for_each_holder(st, holder, |pb, ph, without| {
                            h_list.push(merge_parts(&empty_side, pb, ph, without, None));
                            true
                        });
                        lists.push(h_list);
                        let mut eqs: Vec<Lit> = Vec::with_capacity(eqs_len);
                        eqs.extend(extra.iter().map(|&a| Lit::x(a)));
                        let store = dl(st).map(|d| &d.store);
                        for (a, &i) in cur.iter().enumerate() {
                            let cand = &cands[i];
                            let list = match cand.kind {
                                CandKind::Y(inv_q) => vec![(vec![Lit::ry(inv_q)], Vec::new())],
                                CandKind::Unit => vec![unit()],
                                CandKind::Fixed => {
                                    let (_, p) = fixed.expect("fixed candidate");
                                    vec![(p.0.clone(), p.1.clone())]
                                }
                                CandKind::Refs(start, end) => {
                                    let store = store.expect("dl store for role refs");
                                    let mut v = Vec::with_capacity(end - start);
                                    for &(_, ci, l) in &refs[start..end] {
                                        let cl = &store.clauses[ci as usize].c;
                                        v.push(merge_parts(
                                            &empty_side,
                                            cl.body(),
                                            cl.head(),
                                            Some(l),
                                            None,
                                        ));
                                    }
                                    v
                                }
                            };
                            lists.push(list);
                            for &j in &cur[a + 1..] {
                                if let Made::Lit(l) = Lit::eq(cands[i].term, cands[j].term) {
                                    eqs.push(l);
                                }
                            }
                        }
                        (lists, (Vec::new(), eqs))
                    },
                    guard,
                ) {
                    ok = false;
                    break;
                }
            }
        }
        // the next subset in lexicographic order
        let m = cands.len();
        let mut i = k;
        let mut done = false;
        loop {
            if i == 0 {
                done = true;
                break;
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
        if done {
            break;
        }
    }
    drop(cands);
    drop(refs);
    guard.release_to(found.held_bytes());
    ok
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
fn on_rl<S: Sink>(
    ix: &Index,
    st: &State,
    f: Ex,
    s: Role,
    prem: &Side,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    found.rule = "onrl";
    let dx = dix(ix);
    if !univ_over(ix, st, f, s, prem, found, guard) {
        return false;
    }
    if !dx.counting {
        return true;
    }
    for &(m, n, q) in &dx.atmosts {
        if dx.sub(ix, s, q)
            && !at_most(
                ix,
                st,
                n,
                q,
                Holder::Class(m),
                Some((f, prem)),
                &[],
                found,
                guard,
            )
        {
            return false;
        }
    }
    // item 728's clauses, with the new premise fixed in the product
    for (n, q, head) in &dx.lazy {
        if dx.sub(ix, s, *q)
            && !at_most(
                ix,
                st,
                *n,
                *q,
                Holder::Unit,
                Some((f, prem)),
                head,
                found,
                guard,
            )
        {
            return false;
        }
    }
    found.rule = "qual";
    for (k, &(r, b)) in dx.quals.iter().enumerate() {
        let q = dx.base + 2 * k as Role;
        if let Some(i) = dx.inv(ix, r)
            && dx.sub(ix, s, i)
            && !for_each_partner_x(st, b, |pb, ph, without| {
                let add = Some(Lit::rl(f, q ^ 1));
                let Some(bytes) = side_parts_bytes(prem, pb, ph, without, add) else {
                    guard.data_overflow();
                    return false;
                };
                found.push_admitted(bytes, || merge_parts(prem, pb, ph, without, add), guard)
            })
        {
            return false;
        }
    }
    found.rule = "rdis";
    for &(a, b) in &dx.rdis {
        if dx.sub(ix, s, a)
            && !for_each_partner_rl(ix, dx, st, f, b, |pb, ph, without| {
                let Some(bytes) = side_parts_bytes(prem, pb, ph, without, None) else {
                    guard.data_overflow();
                    return false;
                };
                found.push_admitted(bytes, || merge_parts(prem, pb, ph, without, None), guard)
            })
        {
            return false;
        }
    }
    true
}

/// The rules of a new premise `prem` of `b(x)` for the qualified roles `S_B` with B = b:
/// `S(z, x) ∧ b(x) → S_B(z, x)` with z ↦ y, which the body atom `S(y, x)` carries, and
/// with z ↦ f(x) for each premise of `S(f(x), x)`.
fn on_filler<S: Sink>(
    ix: &Index,
    st: &State,
    b: Ex,
    prem: &Side,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    found.rule = "filler";
    let dx = dix(ix);
    let Some(qs) = dx.quals_of.get(&b) else {
        return true;
    };
    for &q in qs {
        let r = dx.quals[((q - dx.base) / 2) as usize].0;
        if dx.carries(r) {
            let Some(bytes) = side_exact_bytes(prem.0.len() + 1, prem.1.len() + 1) else {
                guard.data_overflow();
                return false;
            };
            if !found.push_admitted(
                bytes,
                || {
                    let mut pb = Vec::with_capacity(prem.0.len() + 1);
                    pb.extend_from_slice(&prem.0);
                    pb.push(Lit::ry(r));
                    let mut ph = Vec::with_capacity(prem.1.len() + 1);
                    ph.extend_from_slice(&prem.1);
                    ph.push(Lit::ry(q));
                    (pb, ph)
                },
                guard,
            ) {
                return false;
            }
        }
        if let Some(i) = dx.inv(ix, r) {
            for (f, ps) in partners_rf(ix, st, i) {
                for p in ps {
                    let add = Lit::rl(f, q ^ 1);
                    let Some(bytes) = side_parts_bytes(prem, &p.0, &p.1, None, Some(add)) else {
                        guard.data_overflow();
                        return false;
                    };
                    if !found.push_admitted(bytes, || merge_with_exact(prem, &p, add), guard) {
                        return false;
                    }
                }
            }
        }
    }
    true
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
#[allow(clippy::too_many_arguments)]
fn paramodulate<S: Sink>(
    ix: &Index,
    dx: &DlIx,
    c1: &Side,
    t: Term,
    c2: &Side,
    l2: Lit,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    found.rule = "param";
    match rewrite(ix, dx, l2, t) {
        Some(Made::Lit(l)) => {
            let Some(bytes) = side_parts_bytes(c1, &c2.0, &c2.1, None, Some(l)) else {
                guard.data_overflow();
                return false;
            };
            found.push_admitted(bytes, || merge_with_exact(c1, c2, l), guard)
        }
        Some(Made::False) => {
            let Some(bytes) = side_parts_bytes(c1, &c2.0, &c2.1, None, None) else {
                guard.data_overflow();
                return false;
            };
            found.push_admitted(bytes, || merge_exact(c1, c2), guard)
        }
        Some(Made::True) | None => true,
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
fn on_eq<S: Sink>(
    ix: &Index,
    st: &State,
    me: Option<u32>,
    l: Lit,
    prem: &Side,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    found.rule = "oneq";
    let dx = dix(ix);
    let f = l.fun();
    let t = l.term();
    let mut us = Vec::new();
    unit_lits(ix, dx, st, f, &mut us);
    for &u in &us {
        if !paramodulate(ix, dx, prem, t, &unit(), u, found, guard) {
            return false;
        }
    }
    if let Some(d) = dl(st) {
        for (i, l2) in d.store.over(f) {
            if Some(i) == me {
                continue;
            }
            let c2 = side(&d.store.clauses[i as usize].c, l2);
            if !paramodulate(ix, dx, prem, t, &c2, l2, found, guard) {
                return false;
            }
        }
    }
    // Factor: Γ → Δ ∨ f ≈ u ∨ f ≈ t gives Γ → Δ ∨ u ≉ t ∨ f ≈ t
    for &o in &prem.1 {
        if o.kind() == EQ && o.fun() == f {
            let extra_lit = match Lit::ne(o.term(), t) {
                Made::Lit(n) => Some(n),
                Made::False => None,
                Made::True => continue,
            };
            let h_len =
                prem.1.iter().filter(|&&m| m != o).count() + 1 + usize::from(extra_lit.is_some());
            let Some(bytes) = side_exact_bytes(prem.0.len(), h_len) else {
                guard.data_overflow();
                return false;
            };
            if !found.push_admitted(
                bytes,
                || {
                    let mut h = Vec::with_capacity(h_len);
                    h.extend(prem.1.iter().copied().filter(|&m| m != o));
                    h.push(l);
                    if let Some(n) = extra_lit {
                        h.push(n);
                    }
                    (prem.0.clone(), h)
                },
                guard,
            ) {
                return false;
            }
        }
    }
    true
}

/// The Eq rule of a new premise `prem` with the maximal literal `l2` over f(x), not an
/// equality: each premise with a maximal equality of f(x) rewrites it.
fn into_eq<S: Sink>(
    ix: &Index,
    st: &State,
    me: Option<u32>,
    l2: Lit,
    prem: &Side,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    found.rule = "intoeq";
    let dx = dix(ix);
    let Some(d) = dl(st) else { return true };
    for (i, l) in d.store.over(l2.fun()) {
        if l.kind() != EQ || Some(i) == me {
            continue;
        }
        let c1 = side(&d.store.clauses[i as usize].c, l);
        if !paramodulate(ix, dx, &c1, l.term(), prem, l2, found, guard) {
            return false;
        }
    }
    true
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
    if d == me && tracing() {
        // the trace's context label joined to the context's id
        eprintln!(
            "TR CTX {} me={me} {}",
            tr_ctx(st, false),
            tr(ix, &out.dl().lits, &[Lit::x(me)])
        );
    }
    let dx = dix(ix);
    let roles = &ix.roles;
    let mut found_guard = Guard::new(out);
    let mut found = Found::default();
    let unit = unit();
    if let Some(ids) = dx.by_body.get(&d) {
        for &id in ids {
            if !hyper_clause(ix, st, id, d, &unit, &mut found, &mut found_guard) {
                mark_interrupted(st);
                return;
            }
        }
    }
    let h = ix.head(d);
    if dl(st).is_some() {
        // the stored clauses meet d in conjunctions and disjointness
        for &(p, k) in ix.conj_of(h) {
            if !st.subs.contains(&p)
                && !for_each_partner_x(st, p, |pb, ph, without| {
                    let add = Some(Lit::x(k));
                    let Some(bytes) = side_parts_bytes(&unit, pb, ph, without, add) else {
                        found_guard.data_overflow();
                        return false;
                    };
                    found.push_admitted(
                        bytes,
                        || merge_parts(&unit, pb, ph, without, add),
                        &mut found_guard,
                    )
                })
            {
                mark_interrupted(st);
                return;
            }
        }
        for &set in ix.disjoint_of(h) {
            for &m in &dx.sets[set as usize] {
                if m != d
                    && !st.subs.contains(&m)
                    && !for_each_partner_x(st, m, |pb, ph, without| {
                        let Some(bytes) = side_parts_bytes(&unit, pb, ph, without, None) else {
                            found_guard.data_overflow();
                            return false;
                        };
                        found.push_admitted(
                            bytes,
                            || merge_parts(&unit, pb, ph, without, None),
                            &mut found_guard,
                        )
                    })
                {
                    mark_interrupted(st);
                    return;
                }
            }
        }
        // the stored clauses that d, or the successor by d, satisfies are redundant
        let x = ds(st);
        x.store.kill_head(Lit::x(d));
        if let Node::Exist(r, _) = h.node {
            x.store.kill_head(Lit::rl(d, r));
            for &s in roles.sup.get(r as usize).map_or(&[][..], |v| v.as_slice()) {
                if s != r {
                    x.store.kill_head(Lit::rl(d, s));
                }
            }
            if let Some(core) = dx.cores.get(&d) {
                for &a in core.iter() {
                    x.store.kill_head(Lit::f(d, a));
                }
            }
        }
    }
    // a universal: the propagation over the backward links, as for a negative
    // existential restriction
    if let Node::Forall(t, c) = h.node
        && let Some(i) = roles.inv(t)
    {
        st.props.push((i, c));
        if !found_guard.defer() {
            for (r, srcs) in &st.back {
                if roles.sub(*r, i) {
                    for &e in &srcs.list {
                        found_guard.send(e, Concl::Sub(c));
                    }
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
                let add = Lit::f(e, c);
                let Some(bytes) = side_parts_bytes(&p, &[], &[], None, Some(add)) else {
                    found_guard.data_overflow();
                    mark_interrupted(st);
                    return;
                };
                if !found.push_admitted(bytes, || with_exact(&p, add), &mut found_guard) {
                    mark_interrupted(st);
                    return;
                }
            }
        }
        // over the registered predecessors, and with counting the backward links whose
        // role is not below t⁻, whose sources can hold other roles over this context
        if let Some(i) = roles.inv(t) {
            props_to_dpreds(ix, me, st, i, c, &mut found_guard);
        }
    }
    if dx.counting {
        if let Some(&(n, q)) = dx.atmost.get(&d)
            && !at_most(
                ix,
                st,
                n,
                q,
                Holder::Unit,
                None,
                &[],
                &mut found,
                &mut found_guard,
            )
        {
            mark_interrupted(st);
            return;
        }
        if !on_filler(ix, st, d, &unit, &mut found, &mut found_guard) {
            mark_interrupted(st);
            return;
        }
    }
    // an existential restriction: its role and core over the new successor term are
    // premises in place
    // a restriction into a singleton has no successor term (item 822)
    if let Node::Exist(r, _) = h.node
        && into_nominal(ix, d).is_none()
    {
        if !on_rl(ix, st, d, r, &unit, &mut found, &mut found_guard) {
            mark_interrupted(st);
            return;
        }
        if late_rules(dx) {
            late(ix, me, st, d, h.succ, "unit", &mut found, &mut found_guard);
            if found_guard.stopped() {
                mark_interrupted(st);
                return;
            }
            // a restriction that had an edge before it became a unit reaches that
            // successor too, now with the unit's premises
            let w = dl(st).and_then(|x| x.edge_target(d));
            if let Some(w) = w.filter(|&w| w != h.succ) {
                late(ix, me, st, d, w, "unit-edge", &mut found, &mut found_guard);
                if found_guard.stopped() {
                    mark_interrupted(st);
                    return;
                }
            }
        }
        if dx.counting && dx.root.is_none() {
            let mut us = Vec::new();
            unit_lits(ix, dx, st, d, &mut us);
            for &l in &us {
                if !into_eq(ix, st, None, l, &unit, &mut found, &mut found_guard) {
                    mark_interrupted(st);
                    return;
                }
            }
        }
    }
    if dx.root.is_some() {
        if !rooted_unit(ix, me, st, d, &mut found, &mut found_guard) {
            mark_interrupted(st);
            return;
        }
        // the EL successor of a unit existential restriction takes the ground markers,
        // unless the restriction names its constant
        if let Node::Exist(..) = h.node
            && into_nominal(ix, d).is_none()
        {
            super::dl_root::new_succ(st, h.succ, &mut found_guard);
        }
    }
    drain_admitted(ix, st, found, &mut found_guard);
}

/// The rules of a rooted run on a new premise `prem` whose maximal literal is `l` in a
/// context other than the root: `N_u(x) → x ≈ u`, the differing successor terms of a
/// minimum's holder, and the equality rules.
#[allow(clippy::too_many_arguments)]
fn rooted_max<S: Sink>(
    ix: &Index,
    me: Ex,
    st: &State,
    l: Lit,
    prem: &Side,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
    sel: bool,
    gbody: bool,
    sel_x: bool,
) -> bool {
    let dx = dix(ix);
    if l.kind() == X && !sel_x && !rooted_hyper(ix, l.atom(), prem, found, guard) {
        return false;
    }
    if l.kind() == X && !super::dl_root::univ_from_x(ix, st, l.atom(), prem, found, guard) {
        return false;
    }
    let q = super::dl_eq::Eqs {
        ix,
        dx,
        root: false,
        query: query_of(ix, me),
    };
    q.on_max(st, l, prem, found, guard, sel, gbody)
}

/// The named class whose context `me` is, which is a query context of the journal with that
/// class as its core atom, or `None` for a hypothetical successor or the root. The context
/// of `owl:Thing` is the query context of `⊤ ⊑ C`, so its named-class atoms over a constant
/// are rewritten back over x as well (item 1070).
fn query_of(ix: &Index, me: Ex) -> Option<Ex> {
    (me == TOP || matches!(ix.head(me).node, Node::Class(_))).then_some(me)
}

/// The rules of a rooted run on a new unit `d(x)`, as [`rooted_max`] has them, with the
/// unit's literals in place: the atom, and an existential restriction's role and core.
fn rooted_unit<S: Sink>(
    ix: &Index,
    me: Ex,
    st: &State,
    d: Ex,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    use super::dl_clause::{RTerm, XLit};
    let dx = dix(ix);
    let u = unit();
    if !rooted_hyper(ix, d, &u, found, guard) {
        return false;
    }
    if !super::dl_root::univ_from_x(ix, st, d, &u, found, guard) {
        return false;
    }
    let q = super::dl_eq::Eqs {
        ix,
        dx,
        root: false,
        query: query_of(ix, me),
    };
    if !q.on_unit(st, XLit::Atom(d, RTerm::X), found, guard) {
        return false;
    }
    // an existential restriction into a singleton names the constant, and its EL
    // successor's clauses do not come back over f(x) (item 822)
    if let Some((r, v)) = into_nominal(ix, d) {
        found.rule = "nomunit";
        let inv = |s| dx.inv(ix, s);
        for l in [
            XLit::Role(r, RTerm::X, RTerm::C(v)),
            XLit::Atom(ix.head(d).succ, RTerm::C(v)),
        ] {
            if let Made::Lit(l) = guard.dl().lits.lit(l.canon(inv), inv) {
                let Some(bytes) = side_exact_bytes(0, 1) else {
                    guard.data_overflow();
                    return false;
                };
                if !found.push_admitted(bytes, || with_exact(&u, l), guard) {
                    return false;
                }
            }
        }
        return true;
    }
    if let Node::Exist(r, _) = ix.head(d).node {
        let f = RTerm::Fx(d);
        if !q.on_unit(
            st,
            XLit::Role(r, RTerm::X, f).canon(|s| dx.inv(ix, s)),
            found,
            guard,
        ) {
            return false;
        }
        for &a in dx.core(d) {
            if a != TOP && !q.on_unit(st, XLit::Atom(a, f), found, guard) {
                return false;
            }
        }
    }
    true
}

/// Hyper of a rooted run's own clauses on a premise of `d(x)`: `N_u(x) → x ≈ u`, and
/// `M(x) → f_i(x) ≉ f_j(x)` for a minimum's holder M.
fn rooted_hyper<S: Sink>(
    ix: &Index,
    d: Ex,
    prem: &Side,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    found.rule = "rhyper";
    use super::dl_clause::{RTerm, XLit};
    let dx = dix(ix);
    let inv = |r: Role| dx.inv(ix, r);
    if let Node::Nominal(u) = ix.head(d).node
        && let Made::Lit(e) = guard.dl().lits.lit(XLit::Eq(RTerm::X, RTerm::C(u)), inv)
    {
        let Some(bytes) = side_parts_bytes(prem, &[], &[], None, Some(e)) else {
            guard.data_overflow();
            return false;
        };
        if !found.push_admitted(bytes, || with_exact(prem, e), guard) {
            return false;
        }
    }
    if let Some(es) = dx.ne_of.get(&d) {
        for (i, &a) in es.iter().enumerate() {
            for &b in &es[i + 1..] {
                if let Made::Lit(n) = Lit::ne(Term::Fun(a), Term::Fun(b)) {
                    let Some(bytes) = side_parts_bytes(prem, &[], &[], None, Some(n)) else {
                        guard.data_overflow();
                        return false;
                    };
                    if !found.push_admitted(bytes, || with_exact(prem, n), guard) {
                        return false;
                    }
                }
            }
        }
    }
    true
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
fn prop_id<S: Sink>(x: &mut DlSt, s: Role, c: Ex, out: &mut S) -> Option<u32> {
    if let Some(&id) = x.props.get(&(s, c)) {
        return Some(id);
    }
    let entry = std::mem::size_of::<((Role, Ex), u32)>();
    let bound = if x.props.len() >= x.props.capacity() {
        let Some(b) = next_hash_table_bytes(x.props.capacity(), entry) else {
            out.data_overflow();
            x.store.interrupted = true;
            return None;
        };
        b
    } else {
        0
    };
    if bound > 0 && !out.admit_dl(bound) {
        x.store.interrupted = true;
        return None;
    }
    let before = x.props.capacity();
    let id = out
        .dl()
        .table
        .push(Clause::new(vec![Lit::ry(s)], vec![Lit::y(c)]));
    x.props.insert((s, c), id);
    if bound > 0 {
        let delta = hash_table_bytes(x.props.capacity(), entry)
            .saturating_sub(hash_table_bytes(before, entry));
        out.commit_dl(bound, delta);
    }
    Some(id)
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
            let Some(x) = ds_admit(st, out) else { return };
            let Some(id) = prop_id(x, s, c, out) else {
                return;
            };
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
    let Some(id) = prop_id(x, s, c, out) else {
        return;
    };
    for &(u, e) in &x.dpreds {
        // a predecessor of a rooted run may reach the context over f_e(u) and finds the
        // role premises itself, with every role over f_e(u)
        if counting || dix(ix).root.is_some() || role_of(ix, e).is_some_and(|r| ix.roles.sub(r, s))
        {
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

/// Whether every literal of `c` (both body and head) is a plain ground literal over
/// constants alone (no `x`, no `y`, no `f`), including the empty clause. Such a clause is
/// invariant under both ordinary `Pred` and rooted `root_pred` substitutions.
pub(super) fn constants_only(ix: &Index, dx: &DlIx, lits: &Lits, c: &Clause) -> bool {
    let inv = |r: Role| dx.inv(ix, r);
    let ok = |&l: &Lit| l.kind() == XL && is_ground(lits.view(l, inv));
    c.body().iter().all(ok) && c.head().iter().all(ok)
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
            // a ground literal goes whatever its form, as Table 3 has it
            XLit::Ne(a, b) => matches!((a, b), (RTerm::C(_), RTerm::C(_))),
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
            let Some(x) = ds_admit(st, out) else { return };
            for (s, c) in props {
                let Some(id) = prop_id(x, s, c, out) else {
                    return;
                };
                out.send(e, Concl::PredC(me, id));
            }
        }
    }
    let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) else {
        return;
    };
    let skip_late = out.dl().shared_facts && skip_non_root_late_predc();
    for k in 0..x.store.preds.len() {
        let i = x.store.preds[k];
        if skip_late && x.store.clauses[i as usize].fact {
            continue;
        }
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
    let Some(x) = ds_admit(st, out) else {
        return false;
    };
    match set_insert_admit(&mut x.hyps, a, out) {
        Some(true) => {}
        Some(false) => return false,
        None => {
            x.store.interrupted = true;
            return false;
        }
    }
    let _ = x
        .store
        .add_admitted(Clause::new(vec![Lit::x(a)], vec![Lit::x(a)]), out);
    true
}

/// A predecessor registers by its existential restriction e: it takes the clauses over the
/// predecessor that e's role meets, the propagations, and unsatisfiability.
pub fn dedge<S: Sink>(ix: &Index, me: Ex, st: &mut State, u: Ex, e: Ex, out: &mut S) -> bool {
    let bottom = st.bottom;
    let props: Vec<(Role, Ex)> = st.props.clone();
    // a predecessor of a rooted run may reach the context over f_e(u) and finds the role
    // premises itself, with every role over f_e(u)
    let any = dix(ix).counting || dix(ix).root.is_some();
    if tracing() {
        eprintln!(
            "TR REG {} me={me} hyp={:?} u={u} e={e}",
            tr_ctx(st, false),
            dix(ix).of_hyp.get(&me)
        );
    }
    let Some(x) = ds_admit(st, out) else {
        return false;
    };
    x.empty_core = dix(ix).of_hyp.contains_key(&me);
    if x.dpreds.contains(&(u, e)) {
        return false;
    }
    if !vec_push_admit(&mut x.dpreds, (u, e), out) {
        x.store.interrupted = true;
        return false;
    }
    if bottom {
        let id = out.dl().empty();
        out.send(u, Concl::PredC(me, id));
        return true;
    }
    let Some(r) = role_of(ix, e) else { return true };
    let to_root = (dix(ix).is_root(u) || skip_non_root_late_predc()) && out.dl().shared_facts;
    for k in 0..x.store.preds.len() {
        let i = x.store.preds[k];
        if to_root && x.store.clauses[i as usize].fact {
            continue;
        }
        if let Some(cid) = pred_id(x, i, out)
            && (any || meets(ix, &x.store.clauses[i as usize].c, r))
        {
            out.send(u, Concl::PredC(me, cid));
        }
    }
    for (s, c) in props {
        if any || ix.roles.sub(r, s) {
            let Some(id) = prop_id(x, s, c, out) else {
                break;
            };
            out.send(u, Concl::PredC(me, id));
        }
    }
    true
}

/// Visits each existential restriction of this context whose successor is `v` in
/// ascending restriction-ID order without duplicates and without allocating an
/// intermediate edge snapshot, stopping early when `cb` returns `false`.
fn for_each_reaching(dx: &DlIx, st: &State, v: Ex, mut cb: impl FnMut(Ex) -> bool) -> bool {
    let explicit = dl(st).map_or(&[][..], |x| x.edges_to(v));
    if dx.of_hyp.contains_key(&v) {
        for &(e, _) in explicit {
            if !cb(e) {
                return false;
            }
        }
        return true;
    }
    let unit_candidates = dx.by_succ.get(&v).map_or(&[][..], Vec::as_slice);
    debug_assert!(unit_candidates.windows(2).all(|w| w[0] < w[1]));
    let mut ei = 0usize;
    let mut ui = 0usize;
    loop {
        while ui < unit_candidates.len() && !st.subs.contains(&unit_candidates[ui]) {
            ui += 1;
        }
        match (
            explicit.get(ei).map(|&(e, _)| e),
            unit_candidates.get(ui).copied(),
        ) {
            (Some(ee), Some(ue)) => {
                let next = match ee.cmp(&ue) {
                    std::cmp::Ordering::Less => {
                        ei += 1;
                        ee
                    }
                    std::cmp::Ordering::Greater => {
                        ui += 1;
                        ue
                    }
                    std::cmp::Ordering::Equal => {
                        ei += 1;
                        ui += 1;
                        ee
                    }
                };
                if !cb(next) {
                    return false;
                }
            }
            (Some(ee), None) => {
                ei += 1;
                if !cb(ee) {
                    return false;
                }
            }
            (None, Some(ue)) => {
                ui += 1;
                if !cb(ue) {
                    return false;
                }
            }
            (None, None) => return true,
        }
    }
}

/// A clause over the predecessor from the successor `v`, in the context `me`: Pred with
/// each existential restriction that reaches v over `f_e(x)`, and in a rooted run along
/// each of the context's edges over `f_e(u)` to v, once per clause.
pub fn pred_c<S: Sink>(ix: &Index, me: Ex, st: &mut State, v: Ex, cid: u32, out: &mut S) -> bool {
    if st.bottom {
        return false;
    }
    let dx = dix(ix);
    let root = dx.is_root(me);
    let has_ord = !root && has_eligible_ordinary_edge(ix, dx, st, v);
    let Some(x) = ds_admit(st, out) else {
        return false;
    };
    if x.pred_seen.contains(&(v, cid)) {
        return false;
    }
    let (is_const_shared, is_mark) = {
        let c = out.dl().table.get(cid);
        let cs =
            dx.root.is_some() && out.dl().shared_facts && constants_only(ix, dx, &out.dl().lits, c);
        (cs, cs && super::dl_root::is_marker(c))
    };
    let pairs_nonempty = x.root.as_deref().is_some_and(|r| !r.edges_to(v).is_empty());
    let has_edge = pairs_nonempty || has_ord;
    let queue_pending = is_const_shared && !has_edge && !is_mark;
    if queue_pending && super::dl_root::root_admit(x, out).is_none() {
        x.store.interrupted = true;
        return false;
    }
    let Some(bound) = set_insert_bound(&x.pred_seen, &(v, cid))
        .and_then(|a| map_vec_push_bound(&x.pred_in, &v).and_then(|b| a.checked_add(b)))
        .and_then(|ab| {
            if queue_pending {
                let r = x.root.as_deref().expect("root_admit initialized RootSt");
                map_vec_push_bound(&r.pred_pending, &v).and_then(|p| ab.checked_add(p))
            } else {
                Some(ab)
            }
        })
    else {
        out.data_overflow();
        x.store.interrupted = true;
        return false;
    };
    if bound > 0 && !out.admit_dl(bound) {
        x.store.interrupted = true;
        return false;
    }
    let mut delta = set_insert_delta(&mut x.pred_seen, (v, cid)).unwrap_or(0);
    delta += map_vec_push(&mut x.pred_in, v, cid);
    if queue_pending {
        let r = x
            .root
            .as_deref_mut()
            .expect("root_admit initialized RootSt");
        delta += map_vec_push(&mut r.pred_pending, v, cid);
    }
    if bound > 0 {
        out.commit_dl(bound, delta);
    }
    let mut found_guard = Guard::new(out);
    let mut found = Found {
        root,
        ..Found::default()
    };
    let mut ok = true;
    if is_const_shared {
        if has_edge && !is_mark {
            found.rule = "predg";
            let bytes = {
                let c = found_guard.dl().table.get(cid);
                side_exact_bytes(c.body().len(), c.head().len())
            };
            let Some(bytes) = bytes else {
                found_guard.data_overflow();
                mark_interrupted(st);
                return true;
            };
            if !found.reserve_one_admitted(1, bytes, &mut found_guard) {
                mark_interrupted(st);
                return true;
            }
            let side = {
                let c = found_guard.dl().table.get(cid);
                (c.body().to_vec(), c.head().to_vec())
            };
            found.push(side);
            found_guard.release_to(found.held_bytes());
        }
    } else {
        if !root {
            ok = for_each_reaching(dx, st, v, |e| {
                into_nominal(ix, e).is_some()
                    || pred(ix, st, e, cid, None, &mut found, &mut found_guard)
            });
        }
        if ok
            && let Some(pairs) = dl(st)
                .and_then(|x| x.root.as_deref())
                .map(|r| r.edges_to(v))
        {
            for &(e, u) in pairs {
                if !super::dl_root::root_pred(ix, st, e, u, cid, None, &mut found, &mut found_guard)
                {
                    ok = false;
                    break;
                }
            }
        }
    }
    if !ok {
        mark_interrupted(st);
        return true;
    }
    if root {
        drain_with_admitted(ix, st, found, &mut found_guard, &RootDerive);
    } else {
        drain_admitted(ix, st, found, &mut found_guard);
    }
    true
}

/// Pred over the successor term f with the clause `cid` of f's successor: each body atom `A`
/// takes a premise whose maximal literal is `F(f, A)`, each role atom `s(y, x)` one whose
/// maximal literal is a role below s over f(x), and the head's literals over y become
/// literals over x and f(x). `fixed` is the premise that a new clause gives for one of
/// these literals, which the inference must use at one of the places it fits. Counts
/// borrowed premises with zero heap allocation and pre-admits `Found::prods` growth,
/// payload, and `prune` scratch before materializing any lists or interning head literals.
fn pred<S: Sink>(
    ix: &Index,
    st: &State,
    f: Ex,
    cid: u32,
    fixed: Option<(Lit, &Side)>,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    use super::dl_clause::XLit;
    use std::mem::size_of;

    found.rule = "pred";
    let dx = dix(ix);
    let inv = |r: Role| dx.inv(ix, r);
    let map_xl = |lits: &Lits, l: Lit| -> Result<Option<XLit>, ()> {
        let v = lits.view(l, inv);
        if is_ground(v) {
            return Ok(None);
        }
        // a role atom between x and a constant goes only to the root
        if let XLit::Role(_, a, b) = v
            && matches!((a, b), (RTerm::X, RTerm::C(_)) | (RTerm::C(_), RTerm::X))
        {
            return Err(());
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
        match w {
            None => Err(()),
            Some(XLit::Eq(a, b)) if a == b => Err(()),
            Some(w) => Ok(Some(w)),
        }
    };
    let fits = |n: Lit, p: Lit| match n.kind() {
        RL if dx.counting => p.kind() == RL && p.fun() == f && dx.sub(ix, p.atom(), n.atom()),
        _ => n == p,
    };

    // Pass 0 (zero allocations): validate clause, count `ground_len`, `head_len`, `need_len`,
    // and check whether `fixed` fits at least one body place.
    let (ground_len, head_len, need_len, single_ry, c_head_len) = {
        let dl = guard.dl();
        let lits = &dl.lits;
        let c = dl.table.get(cid);
        let mut ground_len = 0usize;
        let mut x_count = 0usize;
        let mut ry_count = 0usize;
        for &l in c.body() {
            match l.kind() {
                XL if dx.root.is_some() => {
                    if !is_ground(lits.view(l, inv)) {
                        return true;
                    }
                    ground_len += 1;
                }
                X => x_count += 1,
                RY => ry_count += 1,
                _ => {}
            }
        }
        let single_ry = if !dx.counting && ry_count > 0 {
            let Some(r) = role_of(ix, f) else {
                return true;
            };
            if !meets(ix, c, r) {
                return true;
            }
            Some(r)
        } else {
            None
        };
        let need_len = x_count
            + if dx.counting {
                ry_count
            } else {
                usize::from(single_ry.is_some())
            };
        if let Some((p, _)) = fixed {
            let mut any_fit = false;
            for &l in c.body() {
                if l.kind() == X && fits(Lit::f(f, l.atom()), p) {
                    any_fit = true;
                    break;
                }
            }
            if !any_fit {
                if dx.counting {
                    for &l in c.body() {
                        if l.kind() == RY && fits(Lit::rl(f, l.atom()), p) {
                            any_fit = true;
                            break;
                        }
                    }
                } else if let Some(r) = single_ry
                    && fits(Lit::rl(f, r), p)
                {
                    any_fit = true;
                }
            }
            if !any_fit {
                return true;
            }
        }
        let mut head_len = 0usize;
        for &l in c.head() {
            match l.kind() {
                Y | RY => head_len += 1,
                XL if dx.root.is_some() => match map_xl(lits, l) {
                    Ok(None) => head_len += 1,
                    Ok(Some(XLit::Ne(a, b))) if a == b => {}
                    Ok(Some(_)) => head_len += 1,
                    Err(()) => return true,
                },
                _ => return true,
            }
        }
        (ground_len, head_len, need_len, single_ry, c.head().len())
    };

    let Some(base_bytes) = side_exact_bytes(ground_len, head_len) else {
        guard.data_overflow();
        return false;
    };
    let for_each_need = |c: &Clause, cb: &mut dyn FnMut(usize, Lit) -> bool| {
        let mut idx = 0usize;
        for &l in c.body() {
            if l.kind() == X {
                if !cb(idx, Lit::f(f, l.atom())) {
                    return;
                }
                idx += 1;
            }
        }
        if dx.counting {
            for &l in c.body() {
                if l.kind() == RY {
                    if !cb(idx, Lit::rl(f, l.atom())) {
                        return;
                    }
                    idx += 1;
                }
            }
        } else if let Some(r) = single_ry {
            let _ = cb(idx, Lit::rl(f, r));
        }
    };

    let empty_side = unit();
    let emit_place = |fixed_place: Option<(usize, &Side)>,
                      found: &mut Found,
                      guard: &mut Guard<'_, S>|
     -> bool {
        // Pass 1 (zero allocations): count borrowed partner premises and exact bytes.
        let (prod_bytes, prune_scratch, empty_pos) = {
            let c = guard.dl().table.get(cid);
            let Some(mut lists_bytes) = need_len.checked_mul(size_of::<Vec<Side>>()) else {
                guard.data_overflow();
                return false;
            };
            let mut max_list_len = 0usize;
            let mut empty_pos = false;
            let mut overflow = false;
            for_each_need(c, &mut |j, n| {
                let mut count = 0usize;
                let mut sides_bytes = 0usize;
                if let Some((k, side)) = fixed_place
                    && j == k
                {
                    count = 1;
                    let Some(b) = side_exact_bytes(side.0.len(), side.1.len()) else {
                        overflow = true;
                        return false;
                    };
                    sides_bytes = b;
                } else {
                    let mut acc = |pb: &[Lit], ph: &[Lit], without: Option<Lit>| -> bool {
                        let Some(sb) = side_parts_bytes(&empty_side, pb, ph, without, None)
                            .and_then(|b| sides_bytes.checked_add(b))
                        else {
                            overflow = true;
                            return false;
                        };
                        let Some(nc) = count.checked_add(1) else {
                            overflow = true;
                            return false;
                        };
                        count = nc;
                        sides_bytes = sb;
                        true
                    };
                    match n.kind() {
                        RL if dx.counting => {
                            for_each_partner_rl(ix, dx, st, f, n.atom(), &mut acc);
                        }
                        _ => {
                            for_each_partner_succ(ix, dx, st, n, &mut acc);
                        }
                    }
                    if overflow {
                        return false;
                    }
                }
                if count == 0 {
                    empty_pos = true;
                    return false;
                }
                max_list_len = max_list_len.max(count);
                let Some(pos_bytes) = count
                    .checked_mul(size_of::<Side>())
                    .and_then(|b| b.checked_add(sides_bytes))
                    .and_then(|b| lists_bytes.checked_add(b))
                else {
                    overflow = true;
                    return false;
                };
                lists_bytes = pos_bytes;
                true
            });
            if overflow {
                guard.data_overflow();
                return false;
            }
            if empty_pos {
                (0, 0, true)
            } else {
                let Some(prod_bytes) = lists_bytes.checked_add(base_bytes) else {
                    guard.data_overflow();
                    return false;
                };
                let prune_scratch = if max_list_len < 2 {
                    0
                } else {
                    let Some(ps) = max_list_len.checked_mul(PRUNE_SCRATCH_ELEM) else {
                        guard.data_overflow();
                        return false;
                    };
                    ps
                };
                (prod_bytes, prune_scratch, false)
            }
        };
        if empty_pos {
            return true;
        }
        if !found.reserve_prod_admitted(prod_bytes, prune_scratch, 0, guard) {
            return false;
        }
        // Pass 2 (post-admission): build exact-capacity `(ground, head)` and `lists`.
        let mut ground = Vec::with_capacity(ground_len);
        let mut head = Vec::with_capacity(head_len);
        {
            let c = guard.dl().table.get(cid);
            if dx.root.is_some() {
                for &l in c.body() {
                    if l.kind() == XL {
                        ground.push(l);
                    }
                }
            }
            for &l in c.head() {
                match l.kind() {
                    Y => head.push(Lit::x(l.atom())),
                    RY => head.push(Lit::rl(f, l.atom())),
                    _ => {}
                }
            }
        }
        if dx.root.is_some() {
            for idx in 0..c_head_len {
                let step = {
                    let dl = guard.dl();
                    let l = dl.table.get(cid).head()[idx];
                    if l.kind() != XL {
                        None
                    } else {
                        Some((l, map_xl(&dl.lits, l).expect("validated in pass 0")))
                    }
                };
                if let Some((l, mapped)) = step {
                    match mapped {
                        None => head.push(l),
                        Some(w) => {
                            let Some(made) = super::dl_clause::admit_lit(w, inv, guard) else {
                                drop(ground);
                                drop(head);
                                guard.release_to(found.held_bytes());
                                return false;
                            };
                            if let Made::Lit(m) = made {
                                head.push(m);
                            }
                        }
                    }
                }
            }
        }
        let mut lists = Vec::with_capacity(need_len);
        {
            let c = guard.dl().table.get(cid);
            for_each_need(c, &mut |j, n| {
                if let Some((k, side)) = fixed_place
                    && j == k
                {
                    lists.push(vec![(side.0.clone(), side.1.clone())]);
                } else {
                    let mut count = 0usize;
                    let mut count_cb = |_: &[Lit], _: &[Lit], _: Option<Lit>| {
                        count += 1;
                        true
                    };
                    match n.kind() {
                        RL if dx.counting => {
                            for_each_partner_rl(ix, dx, st, f, n.atom(), &mut count_cb);
                        }
                        _ => {
                            for_each_partner_succ(ix, dx, st, n, &mut count_cb);
                        }
                    }
                    let mut v = Vec::with_capacity(count);
                    let mut push_cb = |pb: &[Lit], ph: &[Lit], without: Option<Lit>| {
                        v.push(merge_parts(&empty_side, pb, ph, without, None));
                        true
                    };
                    match n.kind() {
                        RL if dx.counting => {
                            for_each_partner_rl(ix, dx, st, f, n.atom(), &mut push_cb);
                        }
                        _ => {
                            for_each_partner_succ(ix, dx, st, n, &mut push_cb);
                        }
                    }
                    lists.push(v);
                }
                true
            });
        }
        let base = (ground, head);
        debug_assert_eq!(
            prod_inner_bytes(lists.capacity(), &lists, &base),
            prod_bytes
        );
        found.products(lists, base);
        guard.release_to(found.held_bytes());
        true
    };

    match fixed {
        None => emit_place(None, found, guard),
        Some((p, side)) => {
            for k in 0..need_len {
                let mut matches_k = false;
                {
                    let c = guard.dl().table.get(cid);
                    for_each_need(c, &mut |j, n| {
                        if j == k {
                            matches_k = fits(n, p);
                            return false;
                        }
                        true
                    });
                }
                if !matches_k {
                    continue;
                }
                if !emit_place(Some((k, side)), found, guard) {
                    return false;
                }
                if !dx.counting {
                    break;
                }
            }
            true
        }
    }
}

/// Runs the rules on up to `budget` clauses waiting in the store, and stops early once
/// they gave the context units, which the EL rules take first. The answer is the number
/// of clauses processed.
pub fn flush<S: Sink>(ix: &Index, me: Ex, st: &mut State, out: &mut S, budget: usize) -> usize {
    let mut n = 0;
    // the minimums that waited for the units make their witnesses now, unless the context
    // holds a minimum that entails them
    let waiting = st
        .extra
        .as_mut()
        .and_then(|x| x.dl.as_deref_mut())
        .map(|x| std::mem::take(&mut x.pending))
        .unwrap_or_default();
    for d in waiting {
        if blocked(dix(ix), st, d) {
            continue;
        }
        if let Some(&w) = dix(ix).witnesses.get(&d) {
            out.me(Concl::Sub(w));
            n += 1;
        }
    }
    if n > 0 {
        return n;
    }
    let mut max_guard = Guard::new(out);
    let mut max: Vec<Lit> = Vec::new();
    while n < budget {
        if st.bottom {
            break;
        }
        let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) else {
            break;
        };
        let dx = dix(ix);
        let rooted = dx.root.is_some();
        let Some(id) = x.store.peek_live(max_guard.dl()) else {
            break;
        };
        let Some((views_bytes, max_realloc)) =
            x.store.next_scratch_bound(id, rooted, &max, max.capacity())
        else {
            max_guard.data_overflow();
            x.store.interrupted = true;
            break;
        };
        let max_live = max.capacity() * std::mem::size_of::<Lit>();
        let Some(scratch_peak) = max_live
            .checked_add(max_realloc)
            .and_then(|b| b.checked_add(views_bytes))
        else {
            max_guard.data_overflow();
            x.store.interrupted = true;
            break;
        };
        if !max_guard.ensure_admitted(scratch_peak) {
            x.store.interrupted = true;
            break;
        }
        if rooted {
            let inv = |r: Role| dx.inv(ix, r);
            let ground_total = super::dl_clause::ground_total();
            let bound = {
                let o = super::dl_clause::Rooted {
                    ix,
                    lits: &max_guard.dl().lits,
                    inv: &inv,
                    ground_total,
                };
                x.store.next_prepare_id(ix, Some(&o), id, &mut max)
            };
            max_guard.release_to(max.capacity() * std::mem::size_of::<Lit>());
            let Some(bound) = bound else {
                max_guard.data_overflow();
                x.store.interrupted = true;
                break;
            };
            if !max_guard.admit_dl(bound) {
                x.store.interrupted = true;
                break;
            }
            let delta = {
                let o = super::dl_clause::Rooted {
                    ix,
                    lits: &max_guard.dl().lits,
                    inv: &inv,
                    ground_total,
                };
                x.store.next_commit(ix, Some(&o), id, &max, max_guard.dl())
            };
            max_guard.commit_dl(bound, delta);
        } else {
            let bound = x.store.next_prepare_id(ix, None, id, &mut max);
            max_guard.release_to(max.capacity() * std::mem::size_of::<Lit>());
            let Some(bound) = bound else {
                max_guard.data_overflow();
                x.store.interrupted = true;
                break;
            };
            if !max_guard.admit_dl(bound) {
                x.store.interrupted = true;
                break;
            }
            let delta = x.store.next_commit(ix, None, id, &max, max_guard.dl());
            max_guard.commit_dl(bound, delta);
        }
        let mut given_fact = false;
        if rooted {
            super::dl_root::index_consts(ix, x, &max, id, &mut max_guard);
            if max_guard.stopped() {
                x.store.interrupted = true;
                break;
            }
            let inv = |r: Role| dx.inv(ix, r);
            x.empty_core = dx.of_hyp.contains_key(&me);
            let s = &mut x.store.clauses[id as usize];
            let (sel, gbody) = selection_flags(&max_guard.dl().lits, inv, &s.c);
            s.sel = sel;
            s.gbody = gbody;
            given_fact = max_guard.dl().shared_facts
                && s.fact
                && s.c.body().is_empty()
                && s.c.head().len() == 1;
            x.given_fact = given_fact;
        }
        let c_bytes = {
            let c = &x.store.clauses[id as usize].c;
            (c.body().len() + c.head().len()) * std::mem::size_of::<Lit>()
        };
        let Some(live_need) = (max.capacity() * std::mem::size_of::<Lit>()).checked_add(c_bytes)
        else {
            max_guard.data_overflow();
            x.given_fact = false;
            x.store.interrupted = true;
            break;
        };
        if !max_guard.ensure_admitted(live_need) {
            x.given_fact = false;
            x.store.interrupted = true;
            break;
        }
        n += 1;
        let c = x.store.clauses[id as usize].c.clone();
        if given_fact {
            super::dl_root::register_max(ix, me, st, &max, &mut max_guard);
            if max_guard.stopped() {
                if let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) {
                    x.given_fact = false;
                    x.store.interrupted = true;
                }
                break;
            }
        }
        process(ix, me, st, id, &c, &max, &mut max_guard);
        drop(c);
        max_guard.release_to(max.capacity() * std::mem::size_of::<Lit>());
        if let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) {
            x.given_fact = false;
        }
        // the units go to the EL rules first, and a stopped run leaves the rest queued,
        // as an activation of a run in steps does once it derived its share
        if max_guard.busy() || max_guard.stopped() || max_guard.pause() {
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
    // Declare `found_guard` strictly before `found` so LIFO drop always frees `found`
    // before releasing pre-admitted bytes, including on early return or panic unwind.
    let mut found_guard = Guard::new(out);
    let mut found = Found::default();
    if tracing() {
        let lits = &found_guard.dl().lits;
        eprintln!(
            "TR P {} {id} {} -> {}",
            tr_ctx(st, false),
            tr(ix, lits, c.body()),
            tr(ix, lits, c.head())
        );
    }
    if rooted {
        if !super::dl_root::join_body(ix, st, c, &mut found, &mut found_guard) {
            mark_interrupted(st);
            return;
        }
        super::dl_root::register_body(ix, me, st, c, &mut found_guard);
    }
    let ready = if rooted {
        pred_ready(ix, dx, &found_guard.dl().lits, c)
    } else {
        max.is_empty()
    };
    if ready {
        // Pred: the clause goes to the predecessors whose role meets its role atoms
        let x = ds(st);
        let Some(cid) = pred_id(x, id, &mut found_guard) else {
            drain_admitted(ix, st, found, &mut found_guard);
            return;
        };
        // a predecessor of a rooted run may reach the context over f_e(u) and finds the
        // role premises itself, with every role over f_e(u). Ordinary Pred delivery and
        // late replay are preserved for every non-root predecessor (gap G6); only the root
        // itself has an independent witness that it already holds its own delivered unit.
        let skip_live = x.given_fact && skip_non_root_live_predc();
        for &(u, e) in &x.dpreds {
            if x.given_fact && (dx.is_root(u) || skip_live) {
                continue;
            }
            if rooted || role_of(ix, e).is_some_and(|r| meets(ix, c, r)) {
                found_guard.send(u, Concl::PredC(me, cid));
            }
        }
        if !skip_live {
            for (r, srcs) in &st.back {
                if meets(ix, c, *r) {
                    for &e in &srcs.list {
                        found_guard.send(e, Concl::PredC(me, cid));
                    }
                }
            }
        }
        // the predecessors that register later replay the clauses over the predecessor,
        // which a rooted run's store leaves to this test
        if rooted && let Some(x) = ds_admit(st, &mut found_guard) {
            let _ = x.store.push_pred_admitted(id, &mut found_guard);
        }
        if !rooted || max.is_empty() {
            drain_admitted(ix, st, found, &mut found_guard);
            return;
        }
    }
    // a clause with a ground body literal is cut by `gbody` from non-local ground Hyper,
    // head-side Join, and purely ground/Fc Eq/Factor even when `select_ground` is off
    // (item 1250), while `sel` gates mixed non-local Eq/Factor and (without
    // `sel_partners_x`) Hyper-over-x and Fc-Pred partners
    let (sel, gbody) = if rooted {
        dl(st).map_or((false, false), |x| {
            let s = &x.store.clauses[id as usize];
            (s.sel, s.gbody)
        })
    } else {
        (false, false)
    };
    let sel_x = sel && ready;
    let mut ps: Vec<(Role, Ex)> = Vec::new();
    for &l in max {
        if found_guard.stopped() {
            mark_interrupted(st);
            break;
        }
        let Some(prem_bytes) = side_bytes(c, l) else {
            found_guard.data_overflow();
            mark_interrupted(st);
            return;
        };
        let Some(prem_need) = found.held_bytes().checked_add(prem_bytes) else {
            found_guard.data_overflow();
            mark_interrupted(st);
            return;
        };
        if !found_guard.ensure_admitted(prem_need) {
            mark_interrupted(st);
            return;
        }
        let prem = side(c, l);
        debug_assert_eq!(side_cap_bytes(&prem), prem_bytes);
        found.set_extra_bytes(prem_bytes);
        if dx.root.is_some()
            && !rooted_max(
                ix,
                me,
                st,
                l,
                &prem,
                &mut found,
                &mut found_guard,
                sel,
                gbody,
                sel_x,
            )
        {
            mark_interrupted(st);
            return;
        }
        match l.kind() {
            X if sel_x => {
                // the universals d stands for over the successors, the one inference over
                // x whose conclusion no predecessor can make
                let d = l.atom();
                found.rule = "all";
                alls(ix, d, &mut ps);
                for &(t, x) in &ps {
                    for (e, qs) in partners_rf(ix, st, t) {
                        for q in qs {
                            let add = Lit::f(e, x);
                            let Some(bytes) = side_parts_bytes(&prem, &q.0, &q.1, None, Some(add))
                            else {
                                found_guard.data_overflow();
                                mark_interrupted(st);
                                return;
                            };
                            if !found.push_admitted(
                                bytes,
                                || merge_with_exact(&prem, &q, add),
                                &mut found_guard,
                            ) {
                                mark_interrupted(st);
                                return;
                            }
                        }
                    }
                }
            }
            X => {
                let d = l.atom();
                let h = ix.head(d);
                found.rule = "unfold";
                match h.node {
                    Node::Conj(a, b) => {
                        for lit in [Lit::x(a), Lit::x(b)] {
                            let Some(bytes) = side_parts_bytes(&prem, &[], &[], None, Some(lit))
                            else {
                                found_guard.data_overflow();
                                mark_interrupted(st);
                                return;
                            };
                            if !found.push_admitted(
                                bytes,
                                || with_exact(&prem, lit),
                                &mut found_guard,
                            ) {
                                mark_interrupted(st);
                                return;
                            }
                        }
                    }
                    Node::Exist(r, _) => {
                        if let Some((r, v)) = into_nominal(ix, d) {
                            let inv = |r| dx.inv(ix, r);
                            for xl in [
                                super::dl_clause::XLit::Role(r, RTerm::X, RTerm::C(v)),
                                super::dl_clause::XLit::Atom(h.succ, RTerm::C(v)),
                            ] {
                                if let Made::Lit(lit) =
                                    found_guard.dl().lits.lit(xl.canon(inv), inv)
                                {
                                    let Some(bytes) =
                                        side_parts_bytes(&prem, &[], &[], None, Some(lit))
                                    else {
                                        found_guard.data_overflow();
                                        mark_interrupted(st);
                                        return;
                                    };
                                    if !found.push_admitted(
                                        bytes,
                                        || with_exact(&prem, lit),
                                        &mut found_guard,
                                    ) {
                                        mark_interrupted(st);
                                        return;
                                    }
                                }
                            }
                        } else {
                            for lit in [Lit::rl(d, r), Lit::f(d, h.succ)] {
                                let Some(bytes) =
                                    side_parts_bytes(&prem, &[], &[], None, Some(lit))
                                else {
                                    found_guard.data_overflow();
                                    mark_interrupted(st);
                                    return;
                                };
                                if !found.push_admitted(
                                    bytes,
                                    || with_exact(&prem, lit),
                                    &mut found_guard,
                                ) {
                                    mark_interrupted(st);
                                    return;
                                }
                            }
                        }
                    }
                    _ => {}
                }
                found.rule = "told";
                let skip = if blocked(dx, st, d) {
                    dx.witnesses.get(&d).copied()
                } else {
                    None
                };
                for &t in ix.told_of(h) {
                    if Some(t) != skip {
                        let lit = Lit::x(t);
                        let Some(bytes) = side_parts_bytes(&prem, &[], &[], None, Some(lit)) else {
                            found_guard.data_overflow();
                            mark_interrupted(st);
                            return;
                        };
                        if !found.push_admitted(bytes, || with_exact(&prem, lit), &mut found_guard)
                        {
                            mark_interrupted(st);
                            return;
                        }
                    }
                }
                found.rule = "conj";
                for &(p, k) in ix.conj_of(h) {
                    if !for_each_partner_x(st, p, |pb, ph, without| {
                        let add = Some(Lit::x(k));
                        let Some(bytes) = side_parts_bytes(&prem, pb, ph, without, add) else {
                            found_guard.data_overflow();
                            return false;
                        };
                        found.push_admitted(
                            bytes,
                            || merge_parts(&prem, pb, ph, without, add),
                            &mut found_guard,
                        )
                    }) {
                        mark_interrupted(st);
                        return;
                    }
                }
                found.rule = "disj";
                for &set in ix.disjoint_of(h) {
                    for &m in &dx.sets[set as usize] {
                        if m != d
                            && !for_each_partner_x(st, m, |pb, ph, without| {
                                let Some(bytes) = side_parts_bytes(&prem, pb, ph, without, None)
                                else {
                                    found_guard.data_overflow();
                                    return false;
                                };
                                found.push_admitted(
                                    bytes,
                                    || merge_parts(&prem, pb, ph, without, None),
                                    &mut found_guard,
                                )
                            })
                        {
                            mark_interrupted(st);
                            return;
                        }
                    }
                }
                if let Some(ids) = dx.by_body.get(&d) {
                    for &cl in ids {
                        if !hyper_clause(ix, st, cl, d, &prem, &mut found, &mut found_guard) {
                            mark_interrupted(st);
                            return;
                        }
                    }
                }
                // the universals d stands for, over the predecessor and the successors
                found.rule = "all";
                alls(ix, d, &mut ps);
                for &(t, x) in &ps {
                    if let Some(i) = roles.inv(t)
                        && dx.carries(i)
                    {
                        let Some(bytes) = side_exact_bytes(prem.0.len() + 1, prem.1.len() + 1)
                        else {
                            found_guard.data_overflow();
                            mark_interrupted(st);
                            return;
                        };
                        if !found.push_admitted(
                            bytes,
                            || {
                                let mut pb = Vec::with_capacity(prem.0.len() + 1);
                                pb.extend_from_slice(&prem.0);
                                pb.push(Lit::ry(i));
                                let mut ph = Vec::with_capacity(prem.1.len() + 1);
                                ph.extend_from_slice(&prem.1);
                                ph.push(Lit::y(x));
                                (pb, ph)
                            },
                            &mut found_guard,
                        ) {
                            mark_interrupted(st);
                            return;
                        }
                    }
                    for (e, qs) in partners_rf(ix, st, t) {
                        for q in qs {
                            let add = Lit::f(e, x);
                            let Some(bytes) = side_parts_bytes(&prem, &q.0, &q.1, None, Some(add))
                            else {
                                found_guard.data_overflow();
                                mark_interrupted(st);
                                return;
                            };
                            if !found.push_admitted(
                                bytes,
                                || merge_with_exact(&prem, &q, add),
                                &mut found_guard,
                            ) {
                                mark_interrupted(st);
                                return;
                            }
                        }
                    }
                }
                if dx.counting {
                    if let Some(&(n, q)) = dx.atmost.get(&d)
                        && !at_most(
                            ix,
                            st,
                            n,
                            q,
                            Holder::Prem(&prem),
                            None,
                            &[],
                            &mut found,
                            &mut found_guard,
                        )
                    {
                        mark_interrupted(st);
                        return;
                    }
                    if !on_filler(ix, st, d, &prem, &mut found, &mut found_guard) {
                        mark_interrupted(st);
                        return;
                    }
                }
            }
            F | RL | EQ | NE => {
                let e = l.fun();
                let had = dl(st).is_some_and(|x| x.edge_target(e).is_some());
                // Succ: the successor takes the marker
                let v = target(ix, me, st, e, &mut found_guard);
                let edge_v = v;
                // the late rule's replay takes the clauses with every partner, this
                // premise included, so the replay below skips v
                let replayed = late_rules(dx)
                    && !had
                    && dl(st).is_some_and(|x| x.edge_target(e).is_some())
                    && late(ix, me, st, e, v, "edge", &mut found, &mut found_guard);
                if found_guard.stopped() {
                    mark_interrupted(st);
                    return;
                }
                match l.kind() {
                    F => send_hyp(ix, st, v, l.atom(), &mut found_guard),
                    RL if !on_rl(ix, st, e, l.atom(), &prem, &mut found, &mut found_guard) => {
                        mark_interrupted(st);
                        return;
                    }
                    _ => {}
                }
                // a new certificate that e(x) is in a class of a disjointness set: the
                // clauses with an equality or inequality of e(x) are derived again, which
                // simplifies them whatever order they came in, and the shorter clause
                // subsumes the old one
                if dx.counting
                    && l.kind() == F
                    && !again(ix, st, id, c, &mut found, &mut found_guard)
                {
                    mark_interrupted(st);
                    return;
                }
                // a rooted run has its own equality rules
                if dx.counting && dx.root.is_none() {
                    let ok = if l.kind() == EQ {
                        on_eq(ix, st, Some(id), l, &prem, &mut found, &mut found_guard)
                    } else {
                        into_eq(ix, st, Some(id), l, &prem, &mut found, &mut found_guard)
                    };
                    if !ok {
                        mark_interrupted(st);
                        return;
                    }
                }
                if matches!(l.kind(), F | RL) && into_nominal(ix, e).is_none() {
                    // Pred with the clauses the successors of e sent: the one registered with,
                    // and the EL one when e is a unit
                    let mut es = [0 as Ex; 2];
                    let mut es_len = 0usize;
                    if let Some(w) = dl(st).and_then(|x| x.edge_target(e)) {
                        es[es_len] = w;
                        es_len += 1;
                    }
                    if st.subs.contains(&e) && !es[..es_len].contains(&ix.head(e).succ) {
                        es[es_len] = ix.head(e).succ;
                        es_len += 1;
                    }
                    for &v in &es[..es_len] {
                        if replayed && v == edge_v {
                            continue;
                        }
                        let cids = dl(st)
                            .and_then(|x| x.pred_in.get(&v))
                            .map_or(&[][..], |v| v.as_slice());
                        for &cid in cids {
                            if !pred(
                                ix,
                                st,
                                e,
                                cid,
                                Some((l, &prem)),
                                &mut found,
                                &mut found_guard,
                            ) {
                                mark_interrupted(st);
                                return;
                            }
                        }
                    }
                }
            }
            XL if rooted
                && !super::dl_root::on_max(
                    ix,
                    me,
                    st,
                    c,
                    l,
                    &prem,
                    &mut found,
                    &mut found_guard,
                    gbody,
                ) =>
            {
                mark_interrupted(st);
                return;
            }
            _ => {}
        }
        drop(prem);
        found.set_extra_bytes(0);
        found_guard.release_to(found.held_bytes());
    }
    drop(ps);
    debug_assert_eq!(found.extra_bytes(), 0);
    drain_admitted(ix, st, found, &mut found_guard);
}

/// Sends the successor `v` the marker of `a`, unless the successor's core has it.
pub(super) fn send_hyp<S: Sink>(ix: &Index, st: &mut State, v: Ex, a: Ex, out: &mut S) {
    if a == TOP || dl(st).is_some_and(|x| x.sent.contains(&(v, a))) {
        return;
    }
    let mut core = Vec::new();
    if !dix(ix).of_hyp.contains_key(&v) {
        out.core(ix, v, &mut core);
    }
    if core.binary_search(&a).is_err() {
        let Some(x) = ds_admit(st, out) else { return };
        match set_insert_admit(&mut x.sent, (v, a), out) {
            Some(true) => out.send(v, Concl::Hyp(a)),
            Some(false) => {}
            None => x.store.interrupted = true,
        }
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
    if let Some(v) = dl(st).and_then(|x| x.edge_target(e)) {
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
    let Some(x) = ds_admit(st, out) else {
        return v;
    };
    if !x.insert_edge_admit(e, v, out) {
        x.store.interrupted = true;
        return v;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A sink that counts the charges, releases and derived conclusions of [`fold`].
    #[derive(Default)]
    struct Charges {
        reserved: usize,
        released: usize,
        releases: usize,
        work: usize,
        /// the work past which the run stops, as a bound on derived conclusions does
        limit: Option<usize>,
        /// the bytes past which a reservation fails, as the memory budget does
        budget: Option<usize>,
    }

    impl Sink for Charges {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        // a reservation checks the bounds too, as the saturation's does
        fn reserve_data(&mut self, bytes: usize) -> bool {
            let ok = self.go_on()
                && self
                    .budget
                    .is_none_or(|b| self.reserved - self.released + bytes <= b);
            if ok {
                self.reserved += bytes;
            }
            ok
        }
        fn release_data(&mut self, bytes: usize) {
            self.released += bytes;
            self.releases += 1;
        }
        fn commit_dl(&mut self, _: usize, _: usize) {}
        fn worked(&mut self, n: usize) {
            self.work += n;
        }
        fn go_on(&self) -> bool {
            self.limit.is_none_or(|l| self.work <= l)
        }
    }

    fn index() -> Index {
        let mut it = super::super::index::Interner::new();
        Index::build(
            &super::super::index::Axioms::default(),
            &mut it,
            None,
            &|| false,
        )
        .unwrap()
    }

    fn side(head: &[Ex]) -> Side {
        (Vec::new(), head.iter().map(|&a| Lit::x(a)).collect())
    }

    /// Runs [`fold`] over the lists and gives the heads it derived, sorted.
    fn folded(lists: &[Vec<Side>], out: &mut Charges) -> Vec<Vec<Lit>> {
        struct CollectHeads(std::cell::RefCell<Vec<Vec<Lit>>>);
        impl Deriver for CollectHeads {
            fn derive<S: Sink>(&self, _: &Index, _: &mut State, s: Side, _: &mut S) {
                self.0.borrow_mut().push(s.1);
            }
        }
        let ix = index();
        let mut st = State::default();
        let derive = CollectHeads(std::cell::RefCell::new(Vec::new()));
        assert!(fold(
            &ix,
            &mut st,
            lists,
            side(&[]),
            "test",
            false,
            false,
            0,
            out,
            &derive
        ));
        let mut got = derive.0.into_inner();
        got.sort();
        got
    }

    /// A product derives only the least unions of one premise from each list (item 969).
    #[test]
    fn a_product_derives_its_least_unions() {
        let lists = vec![
            vec![side(&[1]), side(&[1, 2])],
            vec![side(&[2]), side(&[3])],
        ];
        let mut out = Charges::default();
        let got = folded(&lists, &mut out);
        let want: Vec<Vec<Lit>> = vec![side(&[1, 2]).1, side(&[1, 3]).1];
        assert_eq!(got, want);
        assert_eq!(out.work, 0);
    }

    /// The unions of a wide product's steps before the last count as derived conclusions
    /// for the run's bound once they pass [`FOLD_PRUNE`], so that a bounded run does not
    /// build them unbounded.
    #[test]
    fn a_wide_product_counts_the_unions_of_its_steps() {
        // fourteen lists of two premises each give 2^14 - 2 unions before the last step
        let lists: Vec<Vec<Side>> = (0..14)
            .map(|k| vec![side(&[2 * k + 1]), side(&[2 * k + 2])])
            .collect();
        let mut out = Charges::default();
        let got = folded(&lists, &mut out);
        assert_eq!(got.len(), 1 << 14);
        let built: usize = (1..14).map(|k| 1usize << k).sum();
        assert_eq!(out.work, built - FOLD_PRUNE);
    }

    /// A last step of more than [`FOLD_PRUNE`] unions derives them as it makes them, each
    /// counted, so that a run with a bound on its derived conclusions stops it within the
    /// bound instead of building every union first.
    #[test]
    fn a_wide_last_step_stops_at_the_bound() {
        let ix = index();
        let mut st = State::default();
        let wide = |from: Ex| (from..from + 4096).map(|a| side(&[a])).collect::<Vec<_>>();
        let lists = vec![wide(1), wide(10_000)];
        let mut out = Charges {
            limit: Some(20_000),
            ..Charges::default()
        };
        struct CountWorked(std::cell::Cell<usize>);
        impl Deriver for CountWorked {
            fn derive<S: Sink>(&self, _: &Index, _: &mut State, _: Side, out: &mut S) {
                self.0.set(self.0.get() + 1);
                out.worked(1);
            }
        }
        let derive = CountWorked(std::cell::Cell::new(0));
        assert!(!fold(
            &ix,
            &mut st,
            &lists,
            side(&[]),
            "test",
            false,
            false,
            0,
            &mut out,
            &derive
        ));
        let made = derive.0;
        assert!(
            made.get() <= 20_000 + PRODUCT_POLL + FOLD_PRUNE,
            "{}",
            made.get()
        );
        // the unions it built and charged are those it derived, not the 16.7 million of
        // the whole step
        assert!(out.reserved < 16 << 20, "{}", out.reserved);
    }

    /// A last step that turns to deriving its unions as it makes them releases the
    /// charge of the list it drains, so that the charge follows the unions that live.
    #[test]
    fn a_streamed_last_step_releases_its_drained_list() {
        let ix = index();
        let wide = |n: Ex| vec![(1..=n).map(|a| side(&[a])).collect::<Vec<_>>()];
        let run = |lists: &[Vec<Side>], out: &mut Charges| {
            let mut st = State::default();
            fold(
                &ix,
                &mut st,
                lists,
                side(&[]),
                "test",
                false,
                false,
                0,
                out,
                &NoopDerive,
            )
        };
        // a step of 2,048 unions, which ends at a check of the bounds, charges each
        // union's two copies, which gives the bytes of one copy
        let mut out = Charges::default();
        assert!(run(&wide(2048), &mut out));
        let unit = out.reserved / 4096;
        // 6,000 unions hold at most 8,194 copies at once, the two of the 4,097 kept
        // before the step turns to deriving and one of each union after it
        let mut out = Charges {
            budget: Some(9000 * unit),
            ..Charges::default()
        };
        let ok = run(&wide(6000), &mut out);
        assert!(
            ok,
            "the drained list stays charged: unit {unit}, reserved {}, released {}",
            out.reserved, out.released
        );
        assert_eq!(out.reserved, out.released);
    }

    /// A step that ends between two checks of the bounds drops its record from what it
    /// has still to charge, so that the next step's check charges only what lives.
    #[test]
    fn a_step_drops_its_record_from_what_it_has_to_charge() {
        let ix = index();
        let mut st = State::default();
        let four: Vec<Side> = (0..128)
            .map(|k| side(&[4 * k + 1, 4 * k + 2, 4 * k + 3, 4 * k + 4]))
            .collect();
        let lists = vec![four, vec![side(&[])]];
        // the first step's 128 sides of four literals hold 10,240 bytes once its record
        // goes, and the second step's record (hash table + copies), list and prune buffer
        // fit well below the cumulative sum of both steps' records
        let mut out = Charges {
            budget: Some(52_000),
            ..Charges::default()
        };
        assert!(fold(
            &ix,
            &mut st,
            &lists,
            side(&[]),
            "test",
            false,
            false,
            0,
            &mut out,
            &NoopDerive
        ));
        assert_eq!(out.reserved, out.released);
    }

    /// A product's sets are charged once they pass the slack, also when the product has
    /// fewer choices than a check of the bounds, and each step releases what it drops, so
    /// that the charge follows the sets that live and ends at zero.
    #[test]
    fn a_product_charges_its_wide_steps_and_releases_what_they_drop() {
        let wide: Vec<Ex> = (10..10 + RECORD_SLACK as Ex).collect();
        let lists = vec![vec![side(&wide)], vec![side(&[1])], vec![side(&[2])]];
        let mut out = Charges::default();
        let got = folded(&lists, &mut out);
        assert_eq!(got.len(), 1);
        assert!(out.reserved > RECORD_SLACK, "{}", out.reserved);
        assert_eq!(out.reserved, out.released);
        assert!(out.releases > 1, "the steps release nothing before the end");
    }

    /// A context whose minimum waits for the units has work left: a run that stops there
    /// and answers in part must not report the context closed, since the minimum's
    /// witnesses can still make it unsatisfiable.
    #[test]
    fn a_context_with_a_waiting_minimum_is_not_idle() {
        let mut st = DlSt::default();
        assert!(st.is_idle());
        st.pending.push(7);
        assert!(!st.is_idle());
        st.pending.clear();
        assert!(st.is_idle());
    }

    /// Four inferences too wide to make, between two checks of the bound, leave the run
    /// past its bound. Each added a quarter of the count's range before, so that four of
    /// them after four or more conclusions wrapped the count to a small number, and the
    /// run ended as complete without those inferences.
    #[test]
    fn four_inferences_too_wide_leave_the_run_past_its_bound() {
        let run = DlRun::default();
        run.set_work_limit(1000);
        run.work.fetch_add(100, Ordering::Relaxed);
        assert!(!run.over_work());
        for _ in 0..4 {
            run.exhaust();
        }
        assert!(run.over_work());
        assert_eq!(run.work(), 100);
    }

    /// The pruning before item 1011, which searched each literal.
    fn prune_by_search(l: &mut Vec<Side>) {
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

    #[test]
    fn pruning_keeps_the_sides_that_a_search_of_each_literal_keeps() {
        // sides over few literals, so that many contain others, sorted or not, with
        // repeats and empty halves, and more literals than a signature has bits
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for round in 0..2000 {
            let atoms = if round % 2 == 0 { 6 } else { 90 };
            let lits = |next: &mut dyn FnMut(u64) -> u64| -> Vec<Lit> {
                let n = next(5) as usize;
                let mut v: Vec<Lit> = (0..n).map(|_| Lit::x(next(atoms) as Ex + 2)).collect();
                match next(3) {
                    0 => {
                        v.sort_unstable();
                        v.dedup();
                    }
                    1 => v.sort_unstable(),
                    _ => {}
                }
                v
            };
            let len = next(12) as usize;
            let list: Vec<Side> = (0..len)
                .map(|_| (lits(&mut next), lits(&mut next)))
                .collect();
            let (mut a, mut b) = (list.clone(), list);
            prune(&mut a);
            prune_by_search(&mut b);
            assert_eq!(a, b, "round {round}");
        }
    }

    #[test]
    fn a_product_wider_than_the_limit_is_too_wide() {
        let run = DlRun::default();
        let list = |n: usize| vec![unit(); n];
        let lists = vec![list(3), list(4)];
        assert!(!run.too_wide(&lists), "no limit");
        run.set_width_limit(12);
        assert!(!run.too_wide(&lists));
        run.set_width_limit(11);
        assert!(run.too_wide(&lists));
        // a product whose count passes the range of the count is too wide
        let wide = vec![list(1 << 12); 6];
        run.set_width_limit(usize::MAX);
        assert!(run.too_wide(&wide));
        // a run that starts again has no limit until it is given one
        let mut run = run;
        run.begin_run();
        assert!(!run.too_wide(&lists));
    }

    #[test]
    fn step_slack_saturates_cleanly_and_matches_the_u128_model() {
        let side_bytes = std::mem::size_of::<Side>() as u128;
        let expected = |unions: usize, listed: usize| -> usize {
            let u = unions as u128;
            let l = listed as u128;
            let table = |n: u128| -> u128 {
                let buckets = match n {
                    0 => 0,
                    1..4 => 4,
                    4..8 => 8,
                    _ => (n * 8 / 7).next_power_of_two(),
                };
                buckets * (side_bytes + 1) + 16
            };
            let grown = table(u) + if u >= 4 { table(u / 2 + 1) } else { 0 };
            let cap = if l == 0 { 0 } else { l.next_power_of_two() };
            let list = (cap + cap / 2) * side_bytes;
            ((grown - u * side_bytes) + (list - l * side_bytes)).min(usize::MAX as u128) as usize
        };
        for (u, l) in [
            (0, 0),
            (1, 1),
            (3, 3),
            (4, 4),
            (7, 7),
            (8, 8),
            (16, 16),
            (100, 100),
            (FOLD_PRUNE, FOLD_PRUNE),
            (FOLD_PRUNE + 1, FOLD_PRUNE + 1),
            (50_000, 50_000),
            (200_000_000, FOLD_PRUNE + 1),
            (usize::MAX / std::mem::size_of::<Side>() + 1, FOLD_PRUNE + 1),
            (usize::MAX, FOLD_PRUNE + 1),
            (usize::MAX, usize::MAX),
        ] {
            assert_eq!(step_slack(u, l), expected(u, l), "u={u}, l={l}");
        }
        assert_eq!(step_slack(usize::MAX, usize::MAX), usize::MAX);
        assert_eq!(step_slack(usize::MAX, FOLD_PRUNE + 1), usize::MAX);
    }

    #[test]
    fn fold_slack_saturates_on_choice_overflow_and_handles_empty_and_ground() {
        assert_eq!(Found::default().fold_slack(), 0);
        // Ground products are ignored by fold_slack.
        let mut ground = Found::default();
        ground.ground_products(
            vec![vec![side(&[1]), side(&[2])], vec![side(&[3]), side(&[4])]],
            side(&[]),
        );
        assert_eq!(ground.fold_slack(), 0);
        // Empty-list products produce no choices.
        let mut empty = Found::default();
        empty.products(vec![vec![side(&[1])], Vec::new()], side(&[]));
        assert_eq!(empty.fold_slack(), 0);
        // Choice multiplication overflowing usize must return usize::MAX.
        let mut overflow = Found::default();
        let lists: Vec<Vec<Side>> = (0..usize::BITS)
            .map(|i| vec![side(&[2 * i + 2]), side(&[2 * i + 3])])
            .collect();
        overflow.products(lists, side(&[]));
        assert_eq!(overflow.fold_slack(), usize::MAX);
        // Multiple sequential products take the maximum, not the sum.
        let mut multi = Found::default();
        multi.products(
            vec![
                vec![side(&[1]), side(&[2]), side(&[3]), side(&[4])],
                vec![side(&[5]), side(&[6]), side(&[7]), side(&[8])],
            ],
            side(&[]),
        );
        let one = multi.fold_slack();
        assert_eq!(one, step_slack(4, 4).max(step_slack(16, 16)));
        multi.products(
            vec![
                vec![side(&[11]), side(&[12]), side(&[13]), side(&[14])],
                vec![side(&[15]), side(&[16]), side(&[17]), side(&[18])],
            ],
            side(&[]),
        );
        assert_eq!(multi.fold_slack(), one);
        // 32-bit 50,000 x 4,000 example (Side = 24 B, max = u32::MAX): step 0 is 2,416,928 B
        // and step 1 (200,000,000 choices, FOLD_PRUNE + 1 listed) must saturate to u32::MAX
        // instead of collapsing to 196,584 B and letting fold_slack return 2,416,928 B.
        assert_eq!(
            step_slack_model(50_000, 50_000, 24, u32::MAX as u128),
            2_416_928
        );
        assert_eq!(
            step_slack_model(200_000_000, (FOLD_PRUNE + 1) as u128, 24, u32::MAX as u128),
            u32::MAX as u128
        );
        assert_eq!(
            fold_slack_lens([50_000, 4_000].into_iter(), 24, u32::MAX as u128),
            u32::MAX as u128
        );
        // Empty list after an overflowing prefix still represents an empty product (0).
        assert_eq!(
            fold_slack_lens([usize::MAX, 2, 0].into_iter(), 48, usize::MAX as u128),
            0
        );
    }

    #[test]
    fn for_each_reaching_merges_sorted_unique_and_handles_early_stop() {
        let mut dx = DlIx::default();
        // `by_succ[10]` is strictly ascending as built by `DlIx::build`.
        dx.by_succ.insert(10, vec![2, 5, 8, 12, 15]);
        // Hypothetical successor `20` should ignore `by_succ`.
        dx.of_hyp.insert(20, 7);
        dx.by_succ.insert(20, vec![1, 3]);

        let mut st = State::default();
        // Active unit edges: 2, 8, 15 (5 and 12 are absent from `subs`).
        st.subs.insert(2);
        st.subs.insert(8);
        st.subs.insert(15);
        // Insert explicit edges out of order across multiple targets, including an overlap (8).
        let x = ds(&mut st);
        x.insert_edge(99, 30);
        x.insert_edge(14, 10);
        x.insert_edge(8, 10);
        x.insert_edge(4, 10);
        x.insert_edge(7, 20);
        x.insert_edge(1, 5);
        x.insert_edge(14, 10); // duplicate must be ignored

        let mut visited = Vec::new();
        let completed = for_each_reaching(&dx, &st, 10, |e| {
            visited.push(e);
            true
        });
        assert!(completed);
        assert_eq!(visited, vec![2, 4, 8, 14, 15]);

        // Early stop after second element.
        visited.clear();
        let completed = for_each_reaching(&dx, &st, 10, |e| {
            visited.push(e);
            visited.len() < 2
        });
        assert!(!completed);
        assert_eq!(visited, vec![2, 4]);

        // Hypothetical successor visits only explicit edges to `20`, ignoring `by_succ[20]`.
        visited.clear();
        let completed = for_each_reaching(&dx, &st, 20, |e| {
            visited.push(e);
            true
        });
        assert!(completed);
        assert_eq!(visited, vec![7]);

        // Target with no explicit edges and matching units still returns units cleanly.
        dx.by_succ.insert(40, vec![2, 15]);
        visited.clear();
        assert!(for_each_reaching(&dx, &st, 40, |e| {
            visited.push(e);
            true
        }));
        assert_eq!(visited, vec![2, 15]);
    }
}
