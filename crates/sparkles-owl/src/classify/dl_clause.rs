//! The context clauses of roadmap item 603's Tier D: their literals, the context order, the
//! run's table of the clauses that contexts send one another, and a context's clause store
//! with its indexes and the elimination of subsumed clauses.
//!
//! A context clause `Γ → Δ` (JAIR 2018, Definition 1) has a body of atoms over the context's
//! variable x and of role atoms that relate x to its predecessor y, and a head of literals
//! over x, y and the successor terms `f_e(x)`, one function symbol for each existential
//! restriction e of the index. A role literal stands for its role's closure under the
//! role hierarchy, which is closed under inverses: `RY(s)` in a body says that y reaches x by
//! a role below s, and `RF(e)` in a head that x reaches `f_e(x)` by e's role.
//!
//! The context order (Definition 3, with condition C2 of Theorem 9 in every context): the
//! literals over y are greater than none; a named class over x is above only itself over y,
//! and named classes are mutually incomparable, so that every context's query atoms are
//! smallest; the other atoms over x are above every named one and ordered by id; and the
//! literals over successor terms are above every literal over x, ordered by function symbol,
//! with `RF` above `F` within one symbol, and `F` literals by atom.

use super::index::{Ex, Index, Node, Role};
use super::saturate::Sink;
#[cfg(test)]
use super::saturate::table_bytes;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// Bucket count of a hashbrown table with `capacity` usable slots.
#[inline]
fn hash_buckets(capacity: usize) -> Option<usize> {
    if capacity == 0 {
        Some(0)
    } else if capacity < 4 {
        Some(4)
    } else if capacity < 8 {
        Some(8)
    } else {
        capacity
            .checked_mul(8)?
            .checked_div(7)?
            .checked_next_power_of_two()
    }
}

/// Control-byte SIMD group width of hashbrown on the current target: 16 bytes with SSE2 or
/// AArch64 NEON, 8 bytes on other 64-bit targets, and 4 bytes on other 32-bit targets.
const HASH_GROUP_WIDTH: usize = if cfg!(any(
    target_feature = "sse2",
    target_arch = "x86_64",
    target_arch = "aarch64"
)) {
    16
} else if cfg!(target_pointer_width = "64") {
    8
} else {
    4
};

/// Allocation size of a hashbrown table with `buckets` slots of `entry_size` bytes plus
/// its control bytes and group clone trailer.
#[inline]
fn table_layout_bytes(buckets: usize, entry_size: usize) -> Option<usize> {
    if buckets == 0 {
        return Some(0);
    }
    let data = buckets.checked_mul(entry_size)?;
    let align_mask = HASH_GROUP_WIDTH - 1;
    let ctrl_offset = data.checked_add(align_mask)? & !align_mask;
    ctrl_offset
        .checked_add(buckets)?
        .checked_add(HASH_GROUP_WIDTH)
}

/// Exact heap allocation size of a `FxHashMap`/`FxHashSet` with `capacity` slots of
/// `entry_size` bytes, returning `None` on arithmetic overflow.
#[inline]
pub(super) fn checked_hash_table_bytes(capacity: usize, entry_size: usize) -> Option<usize> {
    hash_buckets(capacity).and_then(|b| table_layout_bytes(b, entry_size))
}

/// Exact heap allocation size of a `FxHashMap`/`FxHashSet` with `capacity` slots of
/// `entry_size` bytes.
#[inline]
pub(super) fn hash_table_bytes(capacity: usize, entry_size: usize) -> usize {
    checked_hash_table_bytes(capacity, entry_size).unwrap_or(usize::MAX)
}

/// Allocation size of the next hashbrown table when a table at `capacity` rehashes.
#[inline]
pub(super) fn next_hash_table_bytes(capacity: usize, entry_size: usize) -> Option<usize> {
    let cur = hash_buckets(capacity)?;
    let next = if cur == 0 { 4 } else { cur.checked_mul(2)? };
    table_layout_bytes(next, entry_size)
}

/// Upper bound on peak new-table allocation when inserting `add` new keys into a hashbrown
/// table with current `len`, usable capacity `usable_cap` (`items + growth_left`), and
/// physical table capacity `phys_cap` (high-water `capacity()` of the live backing table,
/// which stays unchanged when in-place `remove`/`retain` leaves tombstones).
pub(super) fn hash_table_grow_bound_with_phys(
    len: usize,
    usable_cap: usize,
    phys_cap: usize,
    add: usize,
    entry_size: usize,
) -> Option<usize> {
    if add == 0 {
        return Some(0);
    }
    let end = len.checked_add(add)?;
    if end <= usable_cap {
        return Some(0);
    }
    let cur = hash_buckets(phys_cap.max(usable_cap))?;
    let next = if cur == 0 { 4 } else { cur.checked_mul(2)? };
    let target = hash_buckets(end)?.max(next);
    let full = table_layout_bytes(target, entry_size)?;
    if target > next {
        full.checked_add(table_layout_bytes(target / 2, entry_size)?)
    } else {
        Some(full)
    }
}

/// Upper bound on peak new-table allocation when inserting `add` new keys into a hashbrown
/// table with current `len` and `cap` (when no tombstones lowered `cap` below physical size).
pub(super) fn hash_table_grow_bound(
    len: usize,
    cap: usize,
    add: usize,
    entry_size: usize,
) -> Option<usize> {
    hash_table_grow_bound_with_phys(len, cap, cap, add, entry_size)
}

/// Next capacity of a `Vec<T>` when `len == cap` (matching `alloc::raw_vec` growth).
#[inline]
pub(super) fn next_vec_cap(cap: usize, elem_size: usize) -> Option<usize> {
    let min_cap = if elem_size == 1 {
        8
    } else if elem_size <= 1024 {
        4
    } else {
        1
    };
    if cap == 0 {
        Some(min_cap)
    } else {
        cap.checked_mul(2).map(|doubled| doubled.max(min_cap))
    }
}

/// Upper bound on the new buffer allocation when pushing one element to `v` (0 when spare
/// capacity remains, full new buffer size when reallocating).
#[inline]
pub(super) fn vec_grow_bound<T>(v: &Vec<T>) -> Option<usize> {
    if v.len() < v.capacity() {
        Some(0)
    } else {
        next_vec_cap(v.capacity(), std::mem::size_of::<T>())?.checked_mul(std::mem::size_of::<T>())
    }
}

/// Pushes `x` to `v` and returns the exact retained capacity increase in bytes.
#[inline]
pub(super) fn vec_push_delta<T>(v: &mut Vec<T>, x: T) -> usize {
    let before = v.capacity();
    v.push(x);
    (v.capacity() - before) * std::mem::size_of::<T>()
}

/// Upper bound on peak allocation when inserting `x` into `set` (0 when already present or
/// spare capacity remains).
#[inline]
pub(super) fn set_insert_bound<T: Eq + std::hash::Hash>(s: &FxHashSet<T>, x: &T) -> Option<usize> {
    if s.contains(x) || s.len() < s.capacity() {
        Some(0)
    } else {
        next_hash_table_bytes(s.capacity(), std::mem::size_of::<T>())
    }
}

/// Inserts `x` into `set` and returns `Some(delta_bytes)` when `x` was new, `None` when
/// already present.
#[inline]
pub(super) fn set_insert_delta<T: Eq + std::hash::Hash>(
    s: &mut FxHashSet<T>,
    x: T,
) -> Option<usize> {
    let before = s.capacity();
    if !s.insert(x) {
        return None;
    }
    let sz = std::mem::size_of::<T>();
    Some(hash_table_bytes(s.capacity(), sz).saturating_sub(hash_table_bytes(before, sz)))
}

/// Upper bound on peak allocation when pushing one element into `m.entry(k).or_default()`.
pub(super) fn map_vec_push_bound<K: Eq + std::hash::Hash, T>(
    m: &FxHashMap<K, Vec<T>>,
    k: &K,
) -> Option<usize> {
    if let Some(v) = m.get(k) {
        vec_grow_bound(v)
    } else {
        let map_grow = if m.len() >= m.capacity() {
            next_hash_table_bytes(m.capacity(), std::mem::size_of::<(K, Vec<T>)>())?
        } else {
            0
        };
        let vec_alloc =
            next_vec_cap(0, std::mem::size_of::<T>())?.checked_mul(std::mem::size_of::<T>())?;
        map_grow.checked_add(vec_alloc)
    }
}

/// Pushes `x` into `m.entry(k).or_default()` and returns exact retained byte growth.
#[inline]
pub(super) fn map_vec_push<K: Eq + std::hash::Hash, T>(
    m: &mut FxHashMap<K, Vec<T>>,
    k: K,
    x: T,
) -> usize {
    let entry = std::mem::size_of::<(K, Vec<T>)>();
    let before_table = hash_table_bytes(m.capacity(), entry);
    let v = m.entry(k).or_default();
    let list_delta = vec_push_delta(v, x);
    hash_table_bytes(m.capacity(), entry).saturating_sub(before_table) + list_delta
}

/// Pushes `x` into `m.entry(k).or_default()` unless it is already the last element, and
/// returns exact retained byte growth.
#[inline]
pub(super) fn map_vec_push_dedup<K: Eq + std::hash::Hash, T: PartialEq>(
    m: &mut FxHashMap<K, Vec<T>>,
    k: K,
    x: T,
) -> usize {
    let entry = std::mem::size_of::<(K, Vec<T>)>();
    let before_table = hash_table_bytes(m.capacity(), entry);
    let v = m.entry(k).or_default();
    if v.last() == Some(&x) {
        return hash_table_bytes(m.capacity(), entry).saturating_sub(before_table);
    }
    let list_delta = vec_push_delta(v, x);
    hash_table_bytes(m.capacity(), entry).saturating_sub(before_table) + list_delta
}

/// Exact retained bytes of a `FxHashMap<K, Vec<T>>` (table + inner vector capacities).
pub(super) fn map_vec_bytes<K, T>(m: &FxHashMap<K, Vec<T>>) -> usize {
    hash_table_bytes(m.capacity(), std::mem::size_of::<(K, Vec<T>)>())
        + m.values()
            .map(|v| v.capacity() * std::mem::size_of::<T>())
            .sum::<usize>()
}

/// One literal, packed: its kind in the top three bits, a function symbol in the next 29,
/// and an atom, a role or a term in the low 32. The packed order sorts a clause's literals.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Lit(u64);

const KIND_SHIFT: u32 = 61;
/// `A(y)`
pub const Y: u64 = 0;
/// `s(y, x)`: in a body read through the hierarchy, in a head the role itself
pub const RY: u64 = 1;
/// `A(x)`
pub const X: u64 = 2;
/// `A(f(x))`
pub const F: u64 = 3;
/// `s(x, f(x))`, read through the hierarchy when it is a premise
pub const RL: u64 = 4;
/// `f(x) ≈ t`, with t a smaller successor term or y
pub const EQ: u64 = 5;
/// `f(x) ≉ t`, with t a smaller successor term or y
pub const NE: u64 = 6;
/// A literal of a rooted run that the six forms above cannot hold, by its id in the run's
/// [`Lits`]: one that mentions a constant, a loop `s(x, x)`, or an equality of x and y.
pub const XL: u64 = 7;

/// A term an equality relates to a successor term: the predecessor y or a successor
/// term `g(x)`, coded as 0 for y and `g + 1`, so that the code orders the terms.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Term {
    Y,
    Fun(Ex),
}

impl Term {
    #[inline]
    fn code(self) -> u32 {
        match self {
            Term::Y => 0,
            Term::Fun(g) => g + 1,
        }
    }
    #[inline]
    fn of(code: u32) -> Term {
        if code == 0 {
            Term::Y
        } else {
            Term::Fun(code - 1)
        }
    }
}

/// What a literal of an equality between two terms is: a literal, or true or false
/// because the terms are the same.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Made {
    Lit(Lit),
    True,
    False,
}

impl Lit {
    /// The literal's bit of a signature, a union of such bits that a subset's union is
    /// within.
    #[inline]
    pub(super) fn sig_bit(self) -> u64 {
        1 << (self.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58)
    }
    #[inline]
    pub fn y(a: Ex) -> Lit {
        Lit((Y << KIND_SHIFT) | a as u64)
    }
    #[inline]
    pub fn ry(s: Role) -> Lit {
        Lit((RY << KIND_SHIFT) | s as u64)
    }
    #[inline]
    pub fn x(a: Ex) -> Lit {
        Lit((X << KIND_SHIFT) | a as u64)
    }
    #[inline]
    pub fn f(e: Ex, a: Ex) -> Lit {
        debug_assert!(e < 1 << 29);
        Lit((F << KIND_SHIFT) | ((e as u64) << 32) | a as u64)
    }
    /// `s(x, f(x))`
    #[inline]
    pub fn rl(f: Ex, s: Role) -> Lit {
        debug_assert!(f < 1 << 29);
        Lit((RL << KIND_SHIFT) | ((f as u64) << 32) | s as u64)
    }
    fn pair(kind: u64, a: Term, b: Term) -> Made {
        let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
        match hi {
            _ if a == b => {
                if kind == EQ {
                    Made::True
                } else {
                    Made::False
                }
            }
            Term::Fun(f) => {
                debug_assert!(f < 1 << 29);
                Made::Lit(Lit((kind << KIND_SHIFT)
                    | ((f as u64) << 32)
                    | lo.code() as u64))
            }
            // both y
            Term::Y => unreachable!("two different terms of which y is the greater"),
        }
    }
    /// `a ≈ b`, oriented with the greater term first
    #[inline]
    pub fn eq(a: Term, b: Term) -> Made {
        Lit::pair(EQ, a, b)
    }
    /// `a ≉ b`, oriented with the greater term first
    #[inline]
    pub fn ne(a: Term, b: Term) -> Made {
        Lit::pair(NE, a, b)
    }
    #[inline]
    pub fn kind(self) -> u64 {
        self.0 >> KIND_SHIFT
    }
    /// the atom, or the role of an `RY` or `RL` literal
    #[inline]
    pub fn atom(self) -> u32 {
        self.0 as u32
    }
    /// the function symbol of a literal over a successor term
    #[inline]
    pub fn fun(self) -> Ex {
        ((self.0 >> 32) & ((1 << 29) - 1)) as Ex
    }
    /// the smaller term of an equality or inequality
    #[inline]
    pub fn term(self) -> Term {
        Term::of(self.atom())
    }
    /// The key of the total order on the literals over successor terms, which is the
    /// lexicographic path order with the function symbols above the predicates: the
    /// greatest function symbol, then `RL ≻ F ≻ NE ≻ EQ`, then the role, the atom or
    /// the smaller term.
    #[inline]
    fn succ_key(self) -> (Ex, u8, u32) {
        let k = match self.kind() {
            EQ => 0,
            NE => 1,
            F => 2,
            _ => 3,
        };
        (self.fun(), k, self.atom())
    }
    /// The literal of a rooted run with id `id` in the run's [`Lits`].
    #[inline]
    pub(super) fn xl(id: u32) -> Lit {
        Lit((XL << KIND_SHIFT) | id as u64)
    }
    /// The id in the run's [`Lits`] of a literal of kind [`XL`].
    #[inline]
    pub fn xid(self) -> u32 {
        debug_assert_eq!(self.kind(), XL);
        self.0 as u32
    }
}

/// A term of a rooted run: the context's variable x, the predecessor y, a successor term
/// `f_e(x)`, a constant, or a successor term `f_e(u)` of a constant in the root context.
/// Constants are numbered by the index's individuals and then those the run makes for its
/// own encoding. The derived order is only the canonical order of a literal's terms.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum RTerm {
    X,
    Y,
    Fx(Ex),
    C(u32),
    Fc(Ex, u32),
}

/// A literal of a rooted run in its general form: an atom `B(t)`, a role atom `S(s, t)`, an
/// equality or an inequality. Two terms of an equality or a role atom are kept in a
/// canonical order, a role atom's by its inverse, so that one literal has one form.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum XLit {
    Atom(Ex, RTerm),
    Role(Role, RTerm, RTerm),
    Eq(RTerm, RTerm),
    Ne(RTerm, RTerm),
}

