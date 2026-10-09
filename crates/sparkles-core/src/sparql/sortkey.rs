//! ORDER BY sort keys and the bounded top-k heap.
//!
//! A [`SortKey`] is one ORDER BY key of one row, classified once so that comparing two
//! keys is cheap. It reproduces [`order_cmp`] for every pair of values, not just on
//! average: two keys of different classes compare by class, which is the order that
//! `order_cmp` gives values of different kinds and literal families, and two keys of the
//! same class compare by a precomputed primitive (an exact decimal, a double, a string or
//! a boolean) whenever that primitive alone decides the result. Every other pair is
//! handed to `order_cmp` on the original values. That covers equal numbers of different
//! types, NaN and signed zeros, dates, times and durations (whose order is partial),
//! composite literals, triple terms and other literals. A sort that uses these keys
//! therefore makes the same decisions as one that uses `order_cmp`, and it produces the
//! same output with the same algorithm, whether or not the values are totally ordered.
//!
//! [`TopK`] keeps the best `k` entries of a sequence under the composite order of several
//! keys, ties broken by arrival. Eager and cursor execution feed it the same rows in the
//! same order and therefore end in the same state. When the values are totally ordered
//! that state is the first `k` rows of the full sort. When they are not, for example with
//! dates with and without a timezone close to each other, no sort is defined by the data,
//! and the heap still gives every execution mode and batch size the same answer.

use super::value::{Value, order_cmp};
use std::cmp::Ordering;

const NULL: u8 = 0;
const BNODE: u8 = 1;
const IRI: u8 = 2;
/// Literals take the classes `LITERAL + rank`, in the order of `order_cmp`'s fallback.
const LITERAL: u8 = 3;
const TRIPLE: u8 = 14;

/// What decides the order of two keys of the same class without the values.
#[derive(Clone, Copy, Debug)]
enum Prim {
    /// Only `order_cmp` decides (and nothing to decide for the unbound class).
    Plain,
    /// The string of an IRI, a blank node or an `xsd:string` decides completely.
    Text,
    /// A language-tagged string orders by its lexical form, then by its tag as written.
    Lang,
    /// An `xsd:integer` or `xsd:decimal` in units of 10⁻¹⁸ (exact), and whether it is a
    /// decimal. Different values decide; equal values of one type are the same value.
    Exact(i128, bool),
    /// An `xsd:double` or `xsd:float` widened to `f64`, and whether it is a float.
    /// Different values decide. Equal nonzero values of one type are the same value.
    Approx(f64, bool),
    Bool(bool),
}

/// One ORDER BY key of one row (see the module documentation).
#[derive(Clone, Debug)]
pub struct SortKey {
    class: u8,
    prim: Prim,
    value: Option<Value>,
}

impl SortKey {
    /// The key of an unbound variable or an expression error.
    pub const NULL: SortKey = SortKey {
        class: NULL,
        prim: Prim::Plain,
        value: None,
    };

    pub fn new(value: Option<Value>) -> SortKey {
        let Some(v) = value else {
            return SortKey::NULL;
        };
        let (class, prim) = match &v {
            Value::BNode(_) => (BNODE, Prim::Text),
            Value::Iri(_) => (IRI, Prim::Text),
            Value::Triple(_) => (TRIPLE, Prim::Plain),
            Value::Str(_) => (LITERAL, Prim::Text),
            Value::Lang(..) => (LITERAL + 1, Prim::Lang),
            Value::Integer(i) => (
                LITERAL + 2,
                Prim::Exact(i128::from(i64::from(*i)) * 1_000_000_000_000_000_000, false),
            ),
            Value::Decimal(d) => (
                LITERAL + 2,
                Prim::Exact(i128::from_be_bytes(d.to_be_bytes()), true),
            ),
            Value::Float(f) => (LITERAL + 2, Prim::Approx(f64::from(f32::from(*f)), true)),
            Value::Double(d) => (LITERAL + 2, Prim::Approx(f64::from(*d), false)),
            Value::Bool(b) => (LITERAL + 3, Prim::Bool(*b)),
            Value::DateTime(_) => (LITERAL + 4, Prim::Plain),
            Value::Date(_) => (LITERAL + 5, Prim::Plain),
            Value::Time(_) => (LITERAL + 6, Prim::Plain),
            Value::Duration(_) | Value::YearMonth(_) | Value::DayTime(_) => {
                (LITERAL + 7, Prim::Plain)
            }
            Value::Other { dt, .. } if &**dt == super::cdt::LIST => (LITERAL + 8, Prim::Plain),
            Value::Other { dt, .. } if &**dt == super::cdt::MAP => (LITERAL + 9, Prim::Plain),
            Value::LangDir(..) | Value::Other { .. } => (LITERAL + 10, Prim::Plain),
        };
        SortKey {
            class,
            prim,
            value: Some(v),
        }
    }

