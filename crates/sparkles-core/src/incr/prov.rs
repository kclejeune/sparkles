//! Provenance annotations (spec F13 §5): the `count`, `lineage` and `why` semirings that
//! annotate each row of each operator's state, and the keys that name input facts.
//!
//! **Model.** Every row of a circuit has a provenance polynomial over the input quads, as
//! in Green, Karvounarakis and Tannen: a join multiplies its inputs' polynomials, and a
//! union, a projection, `DISTINCT` and a group add them. Each annotation is the image of
//! that polynomial under a homomorphism, kept with signed integer coefficients so that a
//! deletion subtracts what an insertion added.
//!
//! - `count` maps every quad to 1. It is the number of derivations of a row before
//!   `DISTINCT`, which differs from the Z-set weight once `DISTINCT`, `EXISTS` or a group
//!   has combined derivations.
//! - `lineage` keeps, for each atom, the number of derivations that use it, together
//!   with the number of derivations. A join combines them by inclusion and exclusion
//!   (`L(f) = La(f)·Kb + Ka·Lb(f) − La(f)·Lb(f)`, with `K` the known derivations), which is
//!   bilinear, so its maintenance is exact.
//! - `why` maps each derivation to its support, the set of atoms it uses, and counts the
//!   derivations per support. A join's support is the union of its inputs' supports.
//!
//! An atom is an input quad, named by its [`FactKey`], or a negative condition
//! ([`Absence`]): the operator of a `MINUS`, `NOT EXISTS` or an `OPTIONAL` without match,
//! with the bindings that must have no match (spec F13 §5.2).
//!
//! **Bounded witness sets.** A `why` annotation in an operator's state keeps at most
//! `maxWitnesses` supports with their counts, and counts the other derivations as
//! unknown ([`Ann::absorb`]). The total stays exact, so `count` never needs the supports.
//! The kept supports obey two rules, which every operator keeps.
//!
//! 1. A kept support's count is never more than the number of the row's derivations
//!    with that support. Every reported witness is therefore real.
//! 2. When the unknown count is zero, the kept supports are all the row's supports with
//!    their exact counts, and the row is not truncated.
//!
//! A state passes on its *effective* change, the change of what it keeps: a support
//! that does not fit, or that a smaller one displaces, moves to the unknown count, and
//! the operators downstream see that move. A removal of a support that the state does
//! not keep is taken from the unknown count. Two derivations with the same row and the
//! same support are indistinguishable downstream, so the rules hold through every
//! operator, and a row converges to exact supports once its unknown derivations are
//! removed. A build is exact for a row whose supports fit the bound and whose atoms are
//! known, and otherwise keeps the smallest supports.
//!
//! The reported witnesses ([`Ann::report`]) are the minimal kept supports, smallest
//! first, and `truncated` is set exactly when the unknown count is not zero. Rows that a
//! `recompute` node produces have unknown supports.

use super::persist::{Reader, Writer};
use super::zset::Row;
use crate::error::{Error, Result};
use oxrdf::Term;
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};

/// The default bound of a `why` annotation's witnesses.
pub const DEFAULT_MAX_WITNESSES: usize = 8;
/// The largest bound a circuit accepts.
pub const MAX_WITNESSES_LIMIT: usize = 256;

/// The annotation a circuit keeps besides the Z-set weights (spec F13 §5.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Provenance {
    /// Only the weights.
    #[default]
    None,
    /// The number of derivations.
    Count,
    /// The atoms of all derivations.
    Lineage,
    /// At most `max_witnesses` supports, each a set of atoms that derives the row.
    Why { max_witnesses: usize },
}

impl Provenance {
    /// `why` with the default bound.
    pub fn why() -> Provenance {
        Provenance::Why {
            max_witnesses: DEFAULT_MAX_WITNESSES,
        }
    }

    pub fn is_none(self) -> bool {
        self == Provenance::None
    }

    /// The setting's name, as `view.json` spells it.
    pub fn name(self) -> &'static str {
        match self {
            Provenance::None => "none",
            Provenance::Count => "count",
            Provenance::Lineage => "lineage",
            Provenance::Why { .. } => "why",
        }
    }

    /// The bound of a state's supports (no bound outside `why`).
    pub fn max_witnesses(self) -> usize {
        match self {
            Provenance::Why { max_witnesses } => max_witnesses,
            _ => usize::MAX,
        }
    }

    /// Whether the setting is one a circuit accepts.
    pub fn check(self) -> Result<()> {
        match self {
            Provenance::Why { max_witnesses: 0 } => Err(crate::incr::error(
                "invalid-view",
                "maxWitnesses is at least 1",
            )),
            Provenance::Why { max_witnesses } if max_witnesses > MAX_WITNESSES_LIMIT => {
                Err(crate::incr::error(
                    "invalid-view",
                    format!("maxWitnesses is at most {MAX_WITNESSES_LIMIT}"),
                ))
            }
            _ => Ok(()),
        }
    }
}