impl XLit {
    /// The canonical form: an equality's terms and a role atom's in order, the role atom
    /// read by the inverse role when it swaps them, which `inv` gives, and a loop by the
    /// smaller of the role and its inverse.
    pub fn canon(self, inv: impl Fn(Role) -> Option<Role>) -> XLit {
        match self {
            XLit::Role(s, a, b) if a > b => match inv(s) {
                Some(i) => XLit::Role(i, b, a),
                None => self,
            },
            XLit::Role(s, a, b) if a == b => match inv(s) {
                Some(i) if i < s => XLit::Role(i, a, b),
                _ => self,
            },
            XLit::Eq(a, b) if a > b => XLit::Eq(b, a),
            XLit::Ne(a, b) if a > b => XLit::Ne(b, a),
            _ => self,
        }
    }

    /// Whether the literal is ground: its terms are constants.
    pub fn is_ground(self) -> bool {
        let (ts, n) = terms_of(self);
        ts[..n].iter().all(|t| matches!(t, RTerm::C(_)))
    }
}

/// The literals of a rooted run that the packed forms cannot hold, interned: a literal of
/// kind [`XL`] holds its id here. Entries never move, so a worker reads one without a lock,
/// and only a new literal takes the lock of the map.
#[derive(Default)]
pub struct Lits {
    ids: parking_lot::Mutex<FxHashMap<XLit, u32>>,
    recs: Segs<XLit>,
    next: AtomicU32,
    /// Exact total bytes of the map and segments (updated by every insertion).
    bytes: AtomicUsize,
    /// Bytes added by un-admitted [`Lits::lit`] insertions (or carried from an earlier run),
    /// counted by [`super::dl_rules::DlRun::bytes`]. Admitted insertions via
    /// [`Lits::lit_admitted`] / [`admit_lit`] keep their bytes inside `Run::memory` so live
    /// memory checks never observe a two-counter transfer double count.
    unadmitted_bytes: AtomicUsize,
}

#[cfg(test)]
thread_local! {
    pub(super) static BEFORE_LIT_REVALIDATE: std::cell::RefCell<Option<std::sync::Arc<std::sync::Barrier>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn before_lit_revalidate() {
    let barrier = BEFORE_LIT_REVALIDATE.with(|slot| slot.borrow().clone());
    if let Some(b) = barrier {
        b.wait();
    }
}

/// Checks whether `l` can be packed or simplified without touching the interner.
#[inline]
fn packed_xlit(l: XLit, inv: impl Fn(Role) -> Option<Role> + Copy) -> Option<Made> {
    use RTerm::*;
    match l {
        XLit::Atom(a, X) => Some(Made::Lit(Lit::x(a))),
        XLit::Atom(a, Y) => Some(Made::Lit(Lit::y(a))),
        XLit::Atom(a, Fx(e)) => Some(Made::Lit(Lit::f(e, a))),
        XLit::Role(s, Y, X) => Some(Made::Lit(Lit::ry(s))),
        XLit::Role(s, X, Y) => inv(s).map(|i| Made::Lit(Lit::ry(i))),
        XLit::Role(s, X, Fx(f)) => Some(Made::Lit(Lit::rl(f, s))),
        XLit::Role(s, Fx(f), X) => inv(s).map(|i| Made::Lit(Lit::rl(f, i))),
        XLit::Eq(a, b) if a == b => Some(Made::True),
        XLit::Ne(a, b) if a == b => Some(Made::False),
        XLit::Eq(a, b) => packed_pair(EQ, a, b).map(Made::Lit),
        XLit::Ne(a, b) => packed_pair(NE, a, b).map(Made::Lit),
        _ => None,
    }
}

/// Shared admission-and-revalidate loop for [`Lits::lit_admitted`] and [`admit_lit`]. Caller
/// callbacks (`out.admit_dl`, `out.stopped`, `out.commit_dl`) always run outside `ids.lock()`.
fn lit_admitted_loop<S: Sink>(
    try_insert: impl Fn(&S, usize) -> Result<(Made, usize), Option<usize>>,
    out: &mut S,
) -> Option<Made> {
    let mut guard = super::saturate::DlGuard::new(out);
    loop {
        if guard.stopped() {
            return None;
        }
        match try_insert(guard.sink(), guard.bytes()) {
            Ok((made, grown)) => {
                guard.commit(grown);
                return Some(made);
            }
            Err(None) => {
                guard.speculative_overflow();
                return None;
            }
            Err(Some(need)) => {
                debug_assert!(need > guard.bytes());
                let add = need - guard.bytes();
                if !guard.admit_add(add) {
                    return None;
                }
                #[cfg(test)]
                before_lit_revalidate();
            }
        }
    }
}

/// Interns `l` in `out.dl().lits`, pre-admitting and revalidating any hash-table rehash or
/// segment allocation against `out` before growing `Lits`. Returns `None` when the run stops
/// or refuses the growth, publishing neither a literal nor a retained allocation.
#[inline]
pub fn admit_lit<S: Sink>(
    l: XLit,
    inv: impl Fn(Role) -> Option<Role> + Copy,
    out: &mut S,
) -> Option<Made> {
    if out.stopped() {
        return None;
    }
    if let Some(m) = packed_xlit(l, inv) {
        return Some(m);
    }
    let canon = l.canon(inv);
    lit_admitted_loop(
        |sink, budget| sink.dl().lits.lit_try_insert_canon(canon, budget),
        out,
    )
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the rooted rules of item 603 phase 6 make it")
)]
impl Lits {
    /// Looks up `l` without allocating or mutating the interner, returning `Some(lit)` when
    /// it packs or is already interned.
    pub fn get(&self, l: XLit, inv: impl Fn(Role) -> Option<Role> + Copy) -> Option<Lit> {
        if let Some(m) = packed_xlit(l, inv) {
            return match m {
                Made::Lit(l) => Some(l),
                Made::True | Made::False => None,
            };
        }
        let l = l.canon(inv);
        self.ids.lock().get(&l).copied().map(Lit::xl)
    }

    /// The literal of `l`, packed when one of the six packed forms holds it and interned
    /// otherwise, or true or false for an equality or inequality of a term with itself.
    /// `inv` gives a role's inverse.
    pub fn lit(&self, l: XLit, inv: impl Fn(Role) -> Option<Role> + Copy) -> Made {
        if let Some(m) = packed_xlit(l, inv) {
            return m;
        }
        let l = l.canon(inv);
        let mut ids = self.ids.lock();
        let cap = ids.capacity();
        let mut seg_grown = 0usize;
        let id = *ids.entry(l).or_insert_with(|| {
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            let (slot, g) = self.recs.slot_with_delta(id);
            seg_grown = g;
            let _ = slot.set(l);
            id
        });
        let entry = std::mem::size_of::<(XLit, u32)>();
        let map_grown =
            hash_table_bytes(ids.capacity(), entry).saturating_sub(hash_table_bytes(cap, entry));
        let grown = seg_grown + map_grown;
        if grown > 0 {
            self.bytes.fetch_add(grown, Ordering::Relaxed);
            self.unadmitted_bytes.fetch_add(grown, Ordering::Relaxed);
        }
        Made::Lit(Lit::xl(id))
    }

    /// Attempts to look up or insert canonical `l` under `self.ids.lock()` without exceeding
    /// `budget` bytes of peak growth. Returns `Ok((made, grown))` when found or inserted, or
    /// `Err(Some(need))` (`Err(None)` on overflow) without inserting or allocating when
    /// `need > budget`.
    fn lit_try_insert_canon(&self, l: XLit, budget: usize) -> Result<(Made, usize), Option<usize>> {
        let entry = std::mem::size_of::<(XLit, u32)>();
        let mut ids = self.ids.lock();
        if let Some(&id) = ids.get(&l) {
            return Ok((Made::Lit(Lit::xl(id)), 0));
        }
        let map_grow = if ids.len() >= ids.capacity() {
            next_hash_table_bytes(ids.capacity(), entry)
        } else {
            Some(0)
        };
        let next_id = self.next.load(Ordering::Relaxed);
        let seg_grow = self.recs.next_seg_bytes(next_id);
        let Some(need) = map_grow.and_then(|m| m.checked_add(seg_grow)) else {
            return Err(None);
        };
        if need > budget {
            return Err(Some(need));
        }
        let cap = ids.capacity();
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        debug_assert_eq!(id, next_id);
        let (slot, seg_grown) = self.recs.slot_with_delta(id);
        let _ = slot.set(l);
        ids.insert(l, id);
        let map_grown =
            hash_table_bytes(ids.capacity(), entry).saturating_sub(hash_table_bytes(cap, entry));
        let grown = seg_grown + map_grown;
        debug_assert!(
            grown <= budget,
            "Lits growth {grown} exceeded revalidated budget {budget}"
        );
        if grown > 0 {
            self.bytes.fetch_add(grown, Ordering::Relaxed);
        }
        Ok((Made::Lit(Lit::xl(id)), grown))
    }

    /// [`Lits::lit`], pre-admitting and revalidating any hash-table rehash or new segment
    /// allocation against `out` before growing `self`. Returns `None` when the run stops or
    /// refuses the growth, publishing neither a literal nor a retained allocation.
    pub fn lit_admitted<S: Sink>(
        &self,
        l: XLit,
        inv: impl Fn(Role) -> Option<Role> + Copy,
        out: &mut S,
    ) -> Option<Made> {
        if out.stopped() {
            return None;
        }
        if let Some(m) = packed_xlit(l, inv) {
            return Some(m);
        }
        let canon = l.canon(inv);
        lit_admitted_loop(|_, budget| self.lit_try_insert_canon(canon, budget), out)
    }

    /// Bytes of `self` not accounted for in `Run::memory`, for [`super::dl_rules::DlRun::bytes`].
    pub(super) fn unadmitted_bytes(&self) -> usize {
        self.unadmitted_bytes.load(Ordering::Relaxed)
    }

    /// Re-bases `unadmitted_bytes` to total `bytes` when starting a new saturation round that
    /// carries `self` forward from a previous run.
    pub(super) fn sync_unadmitted_on_begin_run(&mut self) {
        *self.unadmitted_bytes.get_mut() = *self.bytes.get_mut();
    }

    /// Whether a head holds an equality and the inequality of the same terms, in any
    /// encoding, which makes its clause a tautology.
    pub fn complementary(&self, head: &[Lit], inv: impl Fn(Role) -> Option<Role> + Copy) -> bool {
        head.iter().any(|&l| {
            matches!(l.kind(), EQ | XL)
                && matches!(self.view(l, inv), XLit::Eq(s, t)
                if head.iter().any(|&m| {
                    matches!(m.kind(), NE | XL) && self.view(m, inv) == XLit::Ne(s, t)
                }))
        })
    }

    /// [`Lits::complementary`], telling `read` of each literal of `head` that it tests,
    /// those of the inner search for an equality's inequality included, `None` once read
    /// answers false.
    pub(super) fn complementary_with(
        &self,
        head: &[Lit],
        inv: impl Fn(Role) -> Option<Role> + Copy,
        read: &mut dyn FnMut(usize) -> bool,
    ) -> Option<bool> {
        for &l in head {
            if !read(1) {
                return None;
            }
            #[cfg(test)]
            super::dl_rules::meter_probe::visit(1);
            if !matches!(l.kind(), EQ | XL) {
                continue;
            }
            let XLit::Eq(s, t) = self.view(l, inv) else {
                continue;
            };
            for &m in head {
                if !read(1) {
                    return None;
                }
                #[cfg(test)]
                super::dl_rules::meter_probe::visit(1);
                if matches!(m.kind(), NE | XL) && self.view(m, inv) == XLit::Ne(s, t) {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    /// The general form of any literal, canonical as [`XLit::canon`] makes it.
    pub fn view(&self, l: Lit, inv: impl Fn(Role) -> Option<Role>) -> XLit {
        use RTerm::Fx;
        let fun = |t: Term| match t {
            Term::Y => RTerm::Y,
            Term::Fun(g) => Fx(g),
        };
        match l.kind() {
            Y => XLit::Atom(l.atom(), RTerm::Y),
            RY => XLit::Role(l.atom(), RTerm::Y, RTerm::X).canon(inv),
            X => XLit::Atom(l.atom(), RTerm::X),
            F => XLit::Atom(l.atom(), Fx(l.fun())),
            RL => XLit::Role(l.atom(), RTerm::X, Fx(l.fun())).canon(inv),
            EQ => XLit::Eq(Fx(l.fun()), fun(l.term())).canon(inv),
            NE => XLit::Ne(Fx(l.fun()), fun(l.term())).canon(inv),
            _ => *self
                .recs
                .slot(l.xid(), &self.bytes)
                .get()
                .expect("a literal the run interned"),
        }
    }

    /// The literals interned.
    pub fn len(&self) -> usize {
        self.next.load(Ordering::Relaxed) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The bytes of the map and of the segments.
    pub fn bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }
}

/// The packed form of an equality or inequality of two terms when both are y or successor
/// terms over x and they differ.
fn packed_pair(kind: u64, a: RTerm, b: RTerm) -> Option<Lit> {
    let t = |r: RTerm| match r {
        RTerm::Y => Some(Term::Y),
        RTerm::Fx(g) => Some(Term::Fun(g)),
        _ => None,
    };
    let (a, b) = (t(a)?, t(b)?);
    match Lit::pair(kind, a, b) {
        Made::Lit(l) => Some(l),
        _ => None,
    }
}

/// The key of the total order on the atoms over x: the at-least conjunctions of item
/// 728's clauses first, so that a context resolves the fresh class and the defined class
/// before it expands them, then the fresh classes of the normalization, then the named
/// classes, then the other expressions, each by id.
#[inline]
pub fn rank(ix: &Index, a: Ex) -> (u8, Ex) {
    if ix.dl.as_deref().is_some_and(|d| d.is_low(a)) {
        return (0, a);
    }
    let r = match ix.head(a).node {
        // the name of a minimum ranks with the conjunction of its witnesses
        Node::Fresh(_) if ix.dl.as_deref().is_some_and(|d| d.is_least(a)) => 3,
        Node::Fresh(_) => 1,
        Node::Class(_) => 2,
        _ => 3,
    };
    (r, a)
}

/// A context clause: its body, then its head, each sorted.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Clause {
    lits: Box<[Lit]>,
    body: u32,
}

impl Clause {
    /// A clause from a body and a head, each sorted and without repeats here.
    pub fn new(mut body: Vec<Lit>, mut head: Vec<Lit>) -> Clause {
        body.sort_unstable();
        body.dedup();
        head.sort_unstable();
        head.dedup();
        Clause::from_sorted(body, head)
    }

    /// Builds a clause from already-sorted, deduplicated `body` and `head`. Reuses `head`
    /// without a copy when `body` is empty (and `body` when `head` is empty), and uses an
    /// exact-capacity combined buffer otherwise.
    fn from_sorted(mut body: Vec<Lit>, head: Vec<Lit>) -> Clause {
        if body.is_empty() {
            return Clause {
                lits: head.into_boxed_slice(),
                body: 0,
            };
        }
        let b = body.len() as u32;
        if head.is_empty() {
            return Clause {
                lits: body.into_boxed_slice(),
                body: b,
            };
        }
        let total = body.len() + head.len();
        if body.capacity() == total {
            body.extend(head);
            return Clause {
                lits: body.into_boxed_slice(),
                body: b,
            };
        }
        let mut lits = Vec::with_capacity(total);
        lits.extend_from_slice(&body);
        lits.extend_from_slice(&head);
        drop(body);
        drop(head);
        Clause {
            lits: lits.into_boxed_slice(),
            body: b,
        }
    }

    /// Upper bound on temporary copy/shrink allocation in [`Clause::from_sorted`].
    fn build_alloc_bound(body: &Vec<Lit>, head: &Vec<Lit>) -> Option<usize> {
        let total = body.len().checked_add(head.len())?;
        let bytes = total.checked_mul(std::mem::size_of::<Lit>())?;
        if body.is_empty() {
            Some(if head.len() == head.capacity() {
                0
            } else {
                bytes
            })
        } else if head.is_empty() {
            Some(if body.len() == body.capacity() {
                0
            } else {
                bytes
            })
        } else if body.capacity() == total {
            Some(0)
        } else {
            Some(bytes)
        }
    }
    #[inline]
    pub fn body(&self) -> &[Lit] {
        &self.lits[..self.body as usize]
    }
    #[inline]
    pub fn head(&self) -> &[Lit] {
        &self.lits[self.body as usize..]
    }
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Clause>() + self.lits.len() * 8
    }
    /// A signature of the literals: one bit for each, by a hash, so that a clause whose
    /// signature has a bit that another's lacks does not subsume it.
    pub fn sig(&self) -> u64 {
        self.lits.iter().fold(0, |s, l| s | l.sig_bit())
    }
    /// Whether this clause subsumes `other`: its body and its head are subsets of
    /// `other`'s.
    pub fn subsumes(&self, other: &Clause) -> bool {
        subset(self.body(), other.body()) && subset(self.head(), other.head())
    }
    /// The maximal literals of the head under the order of a rooted run: each literal that
    /// no other literal of the head is above, without the atoms over y alone, in the head's
    /// order. Empty when no literal of the head but those atoms is maximal.
    pub fn maximal_rooted(&self, o: &Rooted, out: &mut Vec<Lit>) {
        out.clear();
        let head = self.head();
        let views: Vec<XLit> = head.iter().map(|&l| o.view(l)).collect();
        for (i, &v) in views.iter().enumerate() {
            if Rooted::over_y(v) {
                continue;
            }
            if !views.iter().any(|&w| o.gt(w, v)) {
                out.push(head[i]);
            }
        }
    }

    /// The maximal literals of the head under the context order: the one literal over a
    /// successor term that is greatest, else the greatest atom over x. Empty when the head
    /// is over y alone.
    pub fn maximal(&self, ix: &Index, out: &mut Vec<Lit>) {
        out.clear();
        let head = self.head();
        if let Some(&m) = head
            .iter()
            .filter(|l| matches!(l.kind(), F | RL | EQ | NE))
            .max_by_key(|l| l.succ_key())
        {
            out.push(m);
            return;
        }
        if let Some(&m) = head
            .iter()
            .filter(|l| l.kind() == X)
            .max_by_key(|l| rank(ix, l.atom()))
        {
            out.push(m);
        }
    }
}

/// The context order of a rooted run, admissible in the sense of Definition 7 of Tena Cucala,
/// Cuenca Grau and Horrocks (*Pay-As-You-Go Consequence-Based Reasoning for the Description
/// Logic SROIQ*, Artificial Intelligence 2021), with the a-admissible order on function
/// symbols and constants that puts every function symbol above every constant and orders
/// each kind by id. Every context of the run uses it, the root's included.
///
/// On a-terms it is the lexicographic path order with `f(x) ≻ g(x)` for f above g, `f(x)`
/// above x, y and every constant, `x ≻ y`, constants by id, and `f(u) ≻ g(v)` when f is above
/// g or f is g and u is above v. x and y are incomparable with every constant (Definition 7,
/// condition 3). An atom is above x, y and every term no greater than one of its own terms,
/// and no term is above an atom. A literal is the multiset of its terms, `{A, true}` for an
/// atom, `{s, t}` for `s ≈ t` and `{s, s, t, t}` for `s ≉ t`, and literals are compared by the
/// multiset extension.
///
/// Atoms fall into four groups. An atom over a successor term, `f(x)` or `f(u)`, is above
/// every atom without one, and such atoms are ordered as the runs without constants order
/// them: by the greatest function symbol, then its constant, the role atom above the atom
/// over the term, and then by predicate and terms. Of the other atoms, an atom over a
/// constant is above only the atoms that replacing its terms by smaller ones gives, which
/// condition 5 asks of a function-free atom that mentions a constant. The atoms over x
/// alone, `B(x)` and the loops `S(x, x)`, are above every atom that mentions y without a
/// constant or a successor term. Among them the named classes are mutually incomparable and
/// above no other atom over x, and the others are above every named class and ordered by
/// [`rank`], the loops last. The atoms that mention y alone are above only what condition 5
/// allows. So a named class over x is above only x, true and literals that mention y, which
/// is condition C2 of the journal's Theorem 2 for every query of a named class, and a
/// context's units of named classes are complete without the candidate tests.
pub struct Rooted<'a> {
    pub ix: &'a Index,
    pub lits: &'a Lits,
    /// a role's inverse, the qualified roles' included
    pub inv: &'a (dyn Fn(Role) -> Option<Role> + Sync),
    /// whether two ground literals compare by the one order every context shares (F24,
    /// item 804's second departure), which puts a role atom above a class atom, a
    /// predicate above every equality and inequality of constants, and otherwise compares
    /// by the predicate's id and then the constants from the left
    pub ground_total: bool,
}

#[cfg(test)]
thread_local! {
    static GROUND_TOTAL_TEST: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Temporarily overrides [`ground_total`] on the current thread for deterministic tests.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn with_ground_total<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            GROUND_TOTAL_TEST.with(|c| c.set(self.0));
        }
    }
    let prev = GROUND_TOTAL_TEST.with(|c| c.replace(Some(on)));
    let _reset = Reset(prev);
    f()
}

