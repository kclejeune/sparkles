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
use super::saturate::table_bytes;
use rustc_hash::FxHashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

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
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the rooted rules of item 603 phase 6 read it")
    )]
    #[inline]
    fn xl(id: u32) -> Lit {
        Lit((XL << KIND_SHIFT) | id as u64)
    }
    /// The id in the run's [`Lits`] of a literal of kind [`XL`].
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the rooted rules of item 603 phase 6 read it")
    )]
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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the rooted rules of item 603 phase 6 read it")
)]
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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the rooted rules of item 603 phase 6 read it")
)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum XLit {
    Atom(Ex, RTerm),
    Role(Role, RTerm, RTerm),
    Eq(RTerm, RTerm),
    Ne(RTerm, RTerm),
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the rooted rules of item 603 phase 6 read it")
)]
impl XLit {
    /// The canonical form: an equality's terms and a role atom's in order, the role atom
    /// read by the inverse role when it swaps them, which `inv` gives.
    pub fn canon(self, inv: impl Fn(Role) -> Option<Role>) -> XLit {
        match self {
            XLit::Role(s, a, b) if a > b => match inv(s) {
                Some(i) => XLit::Role(i, b, a),
                None => self,
            },
            XLit::Eq(a, b) if a > b => XLit::Eq(b, a),
            XLit::Ne(a, b) if a > b => XLit::Ne(b, a),
            _ => self,
        }
    }
}

/// The literals of a rooted run that the packed forms cannot hold, interned: a literal of
/// kind [`XL`] holds its id here. Entries never move, so a worker reads one without a lock,
/// and only a new literal takes the lock of the map.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the rooted rules of item 603 phase 6 read it")
)]
#[derive(Default)]
pub struct Lits {
    ids: parking_lot::Mutex<FxHashMap<XLit, u32>>,
    recs: Segs<XLit>,
    next: AtomicU32,
    bytes: AtomicUsize,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the rooted rules of item 603 phase 6 read it")
)]
impl Lits {
    /// The literal of `l`, packed when one of the six packed forms holds it and interned
    /// otherwise, or true or false for an equality or inequality of a term with itself.
    /// `inv` gives a role's inverse.
    pub fn lit(&self, l: XLit, inv: impl Fn(Role) -> Option<Role> + Copy) -> Made {
        use RTerm::*;
        let packed = match l {
            XLit::Atom(a, X) => Some(Lit::x(a)),
            XLit::Atom(a, Y) => Some(Lit::y(a)),
            XLit::Atom(a, Fx(e)) => Some(Lit::f(e, a)),
            XLit::Role(s, Y, X) => Some(Lit::ry(s)),
            XLit::Role(s, X, Y) => inv(s).map(Lit::ry),
            XLit::Role(s, X, Fx(f)) => Some(Lit::rl(f, s)),
            XLit::Role(s, Fx(f), X) => inv(s).map(|i| Lit::rl(f, i)),
            XLit::Eq(a, b) | XLit::Ne(a, b) if a == b => {
                return if matches!(l, XLit::Eq(..)) {
                    Made::True
                } else {
                    Made::False
                };
            }
            XLit::Eq(a, b) => packed_pair(EQ, a, b),
            XLit::Ne(a, b) => packed_pair(NE, a, b),
            _ => None,
        };
        if let Some(p) = packed {
            return Made::Lit(p);
        }
        let l = l.canon(inv);
        let mut ids = self.ids.lock();
        let cap = ids.capacity();
        let id = *ids.entry(l).or_insert_with(|| {
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            let _ = self.recs.slot(id, &self.bytes).set(l);
            id
        });
        let grown = table_bytes(ids.capacity(), 24).saturating_sub(table_bytes(cap, 24));
        self.bytes.fetch_add(grown, Ordering::Relaxed);
        Made::Lit(Lit::xl(id))
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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the rooted rules of item 603 phase 6 read it")
)]
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