    /// Bytes this key keeps beyond its own size, for memory accounting.
    pub fn payload_bytes(&self) -> u64 {
        match &self.value {
            Some(Value::Iri(s) | Value::BNode(s) | Value::Str(s)) => s.len() as u64 + 64,
            Some(Value::Lang(s, lang) | Value::LangDir(s, lang, _)) => {
                (s.len() + lang.len()) as u64 + 128
            }
            Some(Value::Other { lex, dt }) => (lex.len() + dt.len()) as u64 + 128,
            Some(Value::Triple(triple)) => super::graph_triple_bytes(triple),
            _ => 0,
        }
    }

    /// The order of [`order_cmp`] on the two values (ascending).
    pub fn cmp(&self, other: &SortKey) -> Ordering {
        if self.class != other.class {
            return self.class.cmp(&other.class);
        }
        let decided = match (self.prim, other.prim) {
            _ if self.class == NULL => Some(Ordering::Equal),
            (Prim::Text, Prim::Text) => Some(text(self).cmp(text(other))),
            (Prim::Lang, Prim::Lang) => match (&self.value, &other.value) {
                (Some(Value::Lang(x, lx)), Some(Value::Lang(y, ly))) => {
                    Some(x.cmp(y).then_with(|| lx.cmp(ly)))
                }
                _ => None,
            },
            (Prim::Exact(x, dx), Prim::Exact(y, dy)) => (x != y || dx == dy).then(|| x.cmp(&y)),
            (Prim::Approx(x, fx), Prim::Approx(y, fy)) => {
                if x < y {
                    Some(Ordering::Less)
                } else if x > y {
                    Some(Ordering::Greater)
                } else {
                    (x == y && x != 0.0 && fx == fy).then_some(Ordering::Equal)
                }
            }
            (Prim::Bool(x), Prim::Bool(y)) => Some(x.cmp(&y)),
            _ => None,
        };
        decided.unwrap_or_else(|| order_cmp(self.value.as_ref(), other.value.as_ref()))
    }
}

fn text(k: &SortKey) -> &str {
    match &k.value {
        Some(Value::Iri(s) | Value::BNode(s) | Value::Str(s)) => s,
        _ => "",
    }
}

/// The composite order of several keys, each ascending or descending.
#[inline]
pub fn cmp_keys(a: &[SortKey], b: &[SortKey], asc: &[bool]) -> Ordering {
    for ((x, y), up) in a.iter().zip(b).zip(asc) {
        let o = x.cmp(y);
        if o != Ordering::Equal {
            return if *up { o } else { o.reverse() };
        }
    }
    Ordering::Equal
}

/// The order of the first key alone, ascending or descending.
#[inline]
fn cmp_first(a: &SortKey, b: &SortKey, asc: bool) -> Ordering {
    let o = a.cmp(b);
    if asc { o } else { o.reverse() }
}

/// The positions `0..n` sorted by `cmp`, ties by position, with the parallel merge sort
/// that the full ORDER BY uses. Eager and cursor execution both sort through here, so
/// they make the same comparisons in the same order.
pub fn sort_positions(n: usize, cmp: impl Fn(usize, usize) -> Ordering + Sync) -> Vec<usize> {
    use rayon::slice::ParallelSliceMut;
    let mut idx: Vec<usize> = (0..n).collect();
    idx.par_sort_by(|&a, &b| cmp(a, b).then(a.cmp(&b)));
    idx
}

/// One row kept by a [`TopK`]: its keys, its position in the input, and what the caller
/// keeps for it.
pub struct Entry<P> {
    pub keys: Vec<SortKey>,
    pub seq: u64,
    pub payload: P,
}

/// What [`TopK::screen`] says about a row from its first key alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// The row is worse than every kept row, so it cannot be among the first k.
    Reject,
    /// The row may be kept. Its other keys decide.
    Evaluate,
}

/// The best `k` rows of a sequence under the order of several keys, ties broken by
/// arrival (see the module documentation). The worst kept row is at the root of a
/// binary heap, so a row that cannot enter costs one comparison.
pub struct TopK<P> {
    k: usize,
    asc: Vec<bool>,
    heap: Vec<Entry<P>>,
    seen: u64,
}