/// Whether every context compares two ground literals by one shared order, item 804's
/// second departure. `SPARKLES_ROOTED_GROUND_ORDER=0` turns it off, for measurement.
pub fn ground_total() -> bool {
    #[cfg(test)]
    if let Some(v) = GROUND_TOTAL_TEST.with(|c| c.get()) {
        return v;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_GROUND_ORDER").is_none_or(|v| v != "0"))
}

/// A term of the rooted order: true, an a-term or an atom.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OTerm {
    True,
    A(RTerm),
    P(XLit),
}

/// The groups of the rooted order's atoms, as [`Rooted`] describes them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Group {
    Succ,
    Const,
    X,
    Y,
}

impl Rooted<'_> {
    /// The general form of a literal.
    #[inline]
    pub fn view(&self, l: Lit) -> XLit {
        self.lits.view(l, self.inv)
    }

    /// `s ≻ t` on a-terms.
    pub fn a_gt(s: RTerm, t: RTerm) -> bool {
        use RTerm::*;
        match (s, t) {
            (Fx(f), Fx(g)) => f > g,
            (Fx(_), X | Y | C(_)) => true,
            (X, Y) => true,
            (C(u), C(v)) => u > v,
            (Fc(f, u), Fc(g, v)) => f > g || f == g && u > v,
            (Fc(..), C(_)) => true,
            _ => false,
        }
    }

    fn a_ge(s: RTerm, t: RTerm) -> bool {
        s == t || Rooted::a_gt(s, t)
    }

    /// The terms of an atom.
    fn args(p: XLit) -> ([RTerm; 2], usize) {
        match p {
            XLit::Atom(_, a) => ([a, a], 1),
            XLit::Role(_, a, b) => ([a, b], 2),
            _ => unreachable!("an atom"),
        }
    }

    fn group(p: XLit) -> Group {
        let (ts, n) = Rooted::args(p);
        let ts = &ts[..n];
        if ts.iter().any(|t| matches!(t, RTerm::Fx(_) | RTerm::Fc(..))) {
            Group::Succ
        } else if ts.iter().any(|t| matches!(t, RTerm::C(_))) {
            Group::Const
        } else if ts.iter().all(|&t| t == RTerm::X) {
            Group::X
        } else {
            Group::Y
        }
    }

    /// The key of the total order on the atoms over a successor term: the greatest
    /// successor term, the role atom above the atom, the predicate, then the terms.
    fn succ_key(p: XLit) -> (Ex, u32, u8, u32, [u64; 2]) {
        let code = |t: RTerm| -> u64 {
            match t {
                RTerm::Y => 0,
                RTerm::X => 1,
                RTerm::C(u) => 2 + u as u64,
                RTerm::Fx(f) => (1 << 33) + f as u64,
                RTerm::Fc(f, u) => (1 << 34) + ((f as u64) << 32 | u as u64),
            }
        };
        let (ts, n) = Rooted::args(p);
        let mut top = (0, 0);
        for &t in &ts[..n] {
            let k = match t {
                RTerm::Fx(f) => (f, 0),
                RTerm::Fc(f, u) => (f, u),
                _ => continue,
            };
            top = top.max(k);
        }
        match p {
            XLit::Atom(b, a) => (top.0, top.1, 0, b, [code(a), 0]),
            XLit::Role(s, a, b) => (top.0, top.1, 1, s, [code(a), code(b)]),
            _ => unreachable!("an atom"),
        }
    }

    /// Whether atom q is atom p with its terms replaced by smaller ones (Definition 7,
    /// condition 5, and the monotonicity of the order).
    fn smaller_terms(&self, p: XLit, q: XLit) -> bool {
        let pointwise =
            |a: &[RTerm], b: &[RTerm]| a.iter().zip(b).all(|(&s, &t)| Rooted::a_ge(s, t)) && a != b;
        match (p, q) {
            (XLit::Atom(b, s), XLit::Atom(c, t)) => b == c && pointwise(&[s], &[t]),
            (XLit::Role(r, a, b), XLit::Role(s, c, d)) => {
                (r == s && pointwise(&[a, b], &[c, d]))
                    || ((self.inv)(s) == Some(r) && pointwise(&[a, b], &[d, c]))
            }
            _ => false,
        }
    }

    /// `p ≻ q` on atoms.
    fn p_gt(&self, p: XLit, q: XLit) -> bool {
        use Group::*;
        let (gp, gq) = (Rooted::group(p), Rooted::group(q));
        match (gp, gq) {
            (Succ, Succ) => Rooted::succ_key(p) > Rooted::succ_key(q),
            (Succ, _) => true,
            (X, Y) => true,
            (X, X) => self.x_key(p) > self.x_key(q),
            (Const, Const) | (Y, Y) => self.smaller_terms(p, q),
            _ => false,
        }
    }

    /// The key of the order on the atoms over x alone: none for a named class, which is
    /// above no other, else above every named class by [`rank`], the loops last.
    fn x_key(&self, p: XLit) -> Option<(u8, Ex)> {
        match p {
            XLit::Atom(b, _) => {
                // a rooted run ranks no atom lowest, so the ranks are 1, 2 and 3
                let (r, b) = rank(self.ix, b);
                (r != 2).then_some((r, b))
            }
            XLit::Role(s, ..) => Some((4, s)),
            _ => None,
        }
    }

    /// `s ≻ t` on the terms of the rooted order.
    fn t_gt(&self, s: OTerm, t: OTerm) -> bool {
        match (s, t) {
            (OTerm::True, _) => false,
            (_, OTerm::True) => true,
            (OTerm::A(a), OTerm::A(b)) => Rooted::a_gt(a, b),
            (OTerm::A(_), OTerm::P(_)) => false,
            (OTerm::P(p), OTerm::A(t)) => {
                let (ts, n) = Rooted::args(p);
                matches!(t, RTerm::X | RTerm::Y)
                    || ts[..n].iter().any(|&a| Rooted::a_ge(a, t))
                    // with the shared order a predicate is above every constant
                    || self.ground_total && matches!(t, RTerm::C(_)) && Rooted::plain_ground(p)
            }
            (OTerm::P(p), OTerm::P(q)) => p != q && self.p_gt(p, q),
        }
    }

    /// A literal as the multiset of its terms.
    fn terms(l: XLit) -> ([OTerm; 4], usize) {
        match l {
            XLit::Atom(..) | XLit::Role(..) => {
                ([OTerm::P(l), OTerm::True, OTerm::True, OTerm::True], 2)
            }
            XLit::Eq(a, b) => ([OTerm::A(a), OTerm::A(b), OTerm::True, OTerm::True], 2),
            XLit::Ne(a, b) => ([OTerm::A(a), OTerm::A(a), OTerm::A(b), OTerm::A(b)], 4),
        }
    }

    /// Whether a literal is ground and function-free: `B(u)`, `S(u, v)`, `u ≈ v` or
    /// `u ≉ v`.
    fn plain_ground(l: XLit) -> bool {
        let (ts, n) = terms_of(l);
        ts[..n].iter().all(|t| matches!(t, RTerm::C(_)))
    }

    /// The key of the shared order on ground atoms: a role atom above a class atom, then
    /// the predicate's id, then the constants from the left, on the canonical reading of
    /// a role atom.
    fn ground_key(&self, p: XLit) -> (u8, u32, u32, u32) {
        match p.canon(self.inv) {
            XLit::Atom(b, RTerm::C(u)) => (0, b, u, 0),
            XLit::Role(s, RTerm::C(a), RTerm::C(b)) => (1, s, a, b),
            _ => unreachable!("a ground atom"),
        }
    }

    /// `a ≻ b` on literals: the multiset extension of the order on terms, and with
    /// `ground_total` the shared order on two ground literals.
    pub fn gt(&self, a: XLit, b: XLit) -> bool {
        if a == b {
            return false;
        }
        if self.ground_total && Rooted::plain_ground(a) && Rooted::plain_ground(b) {
            let atom = |l: XLit| matches!(l, XLit::Atom(..) | XLit::Role(..));
            match (atom(a), atom(b)) {
                (true, true) => return self.ground_key(a) > self.ground_key(b),
                (true, false) => return true,
                (false, true) => return false,
                (false, false) => {}
            }
        }
        let (mut ra, mut ra_len) = Rooted::terms(a);
        let (mb, nb) = Rooted::terms(b);
        // the multiset differences, kept on the stack so `maximal_rooted` never allocates
        // inside comparisons while its views buffer is live
        let mut rb = [OTerm::True; 4];
        let mut rb_len = 0usize;
        for &t in &mb[..nb] {
            if let Some(k) = ra[..ra_len].iter().position(|&s| s == t) {
                ra_len -= 1;
                ra[k] = ra[ra_len];
            } else {
                rb[rb_len] = t;
                rb_len += 1;
            }
        }
        ra_len > 0
            && rb[..rb_len]
                .iter()
                .all(|&t| ra[..ra_len].iter().any(|&s| self.t_gt(s, t)))
    }

    /// Whether a literal is an atom over y alone, which only Pred reads: no rule selects
    /// it, and Eq never rewrites its y. An equality or inequality of x and y can be
    /// selected, since Eq rewrites its x.
    pub fn over_y(l: XLit) -> bool {
        matches!(l, XLit::Atom(..) | XLit::Role(..)) && Rooted::group(l) == Group::Y
    }
}

/// [`subset`] of two sorted slices, telling `read` of each literal of `b`
/// that it steps over, `None` once read answers false.
#[inline]
pub(super) fn subset_with<F: FnMut(usize) -> bool + ?Sized>(
    a: &[Lit],
    b: &[Lit],
    read: &mut F,
) -> Option<bool> {
    if a.len() > b.len() {
        return Some(false);
    }
    let mut j = 0;
    for &x in a {
        loop {
            if !read(1) {
                return None;
            }
            if j < b.len() && b[j] < x {
                j += 1;
                continue;
            }
            break;
        }
        if j == b.len() || b[j] != x {
            return Some(false);
        }
        j += 1;
    }
    Some(true)
}

/// Whether sorted `a` is a subset of sorted `b`.
/// Whether the sorted literals `a` without repeats are among the sorted literals `b`
/// without repeats.
pub(super) fn subset(a: &[Lit], b: &[Lit]) -> bool {
    if a.len() > b.len() {
        return false;
    }
    let mut j = 0;
    for &x in a {
        while j < b.len() && b[j] < x {
            j += 1;
        }
        if j == b.len() || b[j] != x {
            return false;
        }
        j += 1;
    }
    true
}

/// One segment of a [`Segs`], made on first use.
type Segment<T> = OnceLock<Box<[OnceLock<T>]>>;

/// An append-only list whose entries never move, in segments that double, so that a worker
/// reads an entry without a lock.
struct Segs<T> {
    segs: Box<[Segment<T>]>,
}

/// The entries of the first segment of [`Segs`]; each further one doubles.
const SEG0: usize = 1024;