/// A 128-bit key of 16 bytes, stored as two words so that atoms stay compact.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Key128([u64; 2]);

impl Key128 {
    pub fn from_u128(v: u128) -> Key128 {
        Key128([(v >> 64) as u64, v as u64])
    }

    pub fn as_u128(self) -> u128 {
        ((self.0[0] as u128) << 64) | self.0[1] as u128
    }

    fn digest(h: Sha256) -> Key128 {
        let d = h.finalize();
        Key128::from_u128(u128::from_be_bytes(d[..16].try_into().expect("16 bytes")))
    }
}

fn framed(h: &mut Sha256, key: Option<&[u8]>) {
    match key {
        None => h.update([0]),
        Some(k) => {
            h.update([1]);
            h.update((k.len() as u64).to_le_bytes());
            h.update(k);
        }
    }
}

/// The key of an input quad: the first 128 bits of SHA-256 over the vocabulary keys of
/// its subject, predicate, object and graph, each framed by its length, with the
/// default graph as no key. Vocabulary keys name the same term in every generation, so
/// a key survives compaction, where ids do not, and 128 bits do not collide at any
/// store's scale. A key is resolved back to its quad through a snapshot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct FactKey(pub Key128);

impl FactKey {
    /// The key of the quad whose terms have vocabulary keys `s`, `p`, `o` and `g`
    /// (`None`: the default graph).
    pub fn of_keys(s: &[u8], p: &[u8], o: &[u8], g: Option<&[u8]>) -> FactKey {
        let mut h = Sha256::new();
        h.update(b"sparkles-fact\0");
        framed(&mut h, Some(s));
        framed(&mut h, Some(p));
        framed(&mut h, Some(o));
        framed(&mut h, g);
        FactKey(Key128::digest(h))
    }

    /// The key of the quad `(s, p, o, g)` (`g` `None`: the default graph).
    pub fn of_terms(s: &Term, p: &Term, o: &Term, g: Option<&Term>) -> FactKey {
        let k = crate::id::term_key;
        let g = g.map(k);
        FactKey::of_keys(&k(s), &k(p), &k(o), g.as_deref())
    }

    pub fn as_u128(self) -> u128 {
        self.0.as_u128()
    }
}

/// A negative condition (spec F13 §5.2): no right row of operator `node` matches the
/// bindings that `bindings` hashes. `node` numbers the circuit's negating operators and
/// implicit groups in preorder ([`super::circuit::Circuit::conditions`] describes
/// them), and `bindings` is the first 128 bits of SHA-256 over the left row's values of
/// the shared variables in the order of their names, each the variable's name and the
/// value's vocabulary key, an unbound value framed as no key. Names rather than
/// positions keep the hash independent of how the circuit orders its columns.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Absence {
    pub node: u32,
    pub bindings: Key128,
}

impl Absence {
    /// The condition of `node` over variables named with values of vocabulary keys
    /// (`None`: unbound), in any order.
    pub fn of_keys(node: u32, bindings: &[(&str, Option<&[u8]>)]) -> Absence {
        let mut b: Vec<&(&str, Option<&[u8]>)> = bindings.iter().collect();
        b.sort_by(|x, y| x.0.cmp(y.0));
        let mut h = Sha256::new();
        h.update(b"sparkles-absence\0");
        h.update((b.len() as u64).to_le_bytes());
        for (name, k) in b {
            framed(&mut h, Some(name.as_bytes()));
            framed(&mut h, *k);
        }
        Absence {
            node,
            bindings: Key128::digest(h),
        }
    }

    /// The condition of `node` over variables named with terms (`None`: unbound).
    pub fn of_terms(node: u32, bindings: &[(&str, Option<Term>)]) -> Absence {
        let keys: Vec<(&str, Option<Vec<u8>>)> = bindings
            .iter()
            .map(|(n, t)| (*n, t.as_ref().map(crate::id::term_key)))
            .collect();
        let refs: Vec<(&str, Option<&[u8]>)> =
            keys.iter().map(|(n, k)| (*n, k.as_deref())).collect();
        Absence::of_keys(node, &refs)
    }
}