/// The key of the total order on the atoms over x: the fresh classes of the normalization
/// first, then the named classes, then the other expressions, each by id.
#[inline]
pub fn rank(ix: &Index, a: Ex) -> (u8, Ex) {
    let r = match ix.head(a).node {
        Node::Fresh(_) => 0,
        Node::Class(_) => 1,
        _ => 2,
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
        let b = body.len() as u32;
        body.extend(head);
        Clause {
            lits: body.into_boxed_slice(),
            body: b,
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
        self.lits.iter().fold(0, |s, l| {
            s | 1 << (l.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58)
        })
    }
    /// Whether this clause subsumes `other`: its body and its head are subsets of
    /// `other`'s.
    pub fn subsumes(&self, other: &Clause) -> bool {
        subset(self.body(), other.body()) && subset(self.head(), other.head())
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

/// Whether sorted `a` is a subset of sorted `b`.
fn subset(a: &[Lit], b: &[Lit]) -> bool {
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
    /// The slot of entry `id`, whose segment is made on first use and its bytes added to
    /// `bytes`.
    fn slot(&self, id: u32, bytes: &AtomicUsize) -> &OnceLock<T> {
        let off = id as usize;
        let k = (off / SEG0 + 1).ilog2() as usize;
        let at = off - SEG0 * ((1 << k) - 1);
        let seg = self.segs[k].get_or_init(|| {
            bytes.fetch_add(
                (SEG0 << k) * std::mem::size_of::<OnceLock<T>>(),
                Ordering::Relaxed,
            );
            (0..SEG0 << k).map(|_| OnceLock::new()).collect()
        });
        &seg[at]
    }
}

/// The clauses that contexts send one another, by id: an append-only table whose entries
/// never move, so that a worker reads one without a lock.
#[derive(Default)]
pub struct Table {
    next: AtomicU32,
    segs: Segs<Clause>,
    bytes: AtomicUsize,
}

impl Table {
    fn slot(&self, id: u32) -> &OnceLock<Clause> {
        self.segs.slot(id, &self.bytes)
    }

    pub fn push(&self, c: Clause) -> u32 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(c.bytes(), Ordering::Relaxed);
        let _ = self.slot(id).set(c);
        id
    }

    pub fn get(&self, id: u32) -> &Clause {
        self.slot(id).get().expect("a clause the table holds")
    }

    pub fn bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }
}

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
}

/// A context's clause store, with the indexes the rules read.
#[derive(Default, Clone)]
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
    /// the processed clauses whose head is over y alone
    pub preds: Vec<u32>,
    /// every clause by each literal of its head, for the clauses a new one subsumes
    by_lit: FxHashMap<Lit, Vec<u32>>,
    /// every clause by the last literal of its head, which is the most specific, for
    /// whether a new one is subsumed
    by_last: FxHashMap<Lit, Vec<u32>>,
    /// the clauses with an empty head
    empty: Vec<u32>,
    bytes: usize,
    /// whether a stop of the run cut off an inference of the rules, whose conclusions the
    /// store may lack
    pub interrupted: bool,
}

impl Store {
    pub fn bytes(&self) -> usize {
        self.bytes
            + self.clauses.capacity() * std::mem::size_of::<Stored>()
            + table_bytes(self.by_x.capacity(), 32)
            + table_bytes(self.by_f.capacity(), 32)
            + table_bytes(self.by_succ.capacity(), 32)
            + table_bytes(self.by_rl.capacity(), 32)
            + table_bytes(self.by_cert.capacity(), 32)
            + table_bytes(self.by_eq.capacity(), 32)
            + table_bytes(self.by_lit.capacity(), 32)
            + table_bytes(self.by_last.capacity(), 32)
    }

    pub fn is_idle(&self) -> bool {
        self.queue.is_empty() && !self.interrupted
    }

    /// Whether a clause of the store that is not dead subsumes `c`, whose signature is
    /// `sig`: a clause that does has its last head literal in `c`'s head.
    fn subsumed(&self, c: &Clause, sig: u64) -> bool {
        let check = |ids: &[u32]| {
            ids.iter().any(|&i| {
                let s = &self.clauses[i as usize];
                s.sig & !sig == 0 && !s.dead && s.c.subsumes(c)
            })
        };
        if check(&self.empty) {
            return true;
        }
        c.head()
            .iter()
            .any(|l| self.by_last.get(l).is_some_and(|ids| check(ids)))
    }

