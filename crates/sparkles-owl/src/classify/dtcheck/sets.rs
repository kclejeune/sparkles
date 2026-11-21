//! Exact numbers and the two kinds of value sets the checker's normal forms are built
//! from: unions of closed intervals over a discrete order, and piecewise sets over a
//! dense order of rationals.

use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{One, Signed, Zero};
use std::cmp::Ordering;

/// The value a size saturates at, 2^63. A size at this value is at least it.
pub const CAP: u128 = 1 << 63;

/// A rational number in lowest terms with a positive denominator, ordered numerically.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rat {
    n: BigInt,
    d: BigInt,
}

impl Rat {
    pub fn new(n: BigInt, d: BigInt) -> Rat {
        assert!(!d.is_zero(), "zero denominator");
        let g = n.gcd(&d);
        let (mut n, mut d) = (n / &g, d / &g);
        if d.is_negative() {
            n = -n;
            d = -d;
        }
        Rat { n, d }
    }
    pub fn int(n: impl Into<BigInt>) -> Rat {
        Rat {
            n: n.into(),
            d: BigInt::one(),
        }
    }
    pub fn is_int(&self) -> bool {
        self.d.is_one()
    }
    /// Whether the number has a finite decimal expansion, a denominator whose only prime
    /// factors are 2 and 5.
    pub fn is_decimal(&self) -> bool {
        let mut d = self.d.clone();
        for p in [2u32, 5] {
            let p = BigInt::from(p);
            while (&d % &p).is_zero() {
                d /= &p;
            }
        }
        d.is_one()
    }
    pub fn floor(&self) -> BigInt {
        self.n.div_floor(&self.d)
    }
    pub fn ceil(&self) -> BigInt {
        -((-&self.n).div_floor(&self.d))
    }
    pub fn plus(&self, k: i64) -> Rat {
        Rat::new(&self.n + BigInt::from(k) * &self.d, self.d.clone())
    }
    pub fn add(&self, o: &Rat) -> Rat {
        Rat::new(&self.n * &o.d + &o.n * &self.d, &self.d * &o.d)
    }
    /// The byte size of the number's limbs, for memory accounting.
    pub fn bytes(&self) -> usize {
        (self.n.bits() as usize + self.d.bits() as usize).div_ceil(8) + 2 * size_of::<BigInt>()
    }
}

impl Ord for Rat {
    fn cmp(&self, o: &Rat) -> Ordering {
        (&self.n * &o.d).cmp(&(&o.n * &self.d))
    }
}
impl PartialOrd for Rat {
    fn partial_cmp(&self, o: &Rat) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// The count of a set, with a lower bound and an upper bound, both saturating: `lo` at
/// [`CAP`], and `hi` as `None` when it is infinite or not known. `inf` says the set is
/// infinite, which only an unbounded source sets: a count that saturates is a lower bound
/// that removing members still lowers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub lo: u128,
    pub hi: Option<u128>,
    pub inf: bool,
}

impl Size {
    pub const ZERO: Size = Size {
        lo: 0,
        hi: Some(0),
        inf: false,
    };
    pub const INFINITE: Size = Size {
        lo: CAP,
        hi: None,
        inf: true,
    };
    /// A count the checker cannot bound.
    pub const UNKNOWN: Size = Size {
        lo: 0,
        hi: None,
        inf: false,
    };
    pub fn exact(n: u128) -> Size {
        if n >= CAP {
            Size {
                lo: CAP,
                hi: None,
                inf: false,
            }
        } else {
            Size {
                lo: n,
                hi: Some(n),
                inf: false,
            }
        }
    }
    pub fn add(self, o: Size) -> Size {
        let lo = (self.lo + o.lo).min(CAP);
        let hi = match (self.hi, o.hi) {
            (Some(a), Some(b)) if a + b < CAP => Some(a + b),
            _ => None,
        };
        Size {
            lo,
            hi,
            inf: self.inf || o.inf,
        }
    }
    /// The size after removing `k` members of the set.
    pub fn minus(self, k: u128) -> Size {
        Size {
            lo: if self.inf {
                CAP
            } else {
                self.lo.saturating_sub(k)
            },
            hi: self.hi.map(|h| h - k.min(h)),
            inf: self.inf,
        }
    }
    /// Whether the count is known: exactly, or as at least [`CAP`], which no demand of a
    /// problem reaches.
    pub fn is_exact(self) -> bool {
        self.hi == Some(self.lo) || self.lo >= CAP
    }
}

/// A point of a discrete order: its successor and predecessor, and the number of points
/// in a closed interval, saturating at [`CAP`].
pub trait Step: Clone + Ord + std::hash::Hash + std::fmt::Debug {
    fn succ(&self) -> Self;
    fn pred(&self) -> Self;
    fn span(lo: &Self, hi: &Self) -> u128;
    /// The heap bytes of a point.
    fn heap(&self) -> usize;
}