/// One element of a witness: an input quad, or a quad pattern that must have no match.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Atom {
    Fact(FactKey),
    Absent(Absence),
}

/// A support: a set of atoms, sorted. Witnesses order by size, then by their atoms, so
/// "the smallest witnesses" is well defined.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct Witness(Box<[Atom]>);

impl Witness {
    pub fn new(mut atoms: Vec<Atom>) -> Witness {
        atoms.sort_unstable();
        atoms.dedup();
        Witness(atoms.into())
    }

    pub fn atoms(&self) -> &[Atom] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The union of two supports.
    pub fn union(&self, o: &Witness) -> Witness {
        if o.0.is_empty() {
            return self.clone();
        }
        if self.0.is_empty() {
            return o.clone();
        }
        let (a, b) = (&self.0, &o.0);
        let mut out = Vec::with_capacity(a.len() + b.len());
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            match a[i].cmp(&b[j]) {
                std::cmp::Ordering::Less => {
                    out.push(a[i]);
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    out.push(b[j]);
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    out.push(a[i]);
                    i += 1;
                    j += 1;
                }
            }
        }
        out.extend_from_slice(&a[i..]);
        out.extend_from_slice(&b[j..]);
        Witness(out.into())
    }

    /// Whether every atom of `self` is in `o`.
    pub fn is_subset(&self, o: &Witness) -> bool {
        let mut j = 0;
        for a in self.0.iter() {
            while j < o.0.len() && o.0[j] < *a {
                j += 1;
            }
            if j == o.0.len() || o.0[j] != *a {
                return false;
            }
            j += 1;
        }
        true
    }

    fn bytes(&self) -> u64 {
        self.0.len() as u64 * std::mem::size_of::<Atom>() as u64 + 16
    }
}

impl PartialOrd for Witness {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Witness {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.0.len().cmp(&o.0.len()).then_with(|| self.0.cmp(&o.0))
    }
}

/// A `lineage` annotation or change: the derivations, those whose atoms are unknown,
/// and per atom the known derivations that use it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lineage {
    pub n: i64,
    pub unknown: i64,
    pub atoms: FxHashMap<Atom, i64>,
}

/// A `why` annotation or change: derivations per support, and those whose support is
/// unknown.
#[derive(Clone, Debug, Default)]
pub struct Why {
    pub unknown: i64,
    pub supports: Vec<(Witness, i64)>,
}

impl Why {
    fn known(&self) -> i64 {
        self.supports.iter().map(|(_, c)| c).sum()
    }

    fn find(&self, x: &Witness) -> Option<usize> {
        self.supports.iter().position(|(w, _)| w == x)
    }

    /// Add `c` derivations with support `x`.
    fn add(&mut self, x: &Witness, c: i64) {
        if c == 0 {
            return;
        }
        match self.find(x) {
            Some(i) => {
                self.supports[i].1 += c;
                if self.supports[i].1 == 0 {
                    self.supports.swap_remove(i);
                }
            }
            None => self.supports.push((x.clone(), c)),
        }
    }

    fn add_all(&mut self, o: &Why) {
        self.unknown += o.unknown;
        if self.supports.len() + o.supports.len() > 32 {
            // a large sum merges through an index instead of a search per support
            let mut m: FxHashMap<Witness, i64> =
                std::mem::take(&mut self.supports).into_iter().collect();
            for (x, c) in &o.supports {
                *m.entry(x.clone()).or_insert(0) += c;
            }
            self.supports = m.into_iter().filter(|(_, c)| *c != 0).collect();
        } else {
            for (x, c) in &o.supports {
                self.add(x, *c);
            }
        }
    }
}

impl PartialEq for Why {
    fn eq(&self, o: &Self) -> bool {
        if self.unknown != o.unknown || self.supports.len() != o.supports.len() {
            return false;
        }
        self.supports.iter().all(|(x, c)| {
            o.supports
                .iter()
                .find(|(y, _)| y == x)
                .is_some_and(|(_, d)| d == c)
        })
    }
}

impl Eq for Why {}

/// An annotation, or the change of one: a value of one of the semirings with signed
/// coefficients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ann {
    Count(i64),
    Lineage(Box<Lineage>),
    Why(Box<Why>),
}

/// The annotations of rows, beside a state's weights.
pub type AnnMap = FxHashMap<Row, Ann>;

/// Annotated changes: each row's weight change and the change of its annotation. A row
/// may occur more than once, and an entry may change the annotation alone.
pub type AChanges = Vec<(Row, i64, Ann)>;