    /// Adds a clause that is not subsumed, marks dead those it subsumes, and queues it.
    /// The answer is the number of bytes it added, or `None` when it was subsumed.
    pub fn add(&mut self, c: Clause) -> Option<usize> {
        let sig = c.sig();
        if self.subsumed(&c, sig) {
            return None;
        }
        // the clauses it subsumes have every literal of its head
        let every: Vec<u32>;
        let weaker: &[u32] = match c.head().first() {
            Some(l) => {
                let pick = c
                    .head()
                    .iter()
                    .min_by_key(|l| self.by_lit.get(l).map_or(0, |v| v.len()))
                    .unwrap_or(l);
                self.by_lit.get(pick).map_or(&[][..], |v| v.as_slice())
            }
            None => {
                every = (0..self.clauses.len() as u32).collect();
                &every
            }
        };
        for &i in weaker {
            let s = &mut self.clauses[i as usize];
            if sig & !s.sig == 0 && !s.dead && c.subsumes(&s.c) {
                s.dead = true;
            }
        }
        let id = self.clauses.len() as u32;
        // the head's entries in the literal indexes, the queue's entry, and the entries in
        // the maximal-literal indexes once processed; the index by successor literal counts
        // its own growth as it is made
        let bytes = c.bytes() + 8 * (c.head().len() + 2) + 8;
        match c.head().last() {
            None => self.empty.push(id),
            Some(&l) => self.by_last.entry(l).or_default().push(id),
        }
        for &l in c.head() {
            self.by_lit.entry(l).or_default().push(id);
            if matches!(l.kind(), EQ | NE)
                && let Term::Fun(g) = l.term()
            {
                for f in [l.fun(), g] {
                    let v = self.by_eq.entry(f).or_default();
                    if v.last() != Some(&id) {
                        v.push(id);
                    }
                }
            }
        }
        self.queue.push(std::cmp::Reverse((priority(&c), id)));
        self.clauses.push(Stored {
            c,
            sig,
            dead: false,
            done: false,
            sent: None,
            max: None,
        });
        self.bytes += bytes;
        Some(bytes)
    }

    /// The next clause to process, which is marked processed and indexed by its maximal
    /// literals.
    pub fn next(&mut self, ix: &Index, max: &mut Vec<Lit>) -> Option<u32> {
        while let Some(std::cmp::Reverse((_, id))) = self.queue.pop() {
            let s = &mut self.clauses[id as usize];
            if s.dead {
                continue;
            }
            s.done = true;
            s.c.maximal(ix, max);
            s.max = max.first().copied();
            if max.is_empty() {
                self.preds.push(id);
            }
            for &l in max.iter() {
                match l.kind() {
                    X => self.by_x.entry(l.atom()).or_default().push(id),
                    XL => unreachable!("a rooted literal in a store without the rooted order"),
                    RL => {
                        self.by_f.entry(l.fun()).or_default().push(id);
                        self.index_succ(l, id);
                        self.by_rl.entry(l.atom()).or_default().push(id);
                    }
                    _ => {
                        self.by_f.entry(l.fun()).or_default().push(id);
                        self.index_succ(l, id);
                    }
                }
            }
            if let Some(l) = self.clauses[id as usize].max
                && l.kind() == F
                && !ix.disjoint_of(ix.head(l.atom())).is_empty()
            {
                self.by_cert.entry(l.fun()).or_default().push(id);
            }
            return Some(id);
        }
        None
    }

    /// Indexes a processed clause by its maximal literal over a successor term, and adds
    /// what the table and the list grew by to the bytes the run's estimate has not seen,
    /// since a store can hold as many keys as clauses.
    fn index_succ(&mut self, l: Lit, id: u32) {
        let table = table_bytes(self.by_succ.capacity(), 32);
        let list = self.by_succ.entry(l).or_default();
        let cap = list.capacity();
        list.push(id);
        let list = (list.capacity() - cap) * 4;
        self.grown += table_bytes(self.by_succ.capacity(), 32).saturating_sub(table) + list;
    }

    /// The bytes processing added to the indexes since the last call, for the run's
    /// estimate.
    pub fn take_grown(&mut self) -> usize {
        std::mem::take(&mut self.grown)
    }

    /// The clauses that are not dead with `l` in the head.
    pub fn with_head(&self, l: Lit) -> Vec<u32> {
        self.by_lit
            .get(&l)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|&i| !self.clauses[i as usize].dead)
                    .collect()
            })
            .unwrap_or_default()
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
                !s.dead && s.max == Some(l)
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
                s.max.map(|l| (i, l))
            })
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
}
