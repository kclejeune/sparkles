//! In-memory id-level triple set with hash indexes, append-only so that semi-naive
//! evaluation can restrict any access path to a row range (`[lo, hi)`): every index
//! list holds triple positions in ascending order.

use rustc_hash::FxHashMap;

pub(crate) type Triple = [u64; 3];

#[derive(Default)]
pub(crate) struct Graph {
    pub triples: Vec<Triple>,
    index: FxHashMap<Triple, u32>,
    by_p: FxHashMap<u64, Vec<u32>>,
    by_ps: FxHashMap<(u64, u64), Vec<u32>>,
    by_po: FxHashMap<(u64, u64), Vec<u32>>,
    /// per predicate: distinct subjects / distinct objects
    distinct_s: FxHashMap<u64, u32>,
    distinct_o: FxHashMap<u64, u32>,
    by_s: Option<FxHashMap<u64, Vec<u32>>>,
    by_o: Option<FxHashMap<u64, Vec<u32>>>,
}

/// Candidate triple positions for one access.
#[derive(Clone, Copy)]
pub(crate) enum Cands<'a> {
    Slice(&'a [u32]),
    Range(u32, u32),
}

impl<'a> Cands<'a> {
    pub const EMPTY: Cands<'static> = Cands::Slice(&[]);

    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Cands::Slice(s) => s.len(),
            Cands::Range(a, b) => (b - a) as usize,
        }
    }
    #[inline]
    pub fn get(&self, i: usize) -> u32 {
        match self {
            Cands::Slice(s) => s[i],
            Cands::Range(a, _) => a + i as u32,
        }
    }
    pub fn chunks(self, n: usize) -> Vec<Cands<'a>> {
        let n = n.max(1);
        match self {
            Cands::Slice(s) => s.chunks(n).map(Cands::Slice).collect(),
            Cands::Range(a, b) => (a..b)
                .step_by(n)
                .map(|x| Cands::Range(x, (x + n as u32).min(b)))
                .collect(),
        }
    }
}

#[inline]
fn restrict(list: &[u32], lo: u32, hi: u32) -> &[u32] {
    let a = if lo == 0 { 0 } else { list.partition_point(|&x| x < lo) };
    let b = list.partition_point(|&x| x < hi);
    &list[a..b.max(a)]
}

impl Graph {
    pub fn with_capacity(n: usize) -> Graph {
        Graph {
            triples: Vec::with_capacity(n),
            index: FxHashMap::with_capacity_and_hasher(n, Default::default()),
            ..Default::default()
        }
    }

    #[inline]
    pub fn len(&self) -> u32 {
        self.triples.len() as u32
    }

    /// Add a triple; returns false if it was already present.
    #[cfg(test)]
    pub fn add(&mut self, t: Triple) -> bool {
        self.add_batch(std::iter::once(t)) == 1
    }

    /// Add triples (duplicates are skipped); returns the number of new triples. The
    /// secondary indexes are updated in parallel.
    pub fn add_batch(&mut self, ts: impl IntoIterator<Item = Triple>) -> usize {
        let start = self.triples.len();
        for t in ts {
            let i = self.triples.len() as u32;
            if let std::collections::hash_map::Entry::Vacant(v) = self.index.entry(t) {
                v.insert(i);
                self.triples.push(t);
            }
        }
        let new = &self.triples[start..];
        if new.is_empty() {
            return 0;
        }
        let base = start as u32;
        let rows = || new.iter().enumerate().map(|(k, t)| (base + k as u32, t));
        let Graph { by_p, by_ps, by_po, distinct_s, distinct_o, by_s, by_o, .. } = self;
        rayon::scope(|sc| {
            sc.spawn(|_| {
                for (i, t) in rows() {
                    by_p.entry(t[1]).or_default().push(i);
                }
            });
            sc.spawn(|_| {
                for (i, t) in rows() {
                    let e = by_ps.entry((t[1], t[0])).or_default();
                    if e.is_empty() {
                        *distinct_s.entry(t[1]).or_default() += 1;
                    }
                    e.push(i);
                }
            });
            sc.spawn(|_| {
                for (i, t) in rows() {
                    let e = by_po.entry((t[1], t[2])).or_default();
                    if e.is_empty() {
                        *distinct_o.entry(t[1]).or_default() += 1;
                    }
                    e.push(i);
                }
            });
            if let Some(m) = by_s {
                sc.spawn(|_| {
                    for (i, t) in rows() {
                        m.entry(t[0]).or_default().push(i);
                    }
                });
            }
            if let Some(m) = by_o {
                sc.spawn(|_| {
                    for (i, t) in rows() {
                        m.entry(t[2]).or_default().push(i);
                    }
                });
            }
        });
        new.len()
    }

    pub fn ensure_s_index(&mut self) {
        if self.by_s.is_none() {
            let mut m: FxHashMap<u64, Vec<u32>> = FxHashMap::default();
            for (i, t) in self.triples.iter().enumerate() {
                m.entry(t[0]).or_default().push(i as u32);
            }
            self.by_s = Some(m);
        }
    }

    pub fn ensure_o_index(&mut self) {
        if self.by_o.is_none() {
            let mut m: FxHashMap<u64, Vec<u32>> = FxHashMap::default();
            for (i, t) in self.triples.iter().enumerate() {
                m.entry(t[2]).or_default().push(i as u32);
            }
            self.by_o = Some(m);
        }
    }

    #[inline]
    pub fn position(&self, t: &Triple) -> Option<u32> {
        self.index.get(t).copied()
    }