impl Ann {
    /// The annotation of no derivation.
    pub fn zero(m: Provenance) -> Ann {
        match m {
            Provenance::Lineage => Ann::Lineage(Box::default()),
            Provenance::Why { .. } => Ann::Why(Box::default()),
            _ => Ann::Count(0),
        }
    }

    /// One derivation that uses no atom, such as a row of `VALUES`.
    pub fn one(m: Provenance) -> Ann {
        Ann::derivation(m, Vec::new())
    }

    /// One derivation that uses `atoms`.
    pub fn derivation(m: Provenance, atoms: Vec<Atom>) -> Ann {
        match m {
            Provenance::Lineage => {
                let mut l = Lineage {
                    n: 1,
                    ..Default::default()
                };
                for a in atoms {
                    l.atoms.insert(a, 1);
                }
                Ann::Lineage(Box::new(l))
            }
            Provenance::Why { .. } => Ann::Why(Box::new(Why {
                unknown: 0,
                supports: vec![(Witness::new(atoms), 1)],
            })),
            _ => Ann::Count(1),
        }
    }

    /// `n` derivations whose atoms are not known.
    pub fn unknown(m: Provenance, n: i64) -> Ann {
        match m {
            Provenance::Lineage => Ann::Lineage(Box::new(Lineage {
                n,
                unknown: n,
                atoms: FxHashMap::default(),
            })),
            Provenance::Why { .. } => Ann::Why(Box::new(Why {
                unknown: n,
                supports: Vec::new(),
            })),
            _ => Ann::Count(n),
        }
    }

    /// The number of derivations.
    pub fn count(&self) -> i64 {
        match self {
            Ann::Count(n) => *n,
            Ann::Lineage(l) => l.n,
            Ann::Why(w) => w.known() + w.unknown,
        }
    }

    /// The derivations whose atoms are not known.
    pub fn unknown_count(&self) -> i64 {
        match self {
            Ann::Count(_) => 0,
            Ann::Lineage(l) => l.unknown,
            Ann::Why(w) => w.unknown,
        }
    }

    pub fn is_zero(&self) -> bool {
        match self {
            Ann::Count(n) => *n == 0,
            Ann::Lineage(l) => l.n == 0 && l.unknown == 0 && l.atoms.is_empty(),
            Ann::Why(w) => w.unknown == 0 && w.supports.is_empty(),
        }
    }

    /// Multiply every coefficient by `k`.
    pub fn scale(&mut self, k: i64) {
        if k == 1 {
            return;
        }
        match self {
            Ann::Count(n) => *n *= k,
            Ann::Lineage(l) => {
                l.n *= k;
                l.unknown *= k;
                if k == 0 {
                    l.atoms.clear();
                } else {
                    for c in l.atoms.values_mut() {
                        *c *= k;
                    }
                }
            }
            Ann::Why(w) => {
                w.unknown *= k;
                if k == 0 {
                    w.supports.clear();
                } else {
                    for (_, c) in w.supports.iter_mut() {
                        *c *= k;
                    }
                }
            }
        }
    }

    /// `self` with every coefficient multiplied by `k`.
    pub fn scaled(mut self, k: i64) -> Ann {
        self.scale(k);
        self
    }

    /// The sum `self + o`, the annotation of a union.
    pub fn add_assign(&mut self, o: &Ann) {
        match (self, o) {
            (Ann::Count(a), Ann::Count(b)) => *a += b,
            (Ann::Lineage(a), Ann::Lineage(b)) => {
                a.n += b.n;
                a.unknown += b.unknown;
                for (k, c) in &b.atoms {
                    let e = a.atoms.entry(*k).or_insert(0);
                    *e += c;
                    if *e == 0 {
                        a.atoms.remove(k);
                    }
                }
            }
            (Ann::Why(a), Ann::Why(b)) => a.add_all(b),
            (a, b) => debug_assert!(false, "annotations of two semirings: {a:?} {b:?}"),
        }
    }

    /// `self − o`.
    pub fn sub(&self, o: &Ann) -> Ann {
        let mut d = self.clone();
        d.add_assign(&o.clone().scaled(-1));
        d
    }