impl<T> Default for Segs<T> {
    fn default() -> Segs<T> {
        Segs {
            segs: (0..32).map(|_| OnceLock::new()).collect(),
        }
    }
}

impl<T> Segs<T> {
    /// Bytes of the segment that `id` would allocate if not yet initialized.
    fn next_seg_bytes(&self, id: u32) -> usize {
        let off = id as usize;
        let k = (off / SEG0 + 1).ilog2() as usize;
        if self.segs[k].get().is_some() {
            0
        } else {
            (SEG0 << k) * std::mem::size_of::<OnceLock<T>>()
        }
    }

    /// The slot of entry `id`, initializing its segment on first use and returning the
    /// newly allocated segment bytes (`0` when already initialized).
    fn slot_with_delta(&self, id: u32) -> (&OnceLock<T>, usize) {
        let off = id as usize;
        let k = (off / SEG0 + 1).ilog2() as usize;
        let at = off - SEG0 * ((1 << k) - 1);
        let mut grown = 0usize;
        let seg = self.segs[k].get_or_init(|| {
            grown = (SEG0 << k) * std::mem::size_of::<OnceLock<T>>();
            (0..SEG0 << k).map(|_| OnceLock::new()).collect()
        });
        (&seg[at], grown)
    }

    /// The slot of entry `id`, whose segment is made on first use and its bytes added to
    /// `bytes`.
    fn slot(&self, id: u32, bytes: &AtomicUsize) -> &OnceLock<T> {
        let (slot, grown) = self.slot_with_delta(id);
        if grown > 0 {
            bytes.fetch_add(grown, Ordering::Relaxed);
        }
        slot
    }
}

/// The clauses that contexts send one another, by id: an append-only table whose entries
/// never move, so that a worker reads one without a lock.
#[derive(Default)]
pub struct Table {
    next: AtomicU32,
    segs: Segs<Clause>,
    bytes: AtomicUsize,
    unadmitted_bytes: AtomicUsize,
    append_lock: parking_lot::Mutex<()>,
}

impl Table {
    fn slot(&self, id: u32) -> &OnceLock<Clause> {
        self.segs.slot(id, &self.bytes)
    }

    #[cfg(test)]
    pub fn push(&self, c: Clause) -> u32 {
        let _w = self.append_lock.lock();
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (slot, grown) = self.segs.slot_with_delta(id);
        let used = c.bytes() + grown;
        self.bytes.fetch_add(used, Ordering::Relaxed);
        self.unadmitted_bytes.fetch_add(used, Ordering::Relaxed);
        let _ = slot.set(c);
        id
    }

    pub fn get(&self, id: u32) -> &Clause {
        self.slot(id).get().expect("a clause the table holds")
    }

    pub fn bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    pub(super) fn unadmitted_bytes(&self) -> usize {
        self.unadmitted_bytes.load(Ordering::Relaxed)
    }

    pub(super) fn sync_unadmitted_on_begin_run(&mut self) {
        *self.unadmitted_bytes.get_mut() = *self.bytes.get_mut();
    }

    /// The clauses the table holds.
    pub fn len(&self) -> usize {
        self.next.load(Ordering::Relaxed) as usize
    }
}

/// Unwind-safe credit holder for slot-bound [`Table`] appends, refunding any unused
/// reservation strictly outside [`Table::append_lock`].
struct TableAdmittedCredit<'a, S: Sink> {
    out: &'a mut S,
    held: usize,
}

impl<S: Sink> Drop for TableAdmittedCredit<'_, S> {
    fn drop(&mut self) {
        if self.held > 0 {
            let back = std::mem::take(&mut self.held);
            self.out.credit_dl(back);
        }
    }
}

/// Appends a clause to `out.dl().table` with slot-bound pre-admission and deferred
/// construction: `make` is invoked only after credit covering `clause_bytes` and any new
/// segment for the exact next ID is confirmed under `Table::append_lock`.
///
/// Admitted bytes live in `Run::memory` (not `Table::unadmitted_bytes`) so they are never
/// double-counted, and any panic or refusal during top-up admission cleanly refunds all
/// prior grants strictly outside `Table::append_lock`.
pub(super) fn push_clause_admitted<S: Sink>(
    out: &mut S,
    clause_bytes: usize,
    make: impl FnOnce() -> Clause,
) -> Option<u32> {
    let mut credit = TableAdmittedCredit { out, held: 0 };
    let mut make = Some(make);
    loop {
        if credit.out.stopped() {
            return None;
        }
        enum Step {
            Done(u32),
            Need(usize),
            Overflow,
        }
        let step = {
            let table = &credit.out.dl().table;
            let _w = table.append_lock.lock();
            let id = table.next.load(Ordering::Relaxed);
            let seg_bytes = table.segs.next_seg_bytes(id);
            match clause_bytes.checked_add(seg_bytes) {
                None => Step::Overflow,
                Some(total) if total <= credit.held => {
                    let c = make.take().expect("clause constructor")();
                    let actual_id = table.next.fetch_add(1, Ordering::Relaxed);
                    debug_assert_eq!(actual_id, id);
                    let (slot, grown) = table.segs.slot_with_delta(actual_id);
                    debug_assert_eq!(grown, seg_bytes);
                    let actual_used = c.bytes() + grown;
                    debug_assert!(
                        actual_used <= credit.held,
                        "clause_bytes bound {clause_bytes} < actual {}",
                        c.bytes()
                    );
                    table.bytes.fetch_add(actual_used, Ordering::Relaxed);
                    let _ = slot.set(c);
                    credit.held -= actual_used;
                    Step::Done(actual_id)
                }
                Some(total) => Step::Need(total - credit.held),
            }
        };
        match step {
            Step::Done(id) => return Some(id),
            Step::Overflow => {
                if credit.held > 0 {
                    let back = std::mem::take(&mut credit.held);
                    credit.out.credit_dl(back);
                }
                credit.out.speculative_overflow();
                return None;
            }
            Step::Need(needed) => {
                if !credit.out.admit_dl(needed) {
                    return None;
                }
                credit.held += needed;
            }
        }
    }
}

const _: () = assert!(std::mem::size_of::<Stored>() <= 64);

/// A clause of a context's store.
#[derive(Clone)]
pub struct Stored {
    pub c: Clause,
    /// [`Clause::sig`]
    sig: u64,
    /// the clause is subsumed by a later one, and takes part in no further inference
    pub dead: bool,
    /// the rules have run on it
    pub done: bool,
    /// its id in the run's table, once sent to a predecessor
    pub sent: Option<u32>,
    /// its maximal literal once processed, when it has one
    pub max: Option<Lit>,
    /// in a context other than the root of a rooted run, the processed clause has a
    /// ground literal in its body and is fully selected ([`super::dl_rules::selection_flags`]):
    /// controls non-local mixed Eq/Factor and (without [`super::dl_rules::sel_partners_x`])
    /// Hyper-over-x / Fc-Pred partner eligibility
    pub sel: bool,
    /// in a context other than the root of a rooted run, the processed clause has a
    /// ground or `f_e(u)` literal in its body and is cut from non-local ground Hyper,
    /// head-side Join, and purely ground/Fc Eq/Factor ([`super::dl_rules::ground_body_cut`]),
    /// even when [`super::dl_rules::select_ground`] is off (item 1250)
    pub gbody: bool,
    /// in a context other than the root of a rooted run, the clause is an exact empty-body
    /// unit delivered from the root in this run (`RPred`)
    pub fact: bool,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Stored>() == 64);

/// A context's clause store, with the indexes the rules read.
#[derive(Default)]
pub struct Store {
    pub clauses: Vec<Stored>,
    /// the clauses waiting for the rules, by length, shortest first
    queue: std::collections::BinaryHeap<std::cmp::Reverse<(u32, u32)>>,
    /// the processed clauses by each of their maximal literals over x
    pub by_x: FxHashMap<Ex, Vec<u32>>,
    /// the processed clauses whose maximal literal is over `f_e(x)`, by e
    pub by_f: FxHashMap<Ex, Vec<u32>>,
    /// the processed clauses whose maximal literal is over a successor term, by that
    /// literal, so that the premises of one literal are found without the others of its
    /// term
    by_succ: FxHashMap<Lit, Vec<u32>>,
    /// the bytes that processing added to the indexes beyond what [`Store::add`]
    /// answered, which [`Store::take_grown`] hands to the run's estimate
    #[cfg(test)]
    grown: usize,
    /// the processed clauses whose maximal literal is a role literal `s(x, f(x))`, by s
    pub by_rl: FxHashMap<u32, Vec<u32>>,
    /// the processed clauses whose maximal literal is an atom over a successor term that is
    /// in a disjointness set, by the term's function symbol: the certificates that two
    /// successor terms differ
    pub by_cert: FxHashMap<Ex, Vec<u32>>,
    /// the clauses with an equality or an inequality of successor terms in the head, by
    /// each term's function symbol, which a new certificate simplifies
    pub by_eq: FxHashMap<Ex, Vec<u32>>,
    /// the processed clauses whose head is over y alone, and in a rooted run those that
    /// go to the predecessors, which the rules list
    pub preds: Vec<u32>,
    /// in a rooted run, the maximal literals of each processed clause that has more than
    /// one, whose first is [`Stored::max`]
    multi: FxHashMap<u32, Box<[Lit]>>,
    /// in a rooted run, the processed clauses by each of their maximal interned literals
    pub by_xl: FxHashMap<Lit, Vec<u32>>,
    /// in a rooted run, the processed clauses by the terms of their maximal literals
    pub terms: Option<Box<ByTerm>>,
    /// in a rooted run, the processed clauses by each ground literal of their bodies, which
    /// the Join rule resolves
    pub by_ground: FxHashMap<Lit, Vec<u32>>,
    /// every clause by each literal of its head, for the clauses a new one subsumes
    by_lit: FxHashMap<Lit, Vec<u32>>,
    /// every clause by the last literal of its head, which is the most specific, for
    /// whether a new one is subsumed
    by_last: FxHashMap<Lit, Vec<u32>>,
    /// the clauses with an empty head
    empty: Vec<u32>,
    /// exact heap bytes owned by this store across `clauses`, `c.lits`, `queue`, `preds`,
    /// `empty`, all index maps and their inner vectors, `multi`, and `terms`
    owned_bytes: usize,
    /// whether a stop of the run cut off an inference of the rules, whose conclusions the
    /// store may lack
    pub interrupted: bool,
}

impl Clone for Store {
    fn clone(&self) -> Store {
        let mut copy = Store {
            clauses: self.clauses.clone(),
            queue: self.queue.clone(),
            by_x: self.by_x.clone(),
            by_f: self.by_f.clone(),
            by_succ: self.by_succ.clone(),
            #[cfg(test)]
            grown: self.grown,
            by_rl: self.by_rl.clone(),
            by_cert: self.by_cert.clone(),
            by_eq: self.by_eq.clone(),
            preds: self.preds.clone(),
            multi: self.multi.clone(),
            by_xl: self.by_xl.clone(),
            terms: self.terms.clone(),
            by_ground: self.by_ground.clone(),
            by_lit: self.by_lit.clone(),
            by_last: self.by_last.clone(),
            empty: self.empty.clone(),
            owned_bytes: 0,
            interrupted: self.interrupted,
        };
        copy.owned_bytes = copy.compute_bytes();
        copy
    }
}

impl Store {
    pub fn bytes(&self) -> usize {
        self.owned_bytes
    }

    /// Full independent scan of every heap allocation owned by this store (including
    /// scanning `ByTerm`'s inner maps independently of its cached counter).
    fn compute_bytes(&self) -> usize {
        self.clauses.capacity() * std::mem::size_of::<Stored>()
            + self
                .clauses
                .iter()
                .map(|s| s.c.lits.len() * std::mem::size_of::<Lit>())
                .sum::<usize>()
            + self.queue.capacity() * std::mem::size_of::<std::cmp::Reverse<(u32, u32)>>()
            + self.preds.capacity() * std::mem::size_of::<u32>()
            + self.empty.capacity() * std::mem::size_of::<u32>()
            + map_vec_bytes(&self.by_x)
            + map_vec_bytes(&self.by_f)
            + map_vec_bytes(&self.by_succ)
            + map_vec_bytes(&self.by_rl)
            + map_vec_bytes(&self.by_cert)
            + map_vec_bytes(&self.by_eq)
            + map_vec_bytes(&self.by_lit)
            + map_vec_bytes(&self.by_last)
            + hash_table_bytes(
                self.multi.capacity(),
                std::mem::size_of::<(u32, Box<[Lit]>)>(),
            )
            + self
                .multi
                .values()
                .map(|m| m.len() * std::mem::size_of::<Lit>())
                .sum::<usize>()
            + map_vec_bytes(&self.by_xl)
            + self.terms.as_deref().map_or(0, ByTerm::scan_bytes)
            + map_vec_bytes(&self.by_ground)
    }

    /// Full independent scan of every heap allocation owned by this store, for invariant
    /// assertions and tests.
    #[cfg(test)]
    pub(super) fn scan_bytes(&self) -> usize {
        self.compute_bytes()
    }

    pub fn is_idle(&self) -> bool {
        self.queue.is_empty() && !self.interrupted
    }

    /// Number of entries currently resident in the clause processing heap (including
    /// any dead entries not yet popped by [`Store::peek_live`]).
    #[inline]
    pub(super) fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// Whether a clause of the store that is not dead subsumes `(body, head)`, whose
    /// signature is `sig`: a clause that does has its last head literal in `head`.
    fn subsumed_parts(&self, body: &[Lit], head: &[Lit], sig: u64) -> bool {
        let check = |ids: &[u32]| {
            ids.iter().any(|&i| {
                let s = &self.clauses[i as usize];
                s.sig & !sig == 0 && !s.dead && subset(s.c.body(), body) && subset(s.c.head(), head)
            })
        };
        if check(&self.empty) {
            return true;
        }
        head.iter()
            .any(|l| self.by_last.get(l).is_some_and(|ids| check(ids)))
    }

    /// Whether a clause of the store that is not dead subsumes `c`, whose signature is
    /// `sig`: a clause that does has its last head literal in `c`'s head.
    pub(super) fn subsumed(&self, c: &Clause, sig: u64) -> bool {
        self.subsumed_parts(c.body(), c.head(), sig)
    }

    /// [`Store::subsumed_parts`] of a sorted body and head without repeats, telling `read`
    /// of each candidate it reads and of each literal that a comparison steps over, for
    /// item 1280's meter, and `None` once read answers false.
    pub(super) fn subsumed_with(
        &self,
        body: &[Lit],
        head: &[Lit],
        sig: u64,
        read: &mut dyn FnMut(usize) -> bool,
    ) -> Option<bool> {
        let check = |ids: &[u32], read: &mut dyn FnMut(usize) -> bool| -> Option<bool> {
            for &i in ids {
                if !read(1) {
                    return None;
                }
                let s = &self.clauses[i as usize];
                if s.sig & !sig == 0
                    && !s.dead
                    && subset_with(s.c.body(), body, read)?
                    && subset_with(s.c.head(), head, read)?
                {
                    return Some(true);
                }
            }
            Some(false)
        };
        if check(&self.empty, read)? {
            return Some(true);
        }
        for l in head {
            if !read(1) {
                return None;
            }
            if let Some(ids) = self.by_last.get(l)
                && check(ids, read)?
            {
                return Some(true);
            }
        }
        Some(false)
    }