impl<P> TopK<P> {
    pub fn new(k: usize, asc: Vec<bool>) -> Self {
        Self {
            k,
            asc,
            heap: Vec::new(),
            seen: 0,
        }
    }

    /// The number of rows kept now.
    pub fn kept(&self) -> usize {
        self.heap.len()
    }

    /// The number of rows kept at most.
    pub fn k(&self) -> usize {
        self.k
    }

    /// Whether the heap holds `k` rows, so that a new row must beat the worst of them.
    pub fn is_full(&self) -> bool {
        self.heap.len() >= self.k
    }

    fn worse(&self, a: &Entry<P>, b: &Entry<P>) -> bool {
        match cmp_keys(&a.keys, &b.keys, &self.asc) {
            Ordering::Equal => a.seq > b.seq,
            o => o == Ordering::Greater,
        }
    }

    /// Whether the next row, with this first key, can enter. A row whose first key is
    /// worse than the worst kept row's has at least k rows ahead of it, so its other keys
    /// need not be evaluated.
    pub fn screen(&self, first: &SortKey) -> Screen {
        if self.k == 0 {
            return Screen::Reject;
        }
        if !self.is_full() {
            return Screen::Evaluate;
        }
        let worst = &self.heap[0];
        match cmp_first(first, &worst.keys[0], self.asc[0]) {
            Ordering::Greater => Screen::Reject,
            _ => Screen::Evaluate,
        }
    }

    /// Count a row that [`TopK::screen`] rejected.
    pub fn skip(&mut self) {
        self.seen += 1;
    }

    /// Offer the next row with all its keys. Returns the entry that left the heap: the
    /// evicted worst row, or the offered row itself when it does not enter.
    pub fn offer(&mut self, keys: Vec<SortKey>, payload: P) -> Option<Entry<P>> {
        let entry = Entry {
            keys,
            seq: self.seen,
            payload,
        };
        self.seen += 1;
        if self.k == 0 {
            return Some(entry);
        }
        if !self.is_full() {
            self.heap.push(entry);
            self.sift_up(self.heap.len() - 1);
            return None;
        }
        if !self.worse(&self.heap[0], &entry) {
            return Some(entry);
        }
        let out = std::mem::replace(&mut self.heap[0], entry);
        self.sift_down(0);
        Some(out)
    }

    fn sift_up(&mut self, mut i: usize) {
        while i > 0 {
            let parent = (i - 1) / 2;
            if !self.worse(&self.heap[i], &self.heap[parent]) {
                break;
            }
            self.heap.swap(i, parent);
            i = parent;
        }
    }

    fn sift_down(&mut self, mut i: usize) {
        let n = self.heap.len();
        loop {
            let (l, r) = (2 * i + 1, 2 * i + 2);
            let mut top = i;
            if l < n && self.worse(&self.heap[l], &self.heap[top]) {
                top = l;
            }
            if r < n && self.worse(&self.heap[r], &self.heap[top]) {
                top = r;
            }
            if top == i {
                break;
            }
            self.heap.swap(i, top);
            i = top;
        }
    }

    /// The kept rows, best first. When at most `k` rows were offered, every row is kept,
    /// and they are sorted as the full sort sorts them: by the keys, ties by arrival,
    /// with the same parallel merge sort over the rows in input order. Otherwise the heap
    /// is emptied worst first, which needs no total order to terminate or stay
    /// deterministic.
    pub fn into_sorted(mut self) -> Vec<Entry<P>>
    where
        P: Send,
    {
        if self.seen <= self.k as u64 {
            let mut entries = self.heap;
            entries.sort_unstable_by_key(|e| e.seq);
            let keys: Vec<&[SortKey]> = entries.iter().map(|e| e.keys.as_slice()).collect();
            let order = sort_positions(keys.len(), |a, b| cmp_keys(keys[a], keys[b], &self.asc));
            let mut slots: Vec<Option<Entry<P>>> = entries.into_iter().map(Some).collect();
            return order
                .into_iter()
                .map(|i| slots[i].take().expect("each position once"))
                .collect();
        }
        let mut out = Vec::with_capacity(self.heap.len());
        while !self.heap.is_empty() {
            let last = self.heap.len() - 1;
            self.heap.swap(0, last);
            out.push(self.heap.pop().expect("a kept row"));
            self.sift_down(0);
        }
        out.reverse();
        out
    }
}

#[cfg(test)]
#[path = "sortkey_tests.rs"]
mod tests;