    /// The product `self · o`, the annotation of a join's merged row.
    pub fn mul(&self, o: &Ann) -> Ann {
        match (self, o) {
            (Ann::Count(a), Ann::Count(b)) => Ann::Count(a * b),
            (Ann::Lineage(a), Ann::Lineage(b)) => {
                let (ka, kb) = (a.n - a.unknown, b.n - b.unknown);
                let mut atoms: FxHashMap<Atom, i64> = FxHashMap::default();
                for (f, la) in &a.atoms {
                    let lb = b.atoms.get(f).copied().unwrap_or(0);
                    *atoms.entry(*f).or_insert(0) += la * kb - la * lb;
                }
                for (f, lb) in &b.atoms {
                    *atoms.entry(*f).or_insert(0) += ka * lb;
                }
                atoms.retain(|_, c| *c != 0);
                Ann::Lineage(Box::new(Lineage {
                    n: a.n * b.n,
                    unknown: a.n * b.n - ka * kb,
                    atoms,
                }))
            }
            (Ann::Why(a), Ann::Why(b)) => {
                let (na, nb) = (a.known() + a.unknown, b.known() + b.unknown);
                let (ka, kb) = (a.known(), b.known());
                let mut out = Why {
                    unknown: na * nb - ka * kb,
                    supports: Vec::with_capacity(a.supports.len() * b.supports.len()),
                };
                if a.supports.len() * b.supports.len() > 32 {
                    let mut m: FxHashMap<Witness, i64> = FxHashMap::default();
                    for (x, cx) in &a.supports {
                        for (y, cy) in &b.supports {
                            *m.entry(x.union(y)).or_insert(0) += cx * cy;
                        }
                    }
                    out.supports = m.into_iter().filter(|(_, c)| *c != 0).collect();
                } else {
                    for (x, cx) in &a.supports {
                        for (y, cy) in &b.supports {
                            out.add(&x.union(y), cx * cy);
                        }
                    }
                }
                Ann::Why(Box::new(out))
            }
            (a, b) => {
                debug_assert!(false, "annotations of two semirings: {a:?} {b:?}");
                a.clone()
            }
        }
    }