    /// Candidates for a pattern with the given bound positions, restricted to rows
    /// `[lo, hi)`. The result may be a superset (callers re-check every component).
    pub fn cands(&self, s: Option<u64>, p: Option<u64>, o: Option<u64>, lo: u32, hi: u32) -> Cands<'_> {
        if lo >= hi {
            return Cands::EMPTY;
        }
        fn sl(v: Option<&Vec<u32>>, lo: u32, hi: u32) -> Cands<'_> {
            v.map_or(Cands::EMPTY, |l| Cands::Slice(restrict(l, lo, hi)))
        }
        let slice = |v| sl(v, lo, hi);
        match (s, p, o) {
            (Some(s), Some(p), Some(o)) => match self.index.get(&[s, p, o]) {
                Some(i) if *i >= lo && *i < hi => Cands::Slice(std::slice::from_ref(i)),
                _ => Cands::EMPTY,
            },
            (Some(s), Some(p), None) => slice(self.by_ps.get(&(p, s))),
            (None, Some(p), Some(o)) => slice(self.by_po.get(&(p, o))),
            (None, Some(p), None) => slice(self.by_p.get(&p)),
            (Some(s), None, o) => match (&self.by_s, o, &self.by_o) {
                (Some(m), _, _) => slice(m.get(&s)),
                (None, Some(o), Some(m)) => slice(m.get(&o)),
                _ => Cands::Range(lo, hi),
            },
            (None, None, Some(o)) => match &self.by_o {
                Some(m) => slice(m.get(&o)),
                None => Cands::Range(lo, hi),
            },
            (None, None, None) => Cands::Range(lo, hi),
        }
    }

    /// Number of triples with predicate `p` in `[lo, hi)`.
    pub fn count_p(&self, p: u64, lo: u32, hi: u32) -> usize {
        self.by_p.get(&p).map_or(0, |l| restrict(l, lo, hi).len())
    }

    /// (distinct subjects, distinct objects) of predicate `p`
    pub fn pstats(&self, p: u64) -> (u32, u32) {
        (
            self.distinct_s.get(&p).copied().unwrap_or(1),
            self.distinct_o.get(&p).copied().unwrap_or(1),
        )
    }

    pub fn distinct_predicates(&self) -> usize {
        self.by_p.len().max(1)
    }

    pub fn distinct_subjects(&self) -> usize {
        self.by_s.as_ref().map_or(self.triples.len() / 4, |m| m.len()).max(1)
    }

    pub fn distinct_objects(&self) -> usize {
        self.by_o.as_ref().map_or(self.triples.len() / 4, |m| m.len()).max(1)
    }

    /// The objects of `(s p ?)` among rows `< hi`.
    pub fn objects(&self, s: u64, p: u64, hi: u32) -> impl Iterator<Item = u64> + '_ {
        let c = self.cands(Some(s), Some(p), None, 0, hi);
        (0..c.len()).map(move |i| self.triples[c.get(i) as usize][2])
    }

    /// Members of an RDF list (`rdf:first` / `rdf:rest`), or `None` if `head` is not a
    /// well-formed list (missing links or cycles).
    pub fn list(&self, head: u64, first: u64, rest: u64, nil: u64, hi: u32) -> Option<Vec<u64>> {
        const MAX_LEN: usize = 1 << 20;
        let mut out = Vec::new();
        let mut cur = head;
        let mut seen = rustc_hash::FxHashSet::default();
        while cur != nil {
            if !seen.insert(cur) || out.len() > MAX_LEN {
                return None;
            }
            // with several values (e.g. copies made by owl:sameAs replacement) the
            // earliest (asserted) one wins
            out.push(self.objects(cur, first, hi).next()?);
            cur = self.objects(cur, rest, hi).next()?;
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_and_indexes() {
        let mut g = Graph::default();
        assert!(g.add([1, 10, 2]));
        assert!(!g.add([1, 10, 2]));
        assert_eq!(g.add_batch([[1, 10, 3], [2, 11, 3], [1, 10, 3]]), 2);
        assert_eq!(g.len(), 3);
        assert_eq!(g.cands(Some(1), Some(10), None, 0, 3).len(), 2);
        assert_eq!(g.cands(Some(1), Some(10), None, 1, 3).len(), 1);
        assert_eq!(g.cands(None, Some(10), Some(3), 0, 1).len(), 0);
        assert_eq!(g.cands(Some(2), Some(11), Some(3), 0, 3).len(), 1);
        assert_eq!(g.cands(Some(2), Some(11), Some(3), 0, 2).len(), 0);
        // without the subject index the access falls back to a range scan
        assert_eq!(g.cands(Some(1), None, None, 0, 3).len(), 3);
        g.ensure_s_index();
        assert_eq!(g.cands(Some(1), None, None, 0, 3).len(), 2);
        g.add([1, 12, 4]);
        assert_eq!(g.cands(Some(1), None, None, 0, 4).len(), 3);
        assert_eq!(g.pstats(10), (1, 2));
        assert_eq!(g.count_p(10, 1, 4), 1);
    }

    #[test]
    fn lists() {
        let (first, rest, nil) = (100, 101, 102);
        let mut g = Graph::default();
        g.add_batch([[1, first, 7], [1, rest, 2], [2, first, 8], [2, rest, nil], [5, first, 9], [5, rest, 5]]);
        assert_eq!(g.list(1, first, rest, nil, g.len()), Some(vec![7, 8]));
        assert_eq!(g.list(nil, first, rest, nil, g.len()), Some(vec![]));
        assert_eq!(g.list(5, first, rest, nil, g.len()), None, "cycle");
        assert_eq!(g.list(3, first, rest, nil, g.len()), None, "not a list");
    }
}
