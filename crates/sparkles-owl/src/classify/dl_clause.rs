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
        expect(dead_code, reason = "the rooted rules of item 603 phase 6 make it")
    )]
    #[inline]
    fn xl(id: u32) -> Lit {
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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the rooted rules of item 603 phase 6 make it")
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
}

/// The literals of a rooted run that the packed forms cannot hold, interned: a literal of
/// kind [`XL`] holds its id here. Entries never move, so a worker reads one without a lock,
/// and only a new literal takes the lock of the map.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the rooted rules of item 603 phase 6 make it")
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
    expect(dead_code, reason = "the rooted rules of item 603 phase 6 make it")
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
        let entry = std::mem::size_of::<(XLit, u32)>();
        let grown = table_bytes(ids.capacity(), entry).saturating_sub(table_bytes(cap, entry));
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
    expect(dead_code, reason = "the rooted rules of item 603 phase 6 make it")
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
    fn a_gt(s: RTerm, t: RTerm) -> bool {
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
                let (r, b) = rank(self.ix, b);
                (r != 1).then_some((r, b))
            }
            XLit::Role(s, ..) => Some((3, s)),
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
                matches!(t, RTerm::X | RTerm::Y) || ts[..n].iter().any(|&a| Rooted::a_ge(a, t))
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

    /// `a ≻ b` on literals: the multiset extension of the order on terms.
    pub fn gt(&self, a: XLit, b: XLit) -> bool {
        if a == b {
            return false;
        }
        let (ma, na) = Rooted::terms(a);
        let (mb, nb) = Rooted::terms(b);
        // the multiset differences
        let mut ra: Vec<OTerm> = ma[..na].to_vec();
        let mut rb: Vec<OTerm> = Vec::with_capacity(nb);
        for &t in &mb[..nb] {
            if let Some(k) = ra.iter().position(|&s| s == t) {
                ra.swap_remove(k);
            } else {
                rb.push(t);
            }
        }
        !ra.is_empty() && rb.iter().all(|&t| ra.iter().any(|&s| self.t_gt(s, t)))
    }

    /// Whether a literal is an atom over y alone, which only Pred reads: no rule selects
    /// it, and Eq never rewrites its y. An equality or inequality of x and y can be
    /// selected, since Eq rewrites its x.
    pub fn over_y(l: XLit) -> bool {
        matches!(l, XLit::Atom(..) | XLit::Role(..)) && Rooted::group(l) == Group::Y
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
    /// in a rooted run, the maximal literals of each processed clause that has more than
    /// one, whose first is [`Stored::max`]
    multi: FxHashMap<u32, Box<[Lit]>>,
    /// in a rooted run, the processed clauses by each of their maximal interned literals
    pub by_xl: FxHashMap<Lit, Vec<u32>>,
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
            + table_bytes(self.multi.capacity(), 24)
            + self.multi.values().map(|m| m.len() * 8).sum::<usize>()
            + table_bytes(self.by_xl.capacity(), 32)
            + self.by_xl.values().map(|v| v.capacity() * 4).sum::<usize>()
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
    /// literals, under the order of a rooted run when `rooted` is given.
    pub fn next(&mut self, ix: &Index, rooted: Option<&Rooted>, max: &mut Vec<Lit>) -> Option<u32> {
        while let Some(std::cmp::Reverse((_, id))) = self.queue.pop() {
            let s = &mut self.clauses[id as usize];
            if s.dead {
                continue;
            }
            s.done = true;
            match rooted {
                None => s.c.maximal(ix, max),
                Some(o) => s.c.maximal_rooted(o, max),
            }
            s.max = max.first().copied();
            if max.is_empty() {
                self.preds.push(id);
            }
            if max.len() > 1 {
                let bytes = table_bytes(self.multi.capacity(), 24);
                self.multi.insert(id, max.as_slice().into());
                self.grown +=
                    max.len() * 8 + table_bytes(self.multi.capacity(), 24).saturating_sub(bytes);
            }
            for &l in max.iter() {
                match l.kind() {
                    X => self.by_x.entry(l.atom()).or_default().push(id),
                    XL => {
                        let table = table_bytes(self.by_xl.capacity(), 32);
                        let v = self.by_xl.entry(l).or_default();
                        let cap = v.capacity();
                        v.push(id);
                        let list = (v.capacity() - cap) * 4;
                        self.grown +=
                            table_bytes(self.by_xl.capacity(), 32).saturating_sub(table) + list;
                    }
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
            if let Some(l) = self.max_of(id, |l| l.kind() == F)
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

    /// The maximal literals of a processed clause.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the rooted rules of item 603 phase 6 read it")
    )]
    pub fn maxima(&self, i: u32) -> &[Lit] {
        match self.multi.get(&i) {
            Some(m) => m,
            None => self.clauses[i as usize].max.as_slice(),
        }
    }

    /// The first maximal literal of a processed clause that `p` takes. Without a rooted
    /// order a clause has one maximal literal, which [`Stored::max`] holds.
    #[inline]
    pub fn max_of(&self, i: u32, p: impl Fn(Lit) -> bool) -> Option<Lit> {
        match self.clauses[i as usize].max {
            Some(l) if p(l) => Some(l),
            _ if self.multi.is_empty() => None,
            _ => self.multi.get(&i)?.iter().copied().find(|&l| p(l)),
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
        let (ix, [a, b, fresh, e]) = ranked();
        let lits = Lits::default();
        let o = Rooted {
            ix: &ix,
            lits: &lits,
            inv: &inv,
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

    #[test]
    fn a_rooted_clause_keeps_every_incomparable_maximal_literal() {
        use RTerm::*;
        let (ix, [a, b, fresh, e]) = ranked();
        let lits = Lits::default();
        let o = Rooted {
            ix: &ix,
            lits: &lits,
            inv: &inv,
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
        };
        let Made::Lit(g) = lits.lit(XLit::Atom(a, C(0)), inv) else {
            panic!()
        };
        let held = |s: &Store| {
            table_bytes(s.by_xl.capacity(), 32)
                + s.by_xl.values().map(|v| v.capacity() * 4).sum::<usize>()
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
}