    /// Add change `d` to this annotation of a state, and return the state's effective
    /// change. `count` and `lineage` are exact, and their effective change is `d`. A
    /// `why` state keeps at most `max` supports: a support that does not fit, or that a
    /// smaller one displaces, moves to the unknown count, and a removal of a support
    /// the state does not keep is taken from the unknown count (the rules of the module
    /// documentation).
    pub fn absorb(&mut self, d: Ann, max: usize) -> Ann {
        let (Ann::Why(s), Ann::Why(d)) = (&mut *self, &d) else {
            self.add_assign(&d);
            return d;
        };
        let mut e = Why {
            unknown: d.unknown,
            supports: Vec::new(),
        };
        s.unknown += d.unknown;
        // removals first, so that they free room for the additions
        for (x, c) in d.supports.iter().filter(|(_, c)| *c < 0) {
            let want = -c;
            let mut rest = want;
            if let Some(i) = s.find(x) {
                let m = s.supports[i].1.min(want);
                s.supports[i].1 -= m;
                if s.supports[i].1 == 0 {
                    s.supports.swap_remove(i);
                }
                e.add(x, -m);
                rest -= m;
            }
            if rest > 0 {
                s.unknown -= rest;
                e.unknown -= rest;
            }
        }
        let mut adds: Vec<&(Witness, i64)> = d.supports.iter().filter(|(_, c)| *c > 0).collect();
        adds.sort_by(|a, b| a.0.cmp(&b.0));
        for (x, c) in adds {
            if let Some(i) = s.find(x) {
                s.supports[i].1 += c;
                e.add(x, *c);
                continue;
            }
            if s.supports.len() < max {
                s.supports.push((x.clone(), *c));
                e.add(x, *c);
                continue;
            }
            let largest = s
                .supports
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.0.cmp(&b.1.0))
                .map(|(i, _)| i);
            match largest {
                Some(l) if *x < s.supports[l].0 => {
                    let (y, cy) = s.supports.swap_remove(l);
                    s.unknown += cy;
                    e.unknown += cy;
                    e.add(&y, -cy);
                    s.supports.push((x.clone(), *c));
                    e.add(x, *c);
                }
                _ => {
                    s.unknown += c;
                    e.unknown += c;
                }
            }
        }
        debug_assert!(s.unknown >= 0, "a state's unknown count went below zero");
        Ann::Why(Box::new(e))
    }

    /// The annotation as a reader sees it, with at most `max` witnesses.
    pub fn report(&self, max: usize) -> Annotation {
        match self {
            Ann::Count(n) => Annotation {
                count: *n,
                ..Default::default()
            },
            Ann::Lineage(l) => {
                let mut atoms: Vec<Atom> = l
                    .atoms
                    .iter()
                    .filter(|(_, c)| **c > 0)
                    .map(|(a, _)| *a)
                    .collect();
                atoms.sort_unstable();
                Annotation {
                    count: l.n,
                    lineage: atoms,
                    witnesses: Vec::new(),
                    truncated: l.unknown != 0,
                }
            }
            Ann::Why(w) => {
                let mut kept: Vec<&Witness> = w
                    .supports
                    .iter()
                    .filter(|(_, c)| *c > 0)
                    .map(|(x, _)| x)
                    .collect();
                kept.sort();
                let minimal: Vec<Witness> = kept
                    .iter()
                    .filter(|x| !kept.iter().any(|y| y.len() < x.len() && y.is_subset(x)))
                    .take(max)
                    .map(|x| (*x).clone())
                    .collect();
                Annotation {
                    count: w.known() + w.unknown,
                    lineage: Vec::new(),
                    witnesses: minimal,
                    truncated: w.unknown != 0,
                }
            }
        }
    }

    /// The approximate memory of the annotation.
    pub fn bytes(&self) -> u64 {
        match self {
            Ann::Count(_) => 8,
            Ann::Lineage(l) => 48 + l.atoms.len() as u64 * 40,
            Ann::Why(w) => 40 + w.supports.iter().map(|(x, _)| x.bytes() + 8).sum::<u64>(),
        }
    }

    pub fn save(&self, w: &mut Writer) {
        let atom = |w: &mut Writer, a: &Atom| match a {
            Atom::Fact(k) => {
                w.u64(0);
                w.i128(k.as_u128() as i128);
            }
            Atom::Absent(c) => {
                w.u64(c.node as u64 + 1);
                w.i128(c.bindings.as_u128() as i128);
            }
        };
        match self {
            Ann::Count(n) => w.i64(*n),
            Ann::Lineage(l) => {
                w.i64(l.n);
                w.i64(l.unknown);
                w.u64(l.atoms.len() as u64);
                for (a, c) in &l.atoms {
                    atom(w, a);
                    w.i64(*c);
                }
            }
            Ann::Why(x) => {
                w.i64(x.unknown);
                w.u64(x.supports.len() as u64);
                for (s, c) in &x.supports {
                    w.u64(s.len() as u64);
                    for a in s.atoms() {
                        atom(w, a);
                    }
                    w.i64(*c);
                }
            }
        }
    }

    pub fn load(r: &mut Reader, m: Provenance) -> Result<Ann> {
        fn atom(r: &mut Reader) -> Result<Atom> {
            let tag = r.u64()?;
            let k = Key128::from_u128(r.i128()? as u128);
            Ok(match tag {
                0 => Atom::Fact(FactKey(k)),
                n => Atom::Absent(Absence {
                    node: u32::try_from(n - 1).map_err(|_| {
                        Error::Corrupt("a view's state names an impossible condition".into())
                    })?,
                    bindings: k,
                }),
            })
        }
        let bounded = |n: u64| -> Result<usize> {
            if n > 1 << 32 {
                return Err(Error::Corrupt(
                    "a view's state has an impossible annotation".into(),
                ));
            }
            Ok(n as usize)
        };
        Ok(match m {
            Provenance::Lineage => {
                let mut l = Lineage {
                    n: r.i64()?,
                    unknown: r.i64()?,
                    atoms: FxHashMap::default(),
                };
                for _ in 0..bounded(r.u64()?)? {
                    let a = atom(r)?;
                    l.atoms.insert(a, r.i64()?);
                }
                Ann::Lineage(Box::new(l))
            }
            Provenance::Why { .. } => {
                let mut x = Why {
                    unknown: r.i64()?,
                    supports: Vec::new(),
                };
                for _ in 0..bounded(r.u64()?)? {
                    let n = bounded(r.u64()?)?;
                    let atoms = (0..n).map(|_| atom(r)).collect::<Result<Vec<_>>>()?;
                    let c = r.i64()?;
                    x.supports.push((Witness::new(atoms), c));
                }
                Ann::Why(Box::new(x))
            }
            _ => Ann::Count(r.i64()?),
        })
    }
}

/// Write the annotations of a state.
pub fn save_map(w: &mut Writer, m: &AnnMap) -> Result<()> {
    w.u64(m.len() as u64);
    for (r, a) in m {
        w.row(r)?;
        a.save(w);
    }
    Ok(())
}

/// Read what [`save_map`] wrote.
pub fn load_map(r: &mut Reader, mode: Provenance) -> Result<AnnMap> {
    let n = r.u64()?;
    let mut m = AnnMap::default();
    for _ in 0..n {
        let row = r.row()?;
        m.insert(row, Ann::load(r, mode)?);
    }
    Ok(m)
}