    /// Upper bound on peak index/container allocation (excluding the clause's own literal
    /// slice) needed by [`Store::add_unchecked`] for a clause with head `head`.
    fn add_index_growth_bound(&self, head: &[Lit]) -> Option<usize> {
        let mut bound = vec_grow_bound(&self.clauses)?;
        if self.queue.len() >= self.queue.capacity() {
            let q = next_vec_cap(
                self.queue.capacity(),
                std::mem::size_of::<std::cmp::Reverse<(u32, u32)>>(),
            )?
            .checked_mul(std::mem::size_of::<std::cmp::Reverse<(u32, u32)>>())?;
            bound = bound.checked_add(q)?;
        }
        match head.last() {
            None => bound = bound.checked_add(vec_grow_bound(&self.empty)?)?,
            Some(l) => bound = bound.checked_add(map_vec_push_bound(&self.by_last, l)?)?,
        }
        let new_vec =
            next_vec_cap(0, std::mem::size_of::<u32>())?.checked_mul(std::mem::size_of::<u32>())?;
        let mut new_lit_keys = 0usize;
        let mut new_eq_keys = 0usize;
        for &l in head {
            if let Some(v) = self.by_lit.get(&l) {
                bound = bound.checked_add(vec_grow_bound(v)?)?;
            } else {
                new_lit_keys += 1;
                bound = bound.checked_add(new_vec)?;
            }
            if matches!(l.kind(), EQ | NE)
                && let Term::Fun(g) = l.term()
            {
                for f in [l.fun(), g] {
                    if let Some(v) = self.by_eq.get(&f) {
                        bound = bound.checked_add(vec_grow_bound(v)?)?;
                    } else {
                        new_eq_keys += 1;
                        bound = bound.checked_add(new_vec)?;
                    }
                }
            }
        }
        bound = bound.checked_add(hash_table_grow_bound(
            self.by_lit.len(),
            self.by_lit.capacity(),
            new_lit_keys,
            std::mem::size_of::<(Lit, Vec<u32>)>(),
        )?)?;
        bound = bound.checked_add(hash_table_grow_bound(
            self.by_eq.len(),
            self.by_eq.capacity(),
            new_eq_keys,
            std::mem::size_of::<(Ex, Vec<u32>)>(),
        )?)?;
        Some(bound)
    }

    /// Upper bound on peak allocation (including reallocation overlap) needed by
    /// [`Store::add_unchecked`] for `c`.
    fn add_growth_bound(&self, c: &Clause) -> Option<usize> {
        let payload = c.lits.len().checked_mul(std::mem::size_of::<Lit>())?;
        payload.checked_add(self.add_index_growth_bound(c.head())?)
    }

    /// Inserts an unsubsumed clause `c` with signature `sig`, marking backward-subsumed
    /// clauses dead, and returns the exact retained byte increase.
    fn add_unchecked(&mut self, c: Clause, sig: u64, fact: bool) -> usize {
        // the clauses it subsumes have every literal of its head
        match c.head().first() {
            Some(l) => {
                let pick = c
                    .head()
                    .iter()
                    .min_by_key(|l| self.by_lit.get(l).map_or(0, |v| v.len()))
                    .unwrap_or(l);
                if let Some(weaker) = self.by_lit.get(pick) {
                    for &i in weaker {
                        let s = &mut self.clauses[i as usize];
                        if sig & !s.sig == 0 && !s.dead && c.subsumes(&s.c) {
                            s.dead = true;
                        }
                    }
                }
            }
            None => {
                for s in &mut self.clauses {
                    if sig & !s.sig == 0 && !s.dead && c.subsumes(&s.c) {
                        s.dead = true;
                    }
                }
            }
        }
        let id = self.clauses.len() as u32;
        let mut delta = c.lits.len() * std::mem::size_of::<Lit>();
        match c.head().last() {
            None => delta += vec_push_delta(&mut self.empty, id),
            Some(&l) => delta += map_vec_push(&mut self.by_last, l, id),
        }
        for &l in c.head() {
            delta += map_vec_push(&mut self.by_lit, l, id);
            if matches!(l.kind(), EQ | NE)
                && let Term::Fun(g) = l.term()
            {
                for f in [l.fun(), g] {
                    delta += map_vec_push_dedup(&mut self.by_eq, f, id);
                }
            }
        }
        let q_cap = self.queue.capacity();
        self.queue.push(std::cmp::Reverse((priority(&c), id)));
        delta +=
            (self.queue.capacity() - q_cap) * std::mem::size_of::<std::cmp::Reverse<(u32, u32)>>();
        delta += vec_push_delta(
            &mut self.clauses,
            Stored {
                c,
                sig,
                dead: false,
                done: false,
                sent: None,
                max: None,
                sel: false,
                gbody: false,
                fact,
            },
        );
        self.owned_bytes += delta;
        delta
    }

    /// Adds a clause that is not subsumed, marks dead those it subsumes, and queues it.
    /// The answer is the exact retained bytes it added, or `None` when it was subsumed.
    #[cfg(test)]
    pub fn add(&mut self, c: Clause) -> Option<usize> {
        let sig = c.sig();
        if self.subsumed(&c, sig) {
            return None;
        }
        Some(self.add_unchecked(c, sig, false))
    }

    /// Pre-admits peak growth against `out`, then adds `c` if not subsumed and commits its
    /// exact retained growth. Returns `false` and marks `interrupted` without mutating or
    /// allocating when `out` refuses the growth.
    pub fn add_admitted<S: Sink>(&mut self, c: Clause, out: &mut S) -> bool {
        let sig = c.sig();
        if self.subsumed(&c, sig) {
            return true;
        }
        let Some(bound) = self.add_growth_bound(&c) else {
            out.speculative_overflow();
            self.interrupted = true;
            return false;
        };
        if !out.admit_dl(bound) {
            self.interrupted = true;
            return false;
        }
        let delta = self.add_unchecked(c, sig, false);
        out.commit_dl(bound, delta);
        out.dl().on_queue_push();
        true
    }

    /// Pre-admits clause construction copy/shrink scratch and store index growth before
    /// building `Clause` from `(body, head)`. When `covered_payload > 0`, the caller already
    /// holds a `Sink::admit_dl` reservation of `covered_payload` covering the source buffer;
    /// returns `(ok, consumed_covered)` where `consumed_covered` is `true` iff `commit_dl`
    /// settled `covered_payload` (so the caller should disarm its guard).
    pub fn add_parts_admitted_covered<S: Sink>(
        &mut self,
        body: Vec<Lit>,
        head: Vec<Lit>,
        covered_payload: usize,
        out: &mut S,
    ) -> (bool, bool) {
        self.add_parts_admitted_covered_with_fact(body, head, covered_payload, false, out)
    }

    /// [`Store::add_parts_admitted_covered`], conferring exact root-unit provenance
    /// (`Stored::fact = true`) when `is_fact` is set and the normalized clause is an exact
    /// empty-body unit. When a live exact duplicate unit already exists in the store (even
    /// if still queued), marks it `fact = true` in place with zero allocation.
    fn add_parts_admitted_covered_with_fact<S: Sink>(
        &mut self,
        mut body: Vec<Lit>,
        mut head: Vec<Lit>,
        covered_payload: usize,
        is_fact: bool,
        out: &mut S,
    ) -> (bool, bool) {
        body.sort_unstable();
        body.dedup();
        head.sort_unstable();
        head.dedup();
        let unit_fact = is_fact && body.is_empty() && head.len() == 1;
        let sig = body
            .iter()
            .chain(head.iter())
            .fold(0u64, |s, l| s | l.sig_bit());
        if self.subsumed_parts(&body, &head, sig) {
            if unit_fact && let Some(ids) = self.by_last.get(&head[0]) {
                for &i in ids {
                    let s = &mut self.clauses[i as usize];
                    if !s.dead && s.c.body().is_empty() && s.c.head() == head.as_slice() {
                        s.fact = true;
                    }
                }
            }
            return (true, false);
        }
        let Some((total_need, extra_need)) = (|| {
            let payload = body
                .len()
                .checked_add(head.len())?
                .checked_mul(std::mem::size_of::<Lit>())?;
            let build_alloc = Clause::build_alloc_bound(&body, &head)?;
            let index_bound = self.add_index_growth_bound(&head)?;
            let during_build = covered_payload.checked_add(build_alloc)?;
            let during_index = payload.checked_add(index_bound)?;
            let total = during_build.max(during_index);
            let extra = total.saturating_sub(covered_payload);
            Some((total, extra))
        })() else {
            out.speculative_overflow();
            self.interrupted = true;
            return (false, false);
        };
        if extra_need > 0 && !out.admit_dl(extra_need) {
            self.interrupted = true;
            return (false, false);
        }
        if extra_need == 0 && out.stopped() {
            self.interrupted = true;
            return (false, false);
        }
        let c = Clause::from_sorted(body, head);
        let delta = self.add_unchecked(c, sig, unit_fact);
        let committed_bound = covered_payload + extra_need;
        debug_assert!(committed_bound >= total_need);
        out.commit_dl(committed_bound, delta);
        out.dl().on_queue_push();
        (true, true)
    }

    /// Pre-admits clause construction and store index growth before building `Clause` from
    /// `(body, head)`. Returns `false` and marks `interrupted` without copying or mutating
    /// `self` when `out` refuses the growth.
    pub fn add_parts_admitted<S: Sink>(
        &mut self,
        body: Vec<Lit>,
        head: Vec<Lit>,
        out: &mut S,
    ) -> bool {
        self.add_parts_admitted_covered_with_fact(body, head, 0, false, out)
            .0
    }

    /// [`Store::add_parts_admitted`], with exact root-unit provenance when `is_fact` is set.
    pub fn add_parts_admitted_with_fact<S: Sink>(
        &mut self,
        body: Vec<Lit>,
        head: Vec<Lit>,
        is_fact: bool,
        out: &mut S,
    ) -> bool {
        self.add_parts_admitted_covered_with_fact(body, head, 0, is_fact, out)
            .0
    }

    /// Upper bound on peak allocation needed to index processed clause `id` with maximal
    /// literals `max`.
    fn next_growth_bound(
        &self,
        ix: &Index,
        rooted: Option<&Rooted>,
        id: u32,
        max: &[Lit],
    ) -> Option<usize> {
        let mut bound = 0usize;
        if max.is_empty() && rooted.is_none() {
            bound = bound.checked_add(vec_grow_bound(&self.preds)?)?;
        }
        if max.len() > 1 {
            bound = bound.checked_add(max.len().checked_mul(std::mem::size_of::<Lit>())?)?;
            bound = bound.checked_add(hash_table_grow_bound(
                self.multi.len(),
                self.multi.capacity(),
                1,
                std::mem::size_of::<(u32, Box<[Lit]>)>(),
            )?)?;
        }
        let new_vec =
            next_vec_cap(0, std::mem::size_of::<u32>())?.checked_mul(std::mem::size_of::<u32>())?;
        let (mut nx, mut nxl, mut nf, mut nsucc, mut nrl) = (0usize, 0, 0, 0, 0);
        for &l in max {
            match l.kind() {
                X => {
                    if let Some(v) = self.by_x.get(&l.atom()) {
                        bound = bound.checked_add(vec_grow_bound(v)?)?;
                    } else {
                        nx += 1;
                        bound = bound.checked_add(new_vec)?;
                    }
                }
                XL => {
                    if let Some(v) = self.by_xl.get(&l) {
                        bound = bound.checked_add(vec_grow_bound(v)?)?;
                    } else {
                        nxl += 1;
                        bound = bound.checked_add(new_vec)?;
                    }
                }
                RL => {
                    if let Some(v) = self.by_f.get(&l.fun()) {
                        bound = bound.checked_add(vec_grow_bound(v)?)?;
                    } else {
                        nf += 1;
                        bound = bound.checked_add(new_vec)?;
                    }
                    if let Some(v) = self.by_succ.get(&l) {
                        bound = bound.checked_add(vec_grow_bound(v)?)?;
                    } else {
                        nsucc += 1;
                        bound = bound.checked_add(new_vec)?;
                    }
                    if let Some(v) = self.by_rl.get(&l.atom()) {
                        bound = bound.checked_add(vec_grow_bound(v)?)?;
                    } else {
                        nrl += 1;
                        bound = bound.checked_add(new_vec)?;
                    }
                }
                _ => {
                    if let Some(v) = self.by_f.get(&l.fun()) {
                        bound = bound.checked_add(vec_grow_bound(v)?)?;
                    } else {
                        nf += 1;
                        bound = bound.checked_add(new_vec)?;
                    }
                    if let Some(v) = self.by_succ.get(&l) {
                        bound = bound.checked_add(vec_grow_bound(v)?)?;
                    } else {
                        nsucc += 1;
                        bound = bound.checked_add(new_vec)?;
                    }
                }
            }
        }
        bound = bound.checked_add(hash_table_grow_bound(
            self.by_x.len(),
            self.by_x.capacity(),
            nx,
            std::mem::size_of::<(Ex, Vec<u32>)>(),
        )?)?;
        bound = bound.checked_add(hash_table_grow_bound(
            self.by_xl.len(),
            self.by_xl.capacity(),
            nxl,
            std::mem::size_of::<(Lit, Vec<u32>)>(),
        )?)?;
        bound = bound.checked_add(hash_table_grow_bound(
            self.by_f.len(),
            self.by_f.capacity(),
            nf,
            std::mem::size_of::<(Ex, Vec<u32>)>(),
        )?)?;
        bound = bound.checked_add(hash_table_grow_bound(
            self.by_succ.len(),
            self.by_succ.capacity(),
            nsucc,
            std::mem::size_of::<(Lit, Vec<u32>)>(),
        )?)?;
        bound = bound.checked_add(hash_table_grow_bound(
            self.by_rl.len(),
            self.by_rl.capacity(),
            nrl,
            std::mem::size_of::<(u32, Vec<u32>)>(),
        )?)?;
        if let Some(o) = rooted {
            let terms_bound = match self.terms.as_deref() {
                Some(t) => t.index_all_bound(o, max)?,
                None => std::mem::size_of::<ByTerm>()
                    .checked_add(ByTerm::fresh_index_all_bound(o, max)?)?,
            };
            bound = bound.checked_add(terms_bound)?;
            let mut ng = 0usize;
            for &l in self.clauses[id as usize].c.body() {
                if l.kind() == XL && o.view(l).is_ground() {
                    if let Some(v) = self.by_ground.get(&l) {
                        bound = bound.checked_add(vec_grow_bound(v)?)?;
                    } else {
                        ng += 1;
                        bound = bound.checked_add(new_vec)?;
                    }
                }
            }
            bound = bound.checked_add(hash_table_grow_bound(
                self.by_ground.len(),
                self.by_ground.capacity(),
                ng,
                std::mem::size_of::<(Lit, Vec<u32>)>(),
            )?)?;
        }
        if let Some(&l) = max.iter().find(|l| l.kind() == F)
            && !ix.disjoint_of(ix.head(l.atom())).is_empty()
        {
            bound = bound.checked_add(map_vec_push_bound(&self.by_cert, &l.fun())?)?;
        }
        Some(bound)
    }

    /// Marks clause `id` processed and updates every processing index, returning exact
    /// retained byte growth.
    fn index_processed(
        &mut self,
        ix: &Index,
        rooted: Option<&Rooted>,
        id: u32,
        max: &[Lit],
    ) -> usize {
        let mut delta = 0usize;
        {
            let s = &mut self.clauses[id as usize];
            s.done = true;
            s.max = max.first().copied();
        }
        // a rooted run lists the clauses over the predecessor by its own test
        if max.is_empty() && rooted.is_none() {
            delta += vec_push_delta(&mut self.preds, id);
        }
        if max.len() > 1 {
            let entry = std::mem::size_of::<(u32, Box<[Lit]>)>();
            let before = hash_table_bytes(self.multi.capacity(), entry);
            self.multi.insert(id, max.into());
            delta += std::mem::size_of_val(max)
                + hash_table_bytes(self.multi.capacity(), entry).saturating_sub(before);
        }
        for &l in max {
            match l.kind() {
                X => delta += map_vec_push(&mut self.by_x, l.atom(), id),
                XL => delta += map_vec_push(&mut self.by_xl, l, id),
                RL => {
                    delta += map_vec_push(&mut self.by_f, l.fun(), id);
                    delta += map_vec_push(&mut self.by_succ, l, id);
                    delta += map_vec_push(&mut self.by_rl, l.atom(), id);
                }
                _ => {
                    delta += map_vec_push(&mut self.by_f, l.fun(), id);
                    delta += map_vec_push(&mut self.by_succ, l, id);
                }
            }
        }
        if let Some(o) = rooted {
            let fresh = self.terms.is_none();
            let t = self.terms.get_or_insert_with(Box::default);
            if fresh {
                delta += std::mem::size_of::<ByTerm>();
            }
            for &l in max {
                delta += t.index(o.view(l), id);
            }
            for &l in self.clauses[id as usize].c.body() {
                if l.kind() == XL && o.view(l).is_ground() {
                    delta += map_vec_push(&mut self.by_ground, l, id);
                }
            }
        }
        if let Some(l) = self.max_of(id, |l| l.kind() == F)
            && !ix.disjoint_of(ix.head(l.atom())).is_empty()
        {
            delta += map_vec_push(&mut self.by_cert, l.fun(), id);
        }
        self.owned_bytes += delta;
        delta
    }

