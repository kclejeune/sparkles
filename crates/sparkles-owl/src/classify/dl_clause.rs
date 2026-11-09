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
            .filter(|l| l.kind() >= F)
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

/// One segment of a [`Table`], made on first use.
type Segment = OnceLock<Box<[OnceLock<Clause>]>>;

/// The clauses that contexts send one another, by id: an append-only table whose entries
/// never move, so that a worker reads one without a lock.
pub struct Table {
    next: AtomicU32,
    segs: Box<[Segment]>,
    bytes: AtomicUsize,
}

/// The entries of the first segment of [`Table`]; each further one doubles.
const SEG0: usize = 1024;

impl Default for Table {
    fn default() -> Table {
        Table {
            next: AtomicU32::new(0),
            segs: (0..32).map(|_| OnceLock::new()).collect(),
            bytes: AtomicUsize::new(0),
        }
    }
}

impl Table {
    fn slot(&self, id: u32) -> &OnceLock<Clause> {
        let off = id as usize;
        let k = (off / SEG0 + 1).ilog2() as usize;
        let at = off - SEG0 * ((1 << k) - 1);
        let seg = self.segs[k].get_or_init(|| {
            self.bytes.fetch_add(
                (SEG0 << k) * std::mem::size_of::<OnceLock<Clause>>(),
                Ordering::Relaxed,
            );
            (0..SEG0 << k).map(|_| OnceLock::new()).collect()
        });
        &seg[at]
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

/// The bytes one key of [`Store`]'s index by maximal successor literal takes at most: a
/// table entry of the literal and a list, twice over for the table's room to grow, its
/// control byte, and the list's first allocation.
const SUCC_KEY: usize = 2 * 32 + 1 + 16 + 15;

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
        // the maximal-literal indexes once processed, with room for their lists to double
        let bytes = c.bytes() + 8 * (c.head().len() + 2) + 16;
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
                    RL => {
                        self.by_f.entry(l.fun()).or_default().push(id);
                        self.grown += self.index_succ(l, id);
                        self.by_rl.entry(l.atom()).or_default().push(id);
                    }
                    _ => {
                        self.by_f.entry(l.fun()).or_default().push(id);
                        self.grown += self.index_succ(l, id);
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

    /// Indexes a processed clause by its maximal literal over a successor term. The answer
    /// is the bytes a new key takes, its table entry with the table's room to grow and its
    /// list's first allocation, since a store can hold as many keys as clauses.
    fn index_succ(&mut self, l: Lit, id: u32) -> usize {
        match self.by_succ.entry(l) {
            std::collections::hash_map::Entry::Occupied(mut o) => {
                o.get_mut().push(id);
                0
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(vec![id]);
                SUCC_KEY
            }
        }
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
        // as many keys as clauses, the case the per-clause allowance alone misses
        let mut s = Store::default();
        let mut counted = 0;
        for i in 0..5000u32 {
            counted += s.index_succ(Lit::f(i % 97, i), i);
            // the per-clause allowance of `add` for the id in the list
            counted += 8;
        }
        // and many clauses under one key
        for i in 0..5000u32 {
            counted += s.index_succ(Lit::f(1, 0), i);
            counted += 8;
        }
        let held = table_bytes(s.by_succ.capacity(), 32)
            + s.by_succ.values().map(|v| v.capacity() * 4).sum::<usize>();
        assert!(counted >= held, "{counted} counted for {held} held");
        assert_eq!(
            s.take_grown(),
            0,
            "index_succ alone leaves the processing total"
        );
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