/// The approximate memory of a state's annotations.
pub fn map_bytes(m: &AnnMap, width: usize) -> u64 {
    m.values()
        .map(|a| a.bytes() + super::zset::row_bytes(width))
        .sum()
}

/// Add change `d` to row `row`'s annotation in `m`, dropping it when it becomes zero,
/// and return the effective change.
pub fn absorb_row(m: &mut AnnMap, row: &Row, d: Ann, mode: Provenance) -> Ann {
    let max = mode.max_witnesses();
    match m.get_mut(row) {
        Some(a) => {
            let e = a.absorb(d, max);
            if a.is_zero() {
                m.remove(row);
            }
            e
        }
        None => {
            let mut a = Ann::zero(mode);
            let e = a.absorb(d, max);
            if !a.is_zero() {
                m.insert(row.clone(), a);
            }
            e
        }
    }
}

/// A row's annotation in a state, or zero.
pub fn get(m: &AnnMap, row: &[crate::id::Id], mode: Provenance) -> Ann {
    m.get(row).cloned().unwrap_or_else(|| Ann::zero(mode))
}

/// Annotated changes with each row once: weights and annotations summed, and rows whose
/// weight and annotation are both zero dropped. Only for exact annotations or before
/// a state absorbs them, since a sum of `why` changes is unbounded.
pub fn consolidate(c: AChanges) -> AChanges {
    if c.len() <= 1 {
        return c
            .into_iter()
            .filter(|(_, w, a)| *w != 0 || !a.is_zero())
            .collect();
    }
    let mut m: FxHashMap<Row, (i64, Ann)> = FxHashMap::default();
    for (r, w, a) in c {
        match m.entry(r) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let v = e.get_mut();
                v.0 += w;
                v.1.add_assign(&a);
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert((w, a));
            }
        }
    }
    m.into_iter()
        .filter(|(_, (w, a))| *w != 0 || !a.is_zero())
        .map(|(r, (w, a))| (r, w, a))
        .collect()
}

/// A row's annotation as a reader sees it (spec F13 §5.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Annotation {
    /// The number of the row's derivations.
    pub count: i64,
    /// `lineage`: the atoms of the row's derivations, in key order.
    pub lineage: Vec<Atom>,
    /// `why`: the row's minimal witnesses, smallest first, at most `maxWitnesses`.
    pub witnesses: Vec<Witness>,
    /// Whether some derivation's atoms are not known: a support did not fit in the
    /// bound (`why`), or a `recompute` node produced the row. The witnesses are then
    /// real but may not be all, and `lineage` may miss atoms.
    pub truncated: bool,
}

/// The keys of quads met in a circuit's passes, so that a quad's key is hashed once.
/// Ids change at a generation switch, which clears it.
#[derive(Clone, Debug, Default)]
pub struct FactCache {
    map: FxHashMap<[crate::id::Id; 4], FactKey>,
}

impl FactCache {
    /// The most keys the cache holds before it starts again.
    const MAX: usize = 1 << 16;

    pub fn get(&self, q: &[crate::id::Id; 4]) -> Option<FactKey> {
        self.map.get(q).copied()
    }