impl Step for BigInt {
    fn succ(&self) -> Self {
        self + 1
    }
    fn pred(&self) -> Self {
        self - 1
    }
    fn span(lo: &Self, hi: &Self) -> u128 {
        let n: BigInt = hi - lo + 1;
        u128::try_from(n).map_or(CAP, |n| n.min(CAP))
    }
    fn heap(&self) -> usize {
        (self.bits() as usize).div_ceil(8)
    }
}

impl Step for u64 {
    fn succ(&self) -> Self {
        self + 1
    }
    fn pred(&self) -> Self {
        self - 1
    }
    fn span(lo: &Self, hi: &Self) -> u128 {
        (u128::from(*hi - *lo) + 1).min(CAP)
    }
    fn heap(&self) -> usize {
        0
    }
}

/// An end of a discrete interval: a point, or unbounded on that side.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum End<T> {
    Min,
    At(T),
    Max,
}

impl<T: Step> End<T> {
    fn succ(&self) -> End<T> {
        match self {
            End::At(x) => End::At(x.succ()),
            e => e.clone(),
        }
    }
    fn pred(&self) -> End<T> {
        match self {
            End::At(x) => End::At(x.pred()),
            e => e.clone(),
        }
    }
}

/// A union of disjoint, nonadjacent closed intervals in increasing order, within a
/// domain `[lo, hi]` that the caller supplies to complement.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Disc<T> {
    pub iv: Vec<(End<T>, End<T>)>,
}

impl<T: Step> Disc<T> {
    pub fn empty() -> Disc<T> {
        Disc { iv: Vec::new() }
    }
    pub fn range(lo: End<T>, hi: End<T>) -> Disc<T> {
        if lo <= hi && lo != End::Max && hi != End::Min {
            Disc { iv: vec![(lo, hi)] }
        } else {
            Disc::empty()
        }
    }
    pub fn point(x: T) -> Disc<T> {
        Disc::range(End::At(x.clone()), End::At(x))
    }
    pub fn is_empty(&self) -> bool {
        self.iv.is_empty()
    }
    pub fn contains(&self, x: &T) -> bool {
        let x = End::At(x.clone());
        self.iv.iter().any(|(l, h)| *l <= x && x <= *h)
    }
    pub fn meet(&self, o: &Disc<T>) -> Disc<T> {
        let (mut i, mut j, mut out) = (0, 0, Vec::new());
        while i < self.iv.len() && j < o.iv.len() {
            let (a, b) = (&self.iv[i], &o.iv[j]);
            let lo = a.0.clone().max(b.0.clone());
            let hi = a.1.clone().min(b.1.clone());
            if lo <= hi {
                out.push((lo, hi));
            }
            if a.1 < b.1 {
                i += 1;
            } else {
                j += 1;
            }
        }
        Disc { iv: out }
    }
    pub fn join(&self, o: &Disc<T>) -> Disc<T> {
        let mut all: Vec<_> = self.iv.iter().chain(&o.iv).cloned().collect();
        all.sort();
        let mut out: Vec<(End<T>, End<T>)> = Vec::new();
        for (l, h) in all {
            if let Some(last) = out.last_mut()
                && l <= last.1.succ()
            {
                if h > last.1 {
                    last.1 = h;
                }
                continue;
            }
            out.push((l, h));
        }
        Disc { iv: out }
    }
    /// The complement within the domain `[dlo, dhi]`, which holds every interval.
    pub fn not(&self, dlo: &End<T>, dhi: &End<T>) -> Disc<T> {
        let mut out = Vec::new();
        let mut cur = Some(dlo.clone());
        for (l, h) in &self.iv {
            if let Some(c) = &cur
                && c < l
            {
                out.push((c.clone(), l.pred()));
            }
            cur = (h < dhi).then(|| h.succ());
        }
        if let Some(c) = cur
            && c <= *dhi
        {
            out.push((c, dhi.clone()));
        }
        Disc { iv: out }
    }
    pub fn bytes(&self) -> usize {
        let heap = |e: &End<T>| match e {
            End::At(x) => x.heap(),
            _ => 0,
        };
        self.iv.capacity() * size_of::<(End<T>, End<T>)>()
            + self
                .iv
                .iter()
                .map(|(l, h)| heap(l) + heap(h))
                .sum::<usize>()
    }
    pub fn size(&self) -> Size {
        let mut s = Size::ZERO;
        for (l, h) in &self.iv {
            s = s.add(match (l, h) {
                (End::At(a), End::At(b)) => Size::exact(T::span(a, b)),
                _ => Size::INFINITE,
            });
        }
        s
    }
}