    /// The next clause to process, which is marked processed and indexed by its maximal
    /// literals, under the order of a rooted run when `rooted` is given.
    #[cfg(test)]
    pub fn next(&mut self, ix: &Index, rooted: Option<&Rooted>, max: &mut Vec<Lit>) -> Option<u32> {
        while let Some(std::cmp::Reverse((_, id))) = self.queue.pop() {
            let s = &self.clauses[id as usize];
            if s.dead {
                continue;
            }
            match rooted {
                None => s.c.maximal(ix, max),
                Some(o) => s.c.maximal_rooted(o, max),
            }
            let delta = self.index_processed(ix, rooted, id, max);
            self.grown += delta;
            return Some(id);
        }
        None
    }

    /// Peeks the next live queued clause id without popping it. Discards already-subsumed
    /// dead queue entries in place; dead-entry cleanup allocates nothing, updates `dl`'s
    /// resident queue occupancy counter (without crediting retained heap capacity), and leaves
    /// `owned_bytes`, every live clause, and every index unchanged, including if a following
    /// admission refuses.
    pub fn peek_live(&mut self, dl: &super::dl_rules::DlRun) -> Option<u32> {
        let mut dead_popped = 0usize;
        let live = loop {
            let Some(&std::cmp::Reverse((_, id))) = self.queue.peek() else {
                break None;
            };
            if self.clauses[id as usize].dead {
                self.queue.pop();
                dead_popped += 1;
                continue;
            }
            break Some(id);
        };
        if dead_popped > 0 {
            dl.on_queue_pop(dead_popped);
        }
        live
    }

    /// Upper bound on additional temporary scratch `(views_bytes, max_peak_add)` needed above
    /// `max`'s current capacity to compute the maximal literals of clause `id` into `max`
    /// (in a rooted run `maximal_rooted` allocates a `Vec<XLit>` of views and can grow `max`
    /// up to `head.len()`, whose final reallocation holds consecutive capacities simultaneously
    /// under the old/new overlap model).
    pub fn next_scratch_bound(
        &self,
        id: u32,
        rooted: bool,
        max: &[Lit],
        max_cap: usize,
    ) -> Option<(usize, usize)> {
        let _ = max;
        let head_len = self.clauses[id as usize].c.head().len();
        let views = if rooted {
            head_len.checked_mul(std::mem::size_of::<XLit>())?
        } else {
            0
        };
        let target_len = if rooted {
            head_len
        } else {
            usize::from(head_len > 0)
        };
        let max_peak_add = if target_len <= max_cap {
            0
        } else {
            let mut prev = max_cap;
            let mut cap = max_cap;
            while cap < target_len {
                prev = cap;
                cap = next_vec_cap(cap, std::mem::size_of::<Lit>())?;
            }
            // Peak `max` storage during the final growth is `(prev + cap)` entries; callers
            // already hold `max_cap` entries, so the additional peak above `max_cap` is
            // `(prev - max_cap) + cap`.
            prev.checked_sub(max_cap)?
                .checked_add(cap)?
                .checked_mul(std::mem::size_of::<Lit>())?
        };
        Some((views, max_peak_add))
    }

    /// Computes the maximal literals of live queued clause `id` into `max` and returns the
    /// index-growth bound (`None` on overflow) without popping or mutating processing indexes.
    pub fn next_prepare_id(
        &self,
        ix: &Index,
        rooted: Option<&Rooted>,
        id: u32,
        max: &mut Vec<Lit>,
    ) -> Option<usize> {
        match rooted {
            None => self.clauses[id as usize].c.maximal(ix, max),
            Some(o) => self.clauses[id as usize].c.maximal_rooted(o, max),
        }
        self.next_growth_bound(ix, rooted, id, max)
    }

    /// Pops and indexes the clause prepared by [`Store::next_prepare_id`], returning the
    /// exact retained byte delta.
    pub fn next_commit(
        &mut self,
        ix: &Index,
        rooted: Option<&Rooted>,
        id: u32,
        max: &[Lit],
        dl: &super::dl_rules::DlRun,
    ) -> usize {
        let popped = self.queue.pop();
        debug_assert_eq!(popped.map(|r| r.0.1), Some(id));
        if popped.is_some() {
            dl.on_queue_pop(1);
        }
        self.index_processed(ix, rooted, id, max)
    }

    /// Whether the processing queue still holds at least one entry.
    #[cfg(test)]
    pub fn has_queued(&self) -> bool {
        !self.queue.is_empty()
    }

    /// [`Store::next`], pre-admitting maximal-literal scratch and peak index growth against
    /// `out` before computing maxima or popping/mutating any processing index. Leaves the
    /// live clause queued and marks `interrupted` on refusal (already-subsumed dead queue
    /// entries ahead of it may be discarded without allocating).
    #[cfg(test)]
    pub fn next_admitted<S: Sink>(
        &mut self,
        ix: &Index,
        rooted: Option<&Rooted>,
        max: &mut Vec<Lit>,
        out: &mut S,
    ) -> Option<u32> {
        let id = self.peek_live(out.dl())?;
        let max_cap_before = max.capacity();
        let Some((views_bytes, max_peak_add)) =
            self.next_scratch_bound(id, rooted.is_some(), max, max_cap_before)
        else {
            out.speculative_overflow();
            self.interrupted = true;
            return None;
        };
        let Some(scratch) = views_bytes.checked_add(max_peak_add) else {
            out.speculative_overflow();
            self.interrupted = true;
            return None;
        };
        let mut scratch_guard = super::saturate::Guard::new(out);
        if scratch > 0 && !scratch_guard.admit_add(scratch) {
            self.interrupted = true;
            return None;
        }
        let Some(bound) = self.next_prepare_id(ix, rooted, id, max) else {
            scratch_guard.speculative_overflow();
            self.interrupted = true;
            return None;
        };
        // `views` dropped inside `maximal_rooted`; keep only net `max` growth while admitting.
        let max_growth = max.capacity().saturating_sub(max_cap_before) * std::mem::size_of::<Lit>();
        scratch_guard.release_to(max_growth);
        if !scratch_guard.admit_dl(bound) {
            self.interrupted = true;
            return None;
        }
        let delta = self.next_commit(ix, rooted, id, max, scratch_guard.dl());
        scratch_guard.commit_dl(bound, delta);
        Some(id)
    }

    /// Appends `id` to `self.preds`, pre-admitting vector growth against `out`.
    pub fn push_pred_admitted<S: Sink>(&mut self, id: u32, out: &mut S) -> bool {
        let Some(bound) = vec_grow_bound(&self.preds) else {
            out.speculative_overflow();
            self.interrupted = true;
            return false;
        };
        if !out.admit_dl(bound) {
            self.interrupted = true;
            return false;
        }
        let delta = vec_push_delta(&mut self.preds, id);
        self.owned_bytes += delta;
        out.commit_dl(bound, delta);
        true
    }

    /// Marks every clause with `l` in its head dead in place, without allocating.
    pub fn kill_head(&mut self, l: Lit) {
        if let Some(ids) = self.by_lit.get(&l) {
            for &i in ids {
                self.clauses[i as usize].dead = true;
            }
        }
    }

    /// Whether any live clause `D` in this store contains one of `blockers` in its head
    /// and subsumes the weakened witness conclusion
    /// `body → (head \ {resolved}) ∨ witness ∨ ⋁ blockers` (item 896, R1a).
    ///
    /// Every inspected setup literal, blocker-key probe, and `by_lit` entry (including dead
    /// and signature-rejected clauses) increments `*reads`, and `keep_going` is invoked
    /// every `chunk` reads so the caller can charge scan work with remainder carry and
    /// respect cancellation or work bounds.
    #[allow(clippy::too_many_arguments)]
    pub fn has_clause_blocker(
        &self,
        body: &[Lit],
        head: &[Lit],
        resolved: Lit,
        witness: Lit,
        blockers: &[Ex],
        chunk: usize,
        reads: &mut usize,
        mut keep_going: impl FnMut(&mut usize) -> bool,
    ) -> bool {
        if self.by_lit.is_empty() || blockers.is_empty() {
            return false;
        }
        let chunk = chunk.max(1);
        let mut allowed_sig = witness.sig_bit();
        for &l in body {
            *reads += 1;
            if *reads >= chunk && !keep_going(reads) {
                return false;
            }
            allowed_sig |= l.sig_bit();
        }
        for &l in head {
            *reads += 1;
            if *reads >= chunk && !keep_going(reads) {
                return false;
            }
            if l != resolved {
                allowed_sig |= l.sig_bit();
            }
        }
        for &g in blockers {
            *reads += 1;
            if *reads >= chunk && !keep_going(reads) {
                return false;
            }
            allowed_sig |= Lit::x(g).sig_bit();
        }
        for &g in blockers {
            *reads += 1;
            if *reads >= chunk && !keep_going(reads) {
                return false;
            }
            let Some(ids) = self.by_lit.get(&Lit::x(g)) else {
                continue;
            };
            for &i in ids {
                *reads += 1;
                if *reads >= chunk && !keep_going(reads) {
                    return false;
                }
                let s = &self.clauses[i as usize];
                if s.dead || (s.sig & !allowed_sig) != 0 {
                    continue;
                }
                if !subset(s.c.body(), body) {
                    continue;
                }
                if s.c.head().iter().all(|&m| {
                    m == witness
                        || (m != resolved && head.binary_search(&m).is_ok())
                        || (m.kind() == X && blockers.binary_search(&m.atom()).is_ok())
                }) {
                    return true;
                }
            }
        }
        false
    }

    /// Indexes a processed clause by its maximal literal over a successor term, and adds
    /// what the table and the list grew by to the bytes the run's estimate has not seen,
    /// since a store can hold as many keys as clauses.
    #[cfg(test)]
    fn index_succ(&mut self, l: Lit, id: u32) {
        let delta = map_vec_push(&mut self.by_succ, l, id);
        self.owned_bytes += delta;
        self.grown += delta;
    }

    /// The maximal literals of a processed clause.
    pub fn maxima(&self, i: u32) -> &[Lit] {
        match self.multi.get(&i) {
            Some(m) => m,
            None => self.clauses[i as usize].max.as_slice(),
        }
    }

    /// The first maximal literal of a processed clause that `p` takes. Without a rooted
    /// order a clause has one maximal literal, which [`Stored::max`] holds.
    #[inline]
    /// Every maximal literal of the processed clause `i`.
    pub fn maxes(&self, i: u32) -> impl Iterator<Item = Lit> + '_ {
        let multi = self.multi.get(&i);
        let first = self.clauses[i as usize].max.filter(|_| multi.is_none());
        first
            .into_iter()
            .chain(multi.into_iter().flat_map(|m| m.iter().copied()))
    }

    pub fn max_of(&self, i: u32, p: impl Fn(Lit) -> bool) -> Option<Lit> {
        match self.clauses[i as usize].max {
            Some(l) if p(l) => Some(l),
            _ if self.multi.is_empty() => None,
            _ => self.multi.get(&i)?.iter().copied().find(|&l| p(l)),
        }
    }

    /// The bytes processing added to the indexes since the last call, for the run's
    /// estimate.
    #[cfg(test)]
    pub fn take_grown(&mut self) -> usize {
        std::mem::take(&mut self.grown)
    }

    /// The processed clauses that are not dead whose maximal literals include `X(a)`.
    pub fn with_x(&self, a: Ex) -> impl Iterator<Item = u32> + '_ {
        self.by_x
            .get(&a)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| !self.clauses[i as usize].dead)
    }

    /// The processed clauses that are not dead whose maximal literal is over `f_e(x)`.
    pub fn with_succ_fun(&self, e: Ex) -> impl Iterator<Item = u32> + '_ {
        self.by_f
            .get(&e)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| !self.clauses[i as usize].dead)
    }

    /// The processed clauses that are not dead whose maximal literal is `l`, over a
    /// successor term.
    pub fn with_succ(&self, l: Lit) -> impl Iterator<Item = u32> + '_ {
        self.by_succ
            .get(&l)
            .into_iter()
            .flatten()
            .copied()
            .filter(move |&i| {
                let s = &self.clauses[i as usize];
                !s.dead && (s.max == Some(l) || self.max_of(i, |m| m == l).is_some())
            })
    }

    /// The processed clauses that are not dead whose maximal literal is over `f(x)`, with
    /// that literal.
    pub fn over(&self, f: Ex) -> impl Iterator<Item = (u32, Lit)> + '_ {
        self.by_f
            .get(&f)
            .into_iter()
            .flatten()
            .filter_map(move |&i| {
                let s = &self.clauses[i as usize];
                (!s.dead).then_some(())?;
                self.max_of(i, |l| matches!(l.kind(), F | RL | EQ | NE) && l.fun() == f)
                    .map(|l| (i, l))
            })
    }
}

/// The processed clauses of a rooted run's store by the terms of their maximal literals,
/// which the Eq rule reads: every clause by each term of a maximal literal, a constant
/// inside `f(u)` included, and the clauses with a maximal equality by each of its terms.
pub struct ByTerm {
    pub any: FxHashMap<RTerm, Vec<u32>>,
    pub eqs: FxHashMap<RTerm, Vec<u32>>,
    owned_bytes: usize,
}

impl Clone for ByTerm {
    fn clone(&self) -> ByTerm {
        let any = self.any.clone();
        let eqs = self.eqs.clone();
        let owned_bytes = std::mem::size_of::<ByTerm>() + map_vec_bytes(&any) + map_vec_bytes(&eqs);
        ByTerm {
            any,
            eqs,
            owned_bytes,
        }
    }
}

impl Default for ByTerm {
    fn default() -> ByTerm {
        ByTerm {
            any: FxHashMap::default(),
            eqs: FxHashMap::default(),
            owned_bytes: std::mem::size_of::<ByTerm>(),
        }
    }
}