    pub fn insert(&mut self, q: [crate::id::Id; 4], k: FactKey) {
        if self.map.len() >= Self::MAX {
            self.map.clear();
        }
        self.map.insert(q, k);
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn bytes(&self) -> u64 {
        self.map.len() as u64 * 64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::{RngExt, SeedableRng};

    fn fact(n: u128) -> Atom {
        Atom::Fact(FactKey(Key128::from_u128(n)))
    }

    fn wit(v: &[u128]) -> Witness {
        Witness::new(v.iter().map(|&n| fact(n)).collect())
    }

    #[test]
    fn keys_depend_on_every_term_and_the_graph() {
        let t = |s: &str| Term::NamedNode(oxrdf::NamedNode::new_unchecked(s));
        let (a, b, c) = (t("urn:a"), t("urn:b"), t("urn:c"));
        let k = FactKey::of_terms(&a, &b, &c, None);
        assert_eq!(k, FactKey::of_terms(&a, &b, &c, None));
        assert_ne!(k, FactKey::of_terms(&c, &b, &a, None));
        assert_ne!(k, FactKey::of_terms(&a, &b, &c, Some(&a)));
        // framing keeps keys that concatenate alike apart
        assert_ne!(
            FactKey::of_keys(b"ab", b"c", b"d", None),
            FactKey::of_keys(b"a", b"bc", b"d", None)
        );
        assert_ne!(
            Absence::of_terms(0, &[("x", Some(a.clone()))]),
            Absence::of_terms(1, &[("x", Some(a.clone()))])
        );
        assert_ne!(
            Absence::of_terms(0, &[("x", None)]).bindings,
            Absence::of_terms(0, &[]).bindings
        );
        // the order of the variables does not matter, their names do
        assert_eq!(
            Absence::of_terms(0, &[("x", Some(a.clone())), ("y", None)]),
            Absence::of_terms(0, &[("y", None), ("x", Some(a.clone()))])
        );
        assert_ne!(
            Absence::of_terms(0, &[("x", Some(a.clone()))]),
            Absence::of_terms(0, &[("y", Some(a.clone()))])
        );
    }

    #[test]
    fn witnesses_order_by_size_then_atoms() {
        assert!(wit(&[9]) < wit(&[1, 2]));
        assert!(wit(&[1, 2]) < wit(&[1, 3]));
        assert_eq!(wit(&[1, 3]).union(&wit(&[2, 3])), wit(&[1, 2, 3]));
        assert!(wit(&[1, 3]).is_subset(&wit(&[1, 2, 3])));
        assert!(!wit(&[1, 4]).is_subset(&wit(&[1, 2, 3])));
        assert!(Witness::default().is_subset(&wit(&[1])));
    }

    #[test]
    fn lineage_products_count_the_pairs_that_use_an_atom() {
        let m = Provenance::Lineage;
        // a: two derivations {1} and {1, 2}; b: one derivation {2} and one unknown
        let mut a = Ann::derivation(m, vec![fact(1)]);
        a.add_assign(&Ann::derivation(m, vec![fact(1), fact(2)]));
        let mut b = Ann::derivation(m, vec![fact(2)]);
        b.add_assign(&Ann::unknown(m, 1));
        let Ann::Lineage(p) = a.mul(&b) else {
            unreachable!()
        };
        // four pairs, two of them with b's unknown derivation
        assert_eq!((p.n, p.unknown), (4, 2));
        // the known pairs {1}∪{2} and {1,2}∪{2} use 1 and 2 each
        assert_eq!(p.atoms.get(&fact(1)), Some(&2));
        assert_eq!(p.atoms.get(&fact(2)), Some(&2));
    }

    /// A random sequence of changes absorbed into a bounded state keeps the rules of
    /// the module: kept counts never exceed the exact ones, the total is exact, and an
    /// unknown count of zero means the state is exact. The effective changes summed
    /// give the state.
    #[test]
    fn bounded_states_stay_sound_and_converge() {
        let mode = Provenance::Why { max_witnesses: 3 };
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..300 {
            let mut exact: FxHashMap<Witness, i64> = FxHashMap::default();
            let mut state = Ann::zero(mode);
            let mut effective = Ann::zero(mode);
            for _ in 0..60 {
                let x = wit(&[rng.random_range(0..4u128), rng.random_range(0..6u128)]);
                let have = exact.get(&x).copied().unwrap_or(0);
                let c: i64 = if have > 0 && rng.random_bool(0.45) {
                    -rng.random_range(1..=have)
                } else {
                    rng.random_range(1..3)
                };
                *exact.entry(x.clone()).or_insert(0) += c;
                exact.retain(|_, c| *c != 0);
                let mut d = Why::default();
                d.add(&x, c);
                let e = state.absorb(Ann::Why(Box::new(d)), 3);
                effective.add_assign(&e);
                assert_eq!(effective, state, "the effective changes sum to the state");
                let Ann::Why(s) = &state else { unreachable!() };
                assert!(s.supports.len() <= 3);
                assert!(s.unknown >= 0);
                assert_eq!(state.count(), exact.values().sum::<i64>());
                for (y, k) in &s.supports {
                    assert!(*k > 0 && *k <= exact.get(y).copied().unwrap_or(0));
                }
                if s.unknown == 0 {
                    assert_eq!(s.supports.len(), exact.len());
                }
            }
        }
    }

    #[test]
    fn reports_keep_the_smallest_minimal_witnesses() {
        let mode = Provenance::why();
        let mut a = Ann::zero(mode);
        for w in [wit(&[1, 2]), wit(&[1]), wit(&[3, 4]), wit(&[1, 5])] {
            let mut d = Why::default();
            d.add(&w, 1);
            a.absorb(Ann::Why(Box::new(d)), 8);
        }
        let r = a.report(8);
        assert_eq!(r.witnesses, vec![wit(&[1]), wit(&[3, 4])]);
        assert_eq!(r.count, 4);
        assert!(!r.truncated);
        assert_eq!(a.report(1).witnesses, vec![wit(&[1])]);
    }
}