/// A subset of a dense stratum of the rationals, such as the decimals that are not
/// integers, as a function on pieces: the breakpoints `pts` in increasing order, whether
/// each breakpoint is in the set, and whether each open gap between them, and the two
/// unbounded ones, is. A breakpoint outside the stratum is never in the set, and no
/// breakpoint has the same value as both gaps beside it, which makes the form unique.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Dense {
    pts: Vec<Rat>,
    at: Vec<bool>,
    gap: Vec<bool>,
}

impl Dense {
    pub fn empty() -> Dense {
        Dense {
            pts: Vec::new(),
            at: Vec::new(),
            gap: vec![false],
        }
    }
    pub fn full() -> Dense {
        Dense {
            pts: Vec::new(),
            at: Vec::new(),
            gap: vec![true],
        }
    }
    /// The interval between `lo` and `hi`, each with whether it is closed; `None` is
    /// unbounded.
    pub fn interval(
        lo: Option<(Rat, bool)>,
        hi: Option<(Rat, bool)>,
        member: fn(&Rat) -> bool,
    ) -> Dense {
        let mut d = Dense::empty();
        match (lo, hi) {
            (None, None) => return Dense::full(),
            (Some((a, ca)), None) => {
                d.pts = vec![a];
                d.at = vec![ca];
                d.gap = vec![false, true];
            }
            (None, Some((b, cb))) => {
                d.pts = vec![b];
                d.at = vec![cb];
                d.gap = vec![true, false];
            }
            (Some((a, ca)), Some((b, cb))) => match a.cmp(&b) {
                Ordering::Greater => return Dense::empty(),
                Ordering::Equal => {
                    d.pts = vec![a];
                    d.at = vec![ca && cb];
                    d.gap = vec![false, false];
                }
                Ordering::Less => {
                    d.pts = vec![a, b];
                    d.at = vec![ca, cb];
                    d.gap = vec![false, true, false];
                }
            },
        }
        d.canon(member);
        d
    }
    pub fn point(q: Rat, member: fn(&Rat) -> bool) -> Dense {
        Dense::interval(Some((q.clone(), true)), Some((q, true)), member)
    }
    pub fn contains(&self, q: &Rat) -> bool {
        match self.pts.binary_search(q) {
            Ok(i) => self.at[i],
            Err(i) => self.gap[i],
        }
    }
    /// Whether the open gap that starts at `q`, or the first gap for `None`, is in the
    /// set. Every breakpoint of `self` is among those of the gap's partition.
    fn gap_after(&self, q: Option<&Rat>) -> bool {
        match q {
            None => self.gap[0],
            Some(q) => self.gap[self.pts.partition_point(|p| p <= q)],
        }
    }
    fn canon(&mut self, member: fn(&Rat) -> bool) {
        let mut pts = Vec::with_capacity(self.pts.len());
        let mut at = Vec::with_capacity(self.pts.len());
        let mut gap = vec![self.gap[0]];
        for (i, q) in self.pts.iter().enumerate() {
            let a = self.at[i] && member(q);
            let after = self.gap[i + 1];
            let before = *gap.last().expect("a gap precedes every point");
            // a point outside the stratum is no value, so it separates nothing
            if before == after && (a == before || !member(q)) {
                continue;
            }
            pts.push(q.clone());
            at.push(a);
            gap.push(after);
        }
        self.pts = pts;
        self.at = at;
        self.gap = gap;
    }
    pub fn op(&self, o: &Dense, f: impl Fn(bool, bool) -> bool, member: fn(&Rat) -> bool) -> Dense {
        let mut pts: Vec<Rat> = self.pts.iter().chain(&o.pts).cloned().collect();
        pts.sort();
        pts.dedup();
        let at = pts
            .iter()
            .map(|q| f(self.contains(q), o.contains(q)))
            .collect();
        let mut gap = vec![f(self.gap_after(None), o.gap_after(None))];
        for q in &pts {
            gap.push(f(self.gap_after(Some(q)), o.gap_after(Some(q))));
        }
        let mut d = Dense { pts, at, gap };
        d.canon(member);
        d
    }
    pub fn not(&self, member: fn(&Rat) -> bool) -> Dense {
        let mut d = Dense {
            pts: self.pts.clone(),
            at: self.at.iter().map(|a| !a).collect(),
            gap: self.gap.iter().map(|g| !g).collect(),
        };
        d.canon(member);
        d
    }
    /// The size when each point stands for `per_point` values: infinite when a gap is
    /// in the set, since every stratum is dense.
    pub fn size(&self, per_point: u128) -> Size {
        if self.gap.iter().any(|g| *g) {
            return Size::INFINITE;
        }
        Size::exact(self.at.iter().filter(|a| **a).count() as u128 * per_point)
    }
    pub fn bytes(&self) -> usize {
        self.pts.iter().map(Rat::bytes).sum::<usize>() + 2 * self.pts.len() + 1
    }
}