impl ByTerm {
    fn scan_bytes(&self) -> usize {
        std::mem::size_of::<ByTerm>() + map_vec_bytes(&self.any) + map_vec_bytes(&self.eqs)
    }

    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.owned_bytes
    }

    fn index_all_bound(&self, o: &Rooted, max: &[Lit]) -> Option<usize> {
        let new_vec =
            next_vec_cap(0, std::mem::size_of::<u32>())?.checked_mul(std::mem::size_of::<u32>())?;
        let mut bound = 0usize;
        let (mut n_any, mut n_eqs) = (0usize, 0usize);
        for &l in max {
            let v = o.view(l);
            let (ts, n) = terms_of(v);
            for &t in &ts[..n] {
                if let Some(list) = self.any.get(&t) {
                    bound = bound.checked_add(vec_grow_bound(list)?)?;
                } else {
                    n_any += 1;
                    bound = bound.checked_add(new_vec)?;
                }
                if let RTerm::Fc(_, u) = t {
                    let c = RTerm::C(u);
                    if let Some(list) = self.any.get(&c) {
                        bound = bound.checked_add(vec_grow_bound(list)?)?;
                    } else {
                        n_any += 1;
                        bound = bound.checked_add(new_vec)?;
                    }
                }
                if matches!(v, XLit::Eq(..)) {
                    if let Some(list) = self.eqs.get(&t) {
                        bound = bound.checked_add(vec_grow_bound(list)?)?;
                    } else {
                        n_eqs += 1;
                        bound = bound.checked_add(new_vec)?;
                    }
                }
            }
        }
        let entry = std::mem::size_of::<(RTerm, Vec<u32>)>();
        bound = bound.checked_add(hash_table_grow_bound(
            self.any.len(),
            self.any.capacity(),
            n_any,
            entry,
        )?)?;
        bound = bound.checked_add(hash_table_grow_bound(
            self.eqs.len(),
            self.eqs.capacity(),
            n_eqs,
            entry,
        )?)?;
        Some(bound)
    }

    fn fresh_index_all_bound(o: &Rooted, max: &[Lit]) -> Option<usize> {
        let new_vec =
            next_vec_cap(0, std::mem::size_of::<u32>())?.checked_mul(std::mem::size_of::<u32>())?;
        let (mut n_any, mut n_eqs) = (0usize, 0usize);
        for &l in max {
            let v = o.view(l);
            let (ts, n) = terms_of(v);
            for &t in &ts[..n] {
                n_any += 1;
                if matches!(t, RTerm::Fc(..)) {
                    n_any += 1;
                }
                if matches!(v, XLit::Eq(..)) {
                    n_eqs += 1;
                }
            }
        }
        let entry = std::mem::size_of::<(RTerm, Vec<u32>)>();
        let mut bound = n_any.checked_add(n_eqs)?.checked_mul(new_vec)?;
        bound = bound.checked_add(hash_table_grow_bound(0, 0, n_any, entry)?)?;
        bound = bound.checked_add(hash_table_grow_bound(0, 0, n_eqs, entry)?)?;
        Some(bound)
    }

    fn index(&mut self, l: XLit, id: u32) -> usize {
        let mut delta = 0usize;
        let (ts, n) = terms_of(l);
        for &t in &ts[..n] {
            delta += map_vec_push_dedup(&mut self.any, t, id);
            if let RTerm::Fc(_, u) = t {
                delta += map_vec_push_dedup(&mut self.any, RTerm::C(u), id);
            }
            if matches!(l, XLit::Eq(..)) {
                delta += map_vec_push_dedup(&mut self.eqs, t, id);
            }
        }
        self.owned_bytes += delta;
        delta
    }
}

/// The terms of a literal at its top positions.
pub fn terms_of(l: XLit) -> ([RTerm; 2], usize) {
    match l {
        XLit::Atom(_, a) => ([a, a], 1),
        XLit::Role(_, a, b) | XLit::Eq(a, b) | XLit::Ne(a, b) => ([a, b], 2),
    }
}

/// The order in which the store processes its clauses, least first: the Horn clauses before
/// the others, as Sequoia does (Bate et al. 2018, Section 5.2.2), so that the clauses that
/// simplify others come early, and then the shorter.
fn priority(c: &Clause) -> u32 {
    let horn = c.head().len() <= 1;
    (!horn as u32) << 16 | c.lits.len() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsumption_reads_body_and_head() {
        let a = Clause::new(vec![Lit::x(5)], vec![Lit::x(7)]);
        let b = Clause::new(vec![Lit::x(5), Lit::x(6)], vec![Lit::x(7), Lit::y(8)]);
        assert!(a.subsumes(&b));
        assert!(!b.subsumes(&a));
    }

    #[test]
    fn the_store_drops_what_it_subsumes() {
        let mut s = Store::default();
        assert!(
            s.add(Clause::new(vec![], vec![Lit::x(3), Lit::x(4)]))
                .is_some()
        );
        assert!(
            s.add(Clause::new(vec![], vec![Lit::x(3), Lit::x(4), Lit::x(9)]))
                .is_none()
        );
        assert!(s.add(Clause::new(vec![], vec![Lit::x(3)])).is_some());
        assert!(s.clauses[0].dead);
    }

    #[test]
    fn literals_pack_and_unpack() {
        let l = Lit::f(123_456, 789);
        assert_eq!((l.kind(), l.fun(), l.atom()), (F, 123_456, 789));
        assert!(Lit::rl(5, 0) > Lit::f(5, u32::MAX));
        assert_eq!(Lit::eq(Term::Fun(3), Term::Fun(3)), Made::True);
        assert_eq!(Lit::ne(Term::Y, Term::Y), Made::False);
        let Made::Lit(e) = Lit::eq(Term::Y, Term::Fun(9)) else {
            panic!()
        };
        assert_eq!((e.kind(), e.fun(), e.term()), (EQ, 9, Term::Y));
        let Made::Lit(g) = Lit::eq(Term::Fun(9), Term::Fun(4)) else {
            panic!()
        };
        assert!(g.succ_key() > e.succ_key());
        assert!(Lit::f(9, 1).succ_key() > g.succ_key());
        assert!(Lit::x(1) > Lit::ry(u32::MAX));
    }

    #[test]
    fn the_index_by_successor_literal_is_counted() {
        // what the run's estimate is handed covers what the index holds, from its first
        // key on, with a key per clause and with many clauses under one key
        let mut s = Store::default();
        let mut counted = 0;
        let held = |s: &Store| {
            table_bytes(s.by_succ.capacity(), 32)
                + s.by_succ.values().map(|v| v.capacity() * 4).sum::<usize>()
        };
        for i in 0..5000u32 {
            s.index_succ(Lit::f(i % 97, i), i);
            counted += s.take_grown();
            assert!(
                counted >= held(&s),
                "{counted} counted for {} held",
                held(&s)
            );
        }
        for i in 0..5000u32 {
            s.index_succ(Lit::f(1, 0), i);
            counted += s.take_grown();
            assert!(
                counted >= held(&s),
                "{counted} counted for {} held",
                held(&s)
            );
        }
        assert_eq!(s.take_grown(), 0, "the bytes are handed over once");
    }

    /// A role's inverse for the tests: roles pair up as 2k and 2k + 1.
    fn inv(r: Role) -> Option<Role> {
        Some(r ^ 1)
    }

    #[test]
    fn a_literal_the_packed_forms_hold_stays_packed() {
        use RTerm::*;
        let lits = Lits::default();
        let cases = [
            (XLit::Atom(4, X), Lit::x(4)),
            (XLit::Atom(4, Y), Lit::y(4)),
            (XLit::Atom(4, Fx(9)), Lit::f(9, 4)),
            (XLit::Role(2, Y, X), Lit::ry(2)),
            (XLit::Role(2, X, Y), Lit::ry(3)),
            (XLit::Role(2, X, Fx(9)), Lit::rl(9, 2)),
            (XLit::Role(2, Fx(9), X), Lit::rl(9, 3)),
        ];
        for (x, l) in cases {
            assert_eq!(lits.lit(x, inv), Made::Lit(l), "{x:?}");
            assert_eq!(lits.view(l, inv), x.canon(inv), "{x:?}");
        }
        for (a, b) in [(Fx(9), Y), (Y, Fx(9)), (Fx(3), Fx(9))] {
            let Made::Lit(e) = lits.lit(XLit::Eq(a, b), inv) else {
                panic!()
            };
            assert_eq!(e.kind(), EQ);
            assert_eq!(lits.view(e, inv), XLit::Eq(a, b).canon(inv));
            let Made::Lit(n) = lits.lit(XLit::Ne(a, b), inv) else {
                panic!()
            };
            assert_eq!(n.kind(), NE);
        }
        assert!(lits.is_empty(), "nothing packed is interned");
    }

    #[test]
    fn a_literal_with_a_constant_is_interned_once() {
        use RTerm::*;
        let lits = Lits::default();
        let get = |x: XLit| match lits.lit(x, inv) {
            Made::Lit(l) => l,
            m => panic!("{x:?} gave {m:?}"),
        };
        let a = get(XLit::Atom(5, C(1)));
        assert_eq!(a.kind(), XL);
        assert_eq!(get(XLit::Atom(5, C(1))), a);
        assert_ne!(get(XLit::Atom(5, C(2))), a);
        assert_ne!(get(XLit::Atom(6, C(1))), a);
        // an equality's terms and a role atom's direction have one form
        assert_eq!(get(XLit::Eq(X, C(3))), get(XLit::Eq(C(3), X)));
        assert_ne!(get(XLit::Eq(X, C(3))), get(XLit::Ne(X, C(3))));
        assert_eq!(get(XLit::Role(4, X, C(3))), get(XLit::Role(5, C(3), X)));
        assert_ne!(get(XLit::Role(4, X, C(3))), get(XLit::Role(4, C(3), X)));
        // the loop and the equality of x and y have no packed form
        assert_eq!(get(XLit::Role(4, X, X)).kind(), XL);
        assert_eq!(get(XLit::Eq(X, Y)).kind(), XL);
        assert_eq!(get(XLit::Atom(5, Fc(9, 2))).kind(), XL);
        assert_eq!(lits.lit(XLit::Eq(C(3), C(3)), inv), Made::True);
        assert_eq!(lits.lit(XLit::Ne(C(3), C(3)), inv), Made::False);
        assert_eq!(
            lits.view(get(XLit::Role(5, C(3), X)), inv),
            XLit::Role(4, X, C(3))
        );
        assert_eq!(lits.view(a, inv), XLit::Atom(5, C(1)));
        assert!(lits.bytes() > 0);
    }

    #[test]
    fn a_loop_by_a_role_or_its_inverse_is_one_literal() {
        use RTerm::*;
        let lits = Lits::default();
        for t in [X, C(0)] {
            assert_eq!(
                lits.lit(XLit::Role(4, t, t), inv),
                lits.lit(XLit::Role(5, t, t), inv)
            );
        }
    }

    #[test]
    fn the_interned_literals_map_is_counted_by_its_entries() {
        let lits = Lits::default();
        for i in 0..5000 {
            lits.lit(XLit::Atom(i, RTerm::C(1)), inv);
        }
        let map = table_bytes(
            lits.ids.lock().capacity(),
            std::mem::size_of::<(XLit, u32)>(),
        )
            // the segments of 1,024, 2,048 and 4,096 records that 5,000 fill
            + 7168 * std::mem::size_of::<OnceLock<XLit>>();
        assert!(
            lits.bytes() >= map,
            "{} counted for a map of {map}",
            lits.bytes()
        );
    }

    #[test]
    fn interned_literals_sort_after_the_packed_ones_and_no_order_selects_them() {
        let lits = Lits::default();
        let Made::Lit(g) = lits.lit(XLit::Atom(1, RTerm::C(0)), inv) else {
            panic!()
        };
        let Made::Lit(e) = lits.lit(XLit::Eq(RTerm::Fx(u32::MAX >> 3), RTerm::Y), inv) else {
            panic!()
        };
        for l in [
            Lit::y(9),
            Lit::ry(9),
            Lit::x(9),
            Lit::f(9, 9),
            Lit::rl(9, 9),
            e,
        ] {
            assert!(g > l, "{l:?}");
        }
        // the order of the runs without constants never selects an interned literal
        let c = Clause::new(vec![], vec![Lit::x(3), g]);
        let mut max = Vec::new();
        c.maximal(&Index::default(), &mut max);
        assert!(max.iter().all(|l| l.kind() != XL), "{max:?}");
    }

    #[test]
    fn workers_intern_one_literal_once() {
        let lits = Lits::default();
        let ids: Vec<Vec<Lit>> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..4)
                .map(|_| {
                    s.spawn(|| {
                        (0..3000u32)
                            .map(
                                |i| match lits.lit(XLit::Atom(i % 1500, RTerm::C(i % 7)), inv) {
                                    Made::Lit(l) => l,
                                    _ => unreachable!(),
                                },
                            )
                            .collect()
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(ids.windows(2).all(|w| w[0] == w[1]));
        // i mod 1500 and i mod 7 repeat together only after 10,500
        assert_eq!(lits.len(), 3000);
        for (i, l) in ids[0].iter().enumerate() {
            let i = i as u32;
            assert_eq!(lits.view(*l, inv), XLit::Atom(i % 1500, RTerm::C(i % 7)));
        }
    }

    /// An index with two named classes, a fresh class and an existential restriction, for
    /// the rooted order's ranks.
    fn ranked() -> (Index, [Ex; 4]) {
        use super::super::index::{Axioms, Interner, RoleName};
        use crate::model::{Class, ObjectProperty};
        let mut it = Interner::new();
        let a = it.class(&Class::new("urn:t:a"));
        let b = it.class(&Class::new("urn:t:b"));
        let fresh = it.node(Node::Fresh(0));
        let r = it.role(RoleName::Object(ObjectProperty::new("urn:t:r")));
        let e = it.node(Node::Exist(r, a));
        let ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
        (ix, [a, b, fresh, e])
    }

    /// Every literal of a small signature, by where it can occur: those of a context other
    /// than the root, and those of the root, which mentions no x and no `f(x)`.
    fn universes(classes: &[Ex]) -> [Vec<XLit>; 2] {
        let all = universe(classes);
        let fc = |t: &RTerm| matches!(t, RTerm::Fc(..));
        let xs = |t: &RTerm| matches!(t, RTerm::X | RTerm::Fx(_));
        let terms = |l: &XLit| match *l {
            XLit::Atom(_, a) => vec![a],
            XLit::Role(_, a, b) | XLit::Eq(a, b) | XLit::Ne(a, b) => vec![a, b],
        };
        [
            all.iter()
                .filter(|l| !terms(l).iter().any(fc))
                .copied()
                .collect(),
            all.iter()
                .filter(|l| !terms(l).iter().any(xs))
                .copied()
                .collect(),
        ]
    }

    /// Every literal of a small signature: atoms and role atoms over the terms of a context
    /// and of the root, and equalities and inequalities of a-terms.
    fn universe(classes: &[Ex]) -> Vec<XLit> {
        use RTerm::*;
        let terms = [X, Y, Fx(1), Fx(2), C(0), C(1)];
        let root = [Y, C(0), C(1), Fc(1, 0), Fc(2, 1)];
        let mut out = Vec::new();
        for ts in [&terms[..], &root[..]] {
            for &t in ts {
                for &c in classes {
                    out.push(XLit::Atom(c, t));
                }
            }
        }
        // the role atoms of Definition 2
        let pairs = [
            (X, X),
            (X, Y),
            (Y, X),
            (X, Fx(1)),
            (Fx(2), X),
            (X, C(0)),
            (C(1), X),
            (C(0), Y),
            (Y, C(1)),
            (C(0), C(1)),
            (C(1), Fc(1, 1)),
            (Fc(2, 0), C(0)),
        ];
        for (a, b) in pairs {
            for r in [4, 5] {
                out.push(XLit::Role(r, a, b).canon(inv));
            }
        }
        let all = [X, Y, Fx(1), Fx(2), C(0), C(1)];
        for &a in &all {
            for &b in &all {
                if a < b {
                    out.push(XLit::Eq(a, b));
                    out.push(XLit::Ne(a, b));
                }
            }
        }
        for (a, b) in [(Fc(1, 0), C(1)), (Fc(2, 1), Fc(1, 0)), (Y, Fc(1, 1))] {
            out.push(XLit::Eq(a, b).canon(inv));
        }
        out.sort_by_key(|l| format!("{l:?}"));
        out.dedup();
        out
    }

    #[test]
    fn the_rooted_order_is_a_strict_order() {
        for ground_total in [false, true] {
            strict_order(ground_total);
        }
    }

    fn strict_order(ground_total: bool) {
        let (ix, [a, b, fresh, e]) = ranked();
        let lits = Lits::default();
        let o = Rooted {
            ix: &ix,
            lits: &lits,
            inv: &inv,
            ground_total,
        };
        for u in universes(&[a, b, fresh, e]) {
            let gt: Vec<Vec<bool>> = u
                .iter()
                .map(|&p| u.iter().map(|&q| o.gt(p, q)).collect())
                .collect();
            for i in 0..u.len() {
                assert!(!gt[i][i], "{:?} above itself", u[i]);
                for j in 0..u.len() {
                    assert!(
                        !(gt[i][j] && gt[j][i]),
                        "{:?} and {:?} above each other",
                        u[i],
                        u[j]
                    );
                    if gt[i][j] {
                        for k in 0..u.len() {
                            assert!(
                                !gt[j][k] || gt[i][k],
                                "{:?} > {:?} > {:?} but not the first above the last",
                                u[i],
                                u[j],
                                u[k]
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_rooted_order_keeps_the_admissibility_conditions() {
        use RTerm::*;
        let (ix, [a, b, fresh, e]) = ranked();
        let lits = Lits::default();
        let o = Rooted {
            ix: &ix,
            lits: &lits,
            inv: &inv,
            ground_total: false,
        };
        let u = universe(&[a, b, fresh, e]);
        let consts = |l: &XLit| {
            let (ts, n) = Rooted::args(*l);
            ts[..n].iter().any(|t| matches!(t, C(_)))
        };
        let mentions_y = |l: &XLit| {
            let (ts, n) = Rooted::args(*l);
            ts[..n].contains(&Y)
        };
        let fun_free = |l: &XLit| {
            let (ts, n) = Rooted::args(*l);
            !ts[..n].iter().any(|t| matches!(t, Fx(_) | Fc(..)))
        };
        for &p in &u {
            if !matches!(p, XLit::Atom(..) | XLit::Role(..)) {
                continue;
            }
            for &q in &u {
                if !matches!(q, XLit::Atom(..) | XLit::Role(..)) || !o.gt(p, q) {
                    continue;
                }
                // condition 5: a function-free atom that mentions y or a constant is above
                // only atoms with its terms replaced by smaller ones
                if fun_free(&p) && (mentions_y(&p) || consts(&p)) {
                    assert!(o.smaller_terms(p, q), "{p:?} above {q:?}");
                }
                // condition C2 of Theorem 2: a named class over x is above only literals
                // that mention y
                if let XLit::Atom(c, X) = p
                    && (c == a || c == b)
                {
                    assert!(mentions_y(&q), "{p:?} above {q:?}");
                }
            }
            // condition 2: an atom is above x, and no constant is above an atom
            assert!(o.t_gt(OTerm::P(p), OTerm::A(X)));
            for c in [C(0), C(1)] {
                assert!(!o.t_gt(OTerm::A(c), OTerm::P(p)));
            }
        }
        // condition 3: x and y are incomparable with constants, and f(x) is above them
        for s in [X, Y] {
            for c in [C(0), C(1)] {
                assert!(!Rooted::a_gt(s, c) && !Rooted::a_gt(c, s));
            }
        }
        assert!(Rooted::a_gt(Fx(1), C(1)) && Rooted::a_gt(X, Y) && Rooted::a_gt(Fx(2), Fx(1)));
        // monotonicity: a smaller term in an atom gives a smaller atom
        for c in [a, fresh] {
            assert!(o.gt(XLit::Atom(c, Fx(1)), XLit::Atom(c, X)));
            assert!(o.gt(XLit::Atom(c, X), XLit::Atom(c, Y)));
            assert!(o.gt(XLit::Atom(c, C(1)), XLit::Atom(c, C(0))));
            assert!(o.gt(XLit::Atom(c, Fc(1, 1)), XLit::Atom(c, C(1))));
        }
    }

    /// Item 804's shared order on ground literals makes them totally ordered, with a role
    /// atom above a class atom and an atom above an equality or inequality of constants,
    /// and otherwise adds only that a ground atom is above every equality of a constant.
    #[test]
    fn the_shared_ground_order_is_total_and_adds_only_ground_comparisons() {
        use RTerm::*;
        let (ix, [a, b, fresh, e]) = ranked();
        let lits = Lits::default();
        let weak = Rooted {
            ix: &ix,
            lits: &lits,
            inv: &inv,
            ground_total: false,
        };
        let total = Rooted {
            ix: &ix,
            lits: &lits,
            inv: &inv,
            ground_total: true,
        };
        let u = universe(&[a, b, fresh, e]);
        let mut ground = 0;
        for &p in &u {
            for &q in &u {
                if Rooted::plain_ground(p) && Rooted::plain_ground(q) {
                    ground += 1;
                    assert!(
                        p == q || total.gt(p, q) != total.gt(q, p),
                        "{p:?} and {q:?} not ordered"
                    );
                    if weak.gt(p, q) {
                        assert!(total.gt(p, q), "{p:?} above {q:?} only without the order");
                    }
                } else if !(Rooted::plain_ground(p) && matches!(q, XLit::Eq(..) | XLit::Ne(..))) {
                    assert_eq!(weak.gt(p, q), total.gt(p, q), "{p:?} against {q:?} changed");
                } else if weak.gt(p, q) {
                    assert!(total.gt(p, q), "{p:?} above {q:?} only without the order");
                }
            }
        }
        assert!(ground > 0);
        assert!(total.gt(XLit::Role(4, C(0), C(1)).canon(inv), XLit::Atom(a, C(1))));
        assert!(total.gt(XLit::Atom(a, C(0)), XLit::Ne(C(0), C(1))));
        assert!(total.gt(XLit::Atom(a, C(0)), XLit::Eq(C(0), C(1))));
        assert!(total.gt(XLit::Atom(a, C(0)), XLit::Eq(X, C(1))));
        assert!(!total.gt(XLit::Atom(a, C(0)), XLit::Eq(Fx(1), C(1))));
        assert!(!total.gt(XLit::Atom(a, C(0)), XLit::Atom(a, X)));
        assert!(!total.gt(XLit::Atom(a, X), XLit::Atom(a, C(0))));
    }

    #[test]
    fn a_rooted_clause_keeps_every_incomparable_maximal_literal() {
        use RTerm::*;
        let (ix, [a, b, fresh, e]) = ranked();
        let lits = Lits::default();
        let o = Rooted {
            ix: &ix,
            lits: &lits,
            inv: &inv,
            ground_total: false,
        };
        let l = |x: XLit| match lits.lit(x, inv) {
            Made::Lit(l) => l,
            m => panic!("{m:?}"),
        };
        let mut max = Vec::new();
        // two named classes and a ground atom beside them are all maximal
        let ground = l(XLit::Atom(a, C(0)));
        let c = Clause::new(vec![], vec![Lit::x(a), Lit::x(b), ground, Lit::y(a)]);
        c.maximal_rooted(&o, &mut max);
        max.sort_unstable();
        let mut want = vec![Lit::x(a), Lit::x(b), ground];
        want.sort_unstable();
        assert_eq!(max, want);
        // a fresh class is above the named ones, but not above the ground atom
        let c = Clause::new(vec![], vec![Lit::x(a), Lit::x(fresh), ground]);
        c.maximal_rooted(&o, &mut max);
        max.sort_unstable();
        let mut want = vec![Lit::x(fresh), ground];
        want.sort_unstable();
        assert_eq!(max, want);
        // an atom over a successor term is above all of them, and x ≈ u beside it stays
        // maximal only when nothing is above it
        let xu = l(XLit::Eq(X, C(0)));
        // every atom is above x, so an atom over u is above x ≈ u
        let c = Clause::new(vec![], vec![Lit::x(a), Lit::f(e, b), ground, xu]);
        c.maximal_rooted(&o, &mut max);
        assert_eq!(max, vec![Lit::f(e, b)]);
        let c = Clause::new(vec![], vec![Lit::x(a), ground, xu]);
        c.maximal_rooted(&o, &mut max);
        max.sort_unstable();
        let mut want = vec![Lit::x(a), ground];
        want.sort_unstable();
        assert_eq!(max, want, "the ground atom is above x ≈ u");
        // a named class over x is not
        let c = Clause::new(vec![], vec![Lit::x(a), xu]);
        c.maximal_rooted(&o, &mut max);
        assert_eq!(max.len(), 2, "{max:?}");
        // the larger of two constants, and a head over y alone has none
        let c = Clause::new(vec![], vec![ground, l(XLit::Atom(a, C(1)))]);
        c.maximal_rooted(&o, &mut max);
        assert_eq!(max, vec![l(XLit::Atom(a, C(1)))]);
        let c = Clause::new(vec![], vec![Lit::y(a), Lit::ry(4), l(XLit::Eq(X, Y))]);
        c.maximal_rooted(&o, &mut max);
        assert!(max.is_empty(), "{max:?}");
    }

    #[test]
    fn an_inequality_and_an_equality_of_x_and_y_can_be_maximal() {
        use RTerm::*;
        let (ix, [a, ..]) = ranked();
        let lits = Lits::default();
        let o = Rooted {
            ix: &ix,
            lits: &lits,
            inv: &inv,
            ground_total: false,
        };
        let mut max = Vec::new();
        for x in [XLit::Ne(X, Y), XLit::Eq(X, Y)] {
            let Made::Lit(l) = lits.lit(x, inv) else {
                panic!()
            };
            // Eq rewrites x in both with x ≈ u, and x ≉ y is no predecessor trigger
            Clause::new(vec![Lit::x(a)], vec![l]).maximal_rooted(&o, &mut max);
            assert_eq!(max, vec![l], "{x:?}");
        }
    }

    #[test]
    fn the_index_by_interned_literal_is_counted_by_what_it_holds() {
        use RTerm::*;
        let (ix, [a, ..]) = ranked();
        let lits = Lits::default();
        let o = Rooted {
            ix: &ix,
            lits: &lits,
            inv: &inv,
            ground_total: false,
        };
        let Made::Lit(g) = lits.lit(XLit::Atom(a, C(0)), inv) else {
            panic!()
        };
        // the index by interned literal, and the index by term beside it
        let held = |s: &Store| {
            table_bytes(s.by_xl.capacity(), 32)
                + s.by_xl.values().map(|v| v.capacity() * 4).sum::<usize>()
                + s.terms.as_ref().map_or(0, |t| t.bytes())
        };
        let mut s = Store::default();
        let mut max = Vec::new();
        let mut counted = 0;
        for i in 0..2000 {
            s.add(Clause::new(vec![Lit::x(1000 + i)], vec![g]));
            s.next(&ix, Some(&o), &mut max);
            counted += s.take_grown();
            assert!(
                counted >= held(&s),
                "{counted} counted for {} held",
                held(&s)
            );
        }
        // one key holds every clause, so what is counted grows with the list alone
        assert!(
            counted <= 2 * held(&s) + 64,
            "{counted} counted for {} held",
            held(&s)
        );
    }

    #[test]
    fn a_rooted_store_indexes_every_maximal_literal() {
        use RTerm::*;
        let (ix, [a, b, _, e]) = ranked();
        let lits = Lits::default();
        let o = Rooted {
            ix: &ix,
            lits: &lits,
            inv: &inv,
            ground_total: false,
        };
        let Made::Lit(g) = lits.lit(XLit::Atom(a, C(0)), inv) else {
            panic!()
        };
        let Made::Lit(fu) = lits.lit(XLit::Eq(Fx(e), C(0)), inv) else {
            panic!()
        };
        let mut s = Store::default();
        s.add(Clause::new(vec![], vec![Lit::x(a), Lit::x(b), g]));
        // f(x) ≈ u and f(x) ≈ y are incomparable, and both are maximal
        let Made::Lit(fy) = Lit::eq(Term::Fun(e), Term::Y) else {
            panic!()
        };
        s.add(Clause::new(vec![], vec![fy, fu]));
        let mut max = Vec::new();
        let i = s.next(&ix, Some(&o), &mut max).unwrap();
        let j = s.next(&ix, Some(&o), &mut max).unwrap();
        let (i, j) = if s.maxima(i).len() == 3 {
            (i, j)
        } else {
            (j, i)
        };
        assert_eq!(s.maxima(i).len(), 3);
        assert_eq!(s.with_x(a).collect::<Vec<_>>(), vec![i]);
        assert_eq!(s.with_x(b).collect::<Vec<_>>(), vec![i]);
        assert_eq!(s.by_xl.get(&g), Some(&vec![i]));
        assert_eq!(s.by_xl.get(&fu), Some(&vec![j]));
        assert_eq!(s.with_succ(fy).collect::<Vec<_>>(), vec![j]);
        assert_eq!(s.over(e).collect::<Vec<_>>(), vec![(j, fy)]);
        assert_eq!(s.max_of(i, |l| l == g), Some(g));
        assert!(s.take_grown() > 0);
    }

    #[test]
    fn the_table_hands_back_what_it_holds() {
        let t = Table::default();
        let ids: Vec<u32> = (0..3000)
            .map(|i| t.push(Clause::new(vec![], vec![Lit::x(i)])))
            .collect();
        for (i, id) in ids.into_iter().enumerate() {
            assert_eq!(t.get(id).head(), &[Lit::x(i as u32)]);
        }
    }

    #[test]
    fn clause_blocker_checks_exact_subsumption_and_charges_every_inspected_entry() {
        let mut s = Store::default();
        let (p, q, r, g1, g2, w2) = (10, 11, 12, 20, 21, 22);
        // C: P -> Q v G2 (also present in store so we verify C never self-blocks)
        let c = Clause::new(vec![Lit::x(p)], vec![Lit::x(q), Lit::x(g2)]);
        s.add(c.clone());
        // Dead and non-subsuming clauses carrying G1 must still be counted as reads.
        let dead_id = s
            .add(Clause::new(
                vec![Lit::x(p)],
                vec![Lit::x(q), Lit::x(r), Lit::x(g1)],
            ))
            .is_some();
        assert!(dead_id);
        s.clauses.last_mut().unwrap().dead = true;
        // Clause with extra literal R outside Q v W2 v {G1} must NOT block C.
        s.add(Clause::new(
            vec![Lit::x(p)],
            vec![Lit::x(q), Lit::x(r), Lit::x(g1)],
        ));
        // Clause with extra body literal R must NOT block C.
        s.add(Clause::new(
            vec![Lit::x(p), Lit::x(r)],
            vec![Lit::x(q), Lit::x(g1)],
        ));
        // Clause whose head contains G2 itself must NOT block C (resolved G2 is excluded).
        s.add(Clause::new(vec![Lit::x(p)], vec![Lit::x(g1), Lit::x(g2)]));

        let mut reads = 0usize;
        let hit = s.has_clause_blocker(
            c.body(),
            c.head(),
            Lit::x(g2),
            Lit::x(w2),
            &[g1],
            16,
            &mut reads,
            |_| true,
        );
        assert!(!hit, "non-subsuming clauses must not block C");
        assert_eq!(
            reads, 9,
            "setup (1 body + 2 head + 1 blocker), 1 blocker-key probe, and 4 by_lit entries must be counted"
        );

        // Stopping early via keep_going must terminate the scan and return false.
        let mut reads_stop = 0usize;
        let mut ticks = 0usize;
        let hit_stop = s.has_clause_blocker(
            c.body(),
            c.head(),
            Lit::x(g2),
            Lit::x(w2),
            &[g1],
            2,
            &mut reads_stop,
            |r| {
                ticks += 1;
                *r = 0;
                false
            },
        );
        assert!(!hit_stop);
        assert_eq!(ticks, 1);

        // Long blocker-key lists that miss `by_lit` completely must still charge setup and
        // key probes and abort when `keep_going` returns false.
        let missed: Vec<Ex> = (100..164).collect();
        let mut missed_reads = 0usize;
        let hit_missed = s.has_clause_blocker(
            c.body(),
            c.head(),
            Lit::x(g2),
            Lit::x(w2),
            &missed,
            1024,
            &mut missed_reads,
            |_| true,
        );
        assert!(!hit_missed);
        assert_eq!(missed_reads, 1 + 2 + 64 + 64);

        let mut missed_stop_reads = 0usize;
        let mut missed_ticks = 0usize;
        let hit_missed_stop = s.has_clause_blocker(
            c.body(),
            c.head(),
            Lit::x(g2),
            Lit::x(w2),
            &missed,
            16,
            &mut missed_stop_reads,
            |r| {
                missed_ticks += 1;
                *r = 0;
                false
            },
        );
        assert!(!hit_missed_stop);
        assert_eq!(missed_ticks, 1);

        // Now add a live unprocessed clause D: P -> Q v G1 subsuming C's weakened head.
        s.add(Clause::new(vec![Lit::x(p)], vec![Lit::x(q), Lit::x(g1)]));
        let mut reads_hit = 0usize;
        let hit2 = s.has_clause_blocker(
            c.body(),
            c.head(),
            Lit::x(g2),
            Lit::x(w2),
            &[g1],
            16,
            &mut reads_hit,
            |_| true,
        );
        assert!(hit2, "live unprocessed D: P -> Q v G1 must block C");
        assert!(reads_hit >= 1);
    }
}
