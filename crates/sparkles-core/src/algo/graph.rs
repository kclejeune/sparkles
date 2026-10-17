//! The projection: compressed sparse rows over dense node indexes (spec F14 §3).

use super::memory::{self, Buffer, Growing, Reservation, Work};
use super::{Control, Par};
use crate::error::Result;
use std::sync::OnceLock;

/// Compressed sparse rows: the neighbours of node `u` are `adj[off[u]..off[u + 1]]`,
/// sorted by index, with their weights in `w` at the same positions when there are any.
#[derive(Clone, Debug, Default)]
pub struct Csr {
    pub off: Vec<u64>,
    pub adj: Vec<u32>,
    pub w: Option<Vec<f32>>,
}

impl Csr {
    /// The number of nodes.
    #[inline]
    pub fn n(&self) -> usize {
        self.off.len().saturating_sub(1)
    }

    /// The number of stored edges.
    #[inline]
    pub fn nnz(&self) -> usize {
        self.adj.len()
    }

    #[inline]
    pub fn range(&self, u: u32) -> std::ops::Range<usize> {
        self.off[u as usize] as usize..self.off[u as usize + 1] as usize
    }

    /// The neighbours of `u`.
    #[inline]
    pub fn row(&self, u: u32) -> &[u32] {
        &self.adj[self.range(u)]
    }

    /// The weights of the edges of `u` (`None` when the rows have no weights).
    #[inline]
    pub fn wrow(&self, u: u32) -> Option<&[f32]> {
        self.w.as_ref().map(|w| &w[self.range(u)])
    }

    #[inline]
    pub fn deg(&self, u: u32) -> usize {
        self.range(u).len()
    }

    /// Bytes the rows take.
    pub fn bytes(&self) -> u64 {
        self.off.len() as u64 * 8
            + self.adj.len() as u64 * 4
            + self.w.as_ref().map_or(0, |w| w.len() as u64 * 4)
    }

    /// Whether `v` is a neighbour of `u`.
    #[inline]
    pub fn has(&self, u: u32, v: u32) -> bool {
        self.row(u).binary_search(&v).is_ok()
    }

    fn retained_bytes(&self) -> Result<u64> {
        memory::sum(&[
            memory::bytes::<u64>(self.off.capacity())?,
            memory::bytes::<u32>(self.adj.capacity())?,
            self.w
                .as_ref()
                .map_or(Ok(0), |w| memory::bytes::<f32>(w.capacity()))?,
        ])
    }

    fn transpose_bytes(&self) -> Result<u64> {
        let offsets = self
            .n()
            .checked_add(1)
            .ok_or_else(|| super::bad("projection", "offset count overflows"))?;
        memory::sum(&[
            memory::bytes::<u64>(offsets)?,
            memory::bytes::<u32>(self.adj.len())?,
            if self.w.is_some() {
                memory::bytes::<f32>(self.adj.len())?
            } else {
                0
            },
        ])
    }

    /// A separately owned transpose, charged until its vectors have been dropped.
    pub(crate) fn transpose(&self, ctl: &Control) -> Result<OwnedCsr> {
        self.transpose_reserved(ctl, Reservation::new(ctl, self.transpose_bytes()?)?)
    }

    fn transpose_reserved(&self, ctl: &Control, reservation: Reservation) -> Result<OwnedCsr> {
        let n = self.n();
        let mut work = Work::new(ctl)?;
        // The owner exists before allocation so every error drops its vectors first.
        let mut owner = OwnedCsr {
            csr: Csr::default(),
            reservation,
        };
        let mut at = Buffer::<u64>::new("incoming-cursors", n, ctl)?;
        owner.csr.off = memory::allocate("incoming-offsets", n + 1, ctl)?;
        for _ in 0..=n {
            work.tick("incoming-zero-offsets")?;
            owner.csr.off.push(0);
        }
        for &v in &self.adj {
            work.tick("incoming-count")?;
            owner.csr.off[v as usize + 1] += 1;
        }
        for i in 0..n {
            work.tick("incoming-prefix")?;
            owner.csr.off[i + 1] += owner.csr.off[i];
        }
        for &v in &owner.csr.off[..n] {
            work.tick("incoming-cursors")?;
            at.push(v)?;
        }
        owner.csr.adj = memory::allocate("incoming-edges", self.adj.len(), ctl)?;
        for _ in 0..self.adj.len() {
            work.tick("incoming-zero-edges")?;
            owner.csr.adj.push(0);
        }
        if self.w.is_some() {
            owner.csr.w = Some(memory::allocate("incoming-weights", self.adj.len(), ctl)?);
            for _ in 0..self.adj.len() {
                work.tick("incoming-zero-weights")?;
                owner.csr.w.as_mut().unwrap().push(0.0);
            }
        }
        for u in 0..n as u32 {
            work.tick("incoming-source")?;
            for i in self.range(u) {
                work.tick("incoming-scatter")?;
                let v = self.adj[i] as usize;
                let p = at[v] as usize;
                at[v] += 1;
                owner.csr.adj[p] = u;
                if let (Some(dst), Some(src)) = (owner.csr.w.as_mut(), self.w.as_ref()) {
                    dst[p] = src[i];
                }
            }
        }
        ctl.check()?;
        Ok(owner)
    }
}

// Both owners drop data before the corresponding per-call reservation.
pub(crate) struct OwnedCsr {
    csr: Csr,
    reservation: Reservation,
}
impl std::ops::Deref for OwnedCsr {
    type Target = Csr;
    fn deref(&self) -> &Csr {
        &self.csr
    }
}

/// Incoming rows borrowed for one algorithm invocation. Keep this handle until all
/// workers using its rows have joined. Directed cached rows count against every caller;
/// symmetric rows reuse outgoing storage without an additional reservation.
pub struct Incoming<'a> {
    csr: &'a Csr,
    _reservation: Option<Reservation>,
}
impl std::ops::Deref for Incoming<'_> {
    type Target = Csr;
    fn deref(&self) -> &Csr {
        self.csr
    }
}

/// How parallel edges between two nodes combine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregate {
    /// kept apart
    None,
    Sum,
    Max,
    /// one edge whose weight is the number of the edges
    Count,
}

/// The edges of a projection as the ids of the store, before they get indexes.
#[derive(Clone, Debug, Default)]
pub struct RawEdges {
    /// the ids of the nodes, sorted and without repeats; an edge with an end outside
    /// them is left out
    pub nodes: Vec<u64>,
    pub src: Vec<u64>,
    pub dst: Vec<u64>,
    pub w: Option<Vec<f32>>,
    /// every edge is also present reversed (the projection reads edges both ways)
    pub symmetric: bool,
}

/// A projection: the nodes and the rows of their out-edges, with the in-edges built when
/// an algorithm asks for them.
#[derive(Debug)]
pub struct Graph {
    /// the store ids of the nodes, in index order
    pub ids: Vec<u64>,
    pub out: Csr,
    /// every edge is present in both directions
    pub symmetric: bool,
    inn: OnceLock<Csr>,
    inn_init: parking_lot::Mutex<()>,
}

/// Raw collection owners retain all capacities under one call, without readmission.
pub(crate) struct OwnedRawEdges {
    pub(crate) nodes: Buffer<u64>,
    pub(crate) src: Buffer<u64>,
    pub(crate) dst: Buffer<u64>,
    pub(crate) w: Option<Buffer<f32>>,
    pub(crate) symmetric: bool,
}
impl OwnedRawEdges {
    fn validate(&self, ctl: &Control) -> Result<()> {
        if !self.nodes.belongs_to(ctl)
            || !self.src.belongs_to(ctl)
            || !self.dst.belongs_to(ctl)
            || self.w.as_ref().is_some_and(|w| !w.belongs_to(ctl))
        {
            return Err(super::bad(
                "projection",
                "buffers belong to different calls",
            ));
        }
        if self.src.len() != self.dst.len()
            || self.w.as_ref().is_some_and(|w| w.len() != self.src.len())
        {
            return Err(super::bad("projection", "edge array lengths differ"));
        }
        Ok(())
    }
}

impl Graph {
    /// The number of nodes.
    #[inline]
    pub fn n(&self) -> usize {
        self.ids.len()
    }

    /// The number of edges (both directions of a symmetric graph).
    #[inline]
    pub fn m(&self) -> usize {
        self.out.nnz()
    }

    pub fn weighted(&self) -> bool {
        self.out.w.is_some()
    }

    /// Prepare incoming rows under this caller's cancellation, deadline and memory
    /// budget. Native callers migrate from `g.inn()` to `let incoming = g.inn(ctl)?;`
    /// and borrow `&*incoming` while workers run. Failed initialization can be retried.
    pub fn inn(&self, ctl: &Control) -> Result<Incoming<'_>> {
        ctl.check()?;
        if self.symmetric {
            return Ok(Incoming {
                csr: &self.out,
                _reservation: None,
            });
        }
        let reservation = Reservation::new(ctl, self.out.transpose_bytes()?)?;
        if let Some(csr) = self.inn.get() {
            return Ok(Incoming {
                csr,
                _reservation: Some(reservation),
            });
        }
        let guard = loop {
            ctl.check()?;
            if let Some(guard) = self
                .inn_init
                .try_lock_for(std::time::Duration::from_millis(10))
            {
                break guard;
            }
            #[cfg(test)]
            memory::probe::at(memory::probe::Point::Waiting);
        };
        ctl.check()?;
        if self.inn.get().is_none() {
            #[cfg(test)]
            memory::probe::at(memory::probe::Point::Initializing);
            let owner = self.out.transpose_reserved(ctl, reservation)?;
            #[cfg(test)]
            memory::probe::at(memory::probe::Point::Publishing);
            ctl.check()?;
            // The initializer mutex excludes competing publishers. A complete CSR is
            // retained by Graph; only the current call's charge lives in the handle.
            let OwnedCsr { csr, reservation } = owner;
            let _ = self.inn.set(csr);
            drop(guard);
            return Ok(Incoming {
                csr: self.inn.get().unwrap(),
                _reservation: Some(reservation),
            });
        }
        drop(guard);
        Ok(Incoming {
            csr: self.inn.get().unwrap(),
            _reservation: Some(reservation),
        })
    }

    /// Immutable base projection storage, independent of lazy incoming initialization.
    pub(crate) fn base_bytes(&self) -> Result<u64> {
        memory::sum(&[
            memory::bytes::<u64>(self.ids.capacity())?,
            self.out.retained_bytes()?,
        ])
    }

    pub(crate) fn cache_bytes(&self) -> Result<u64> {
        memory::sum(&[
            self.base_bytes()?,
            if self.symmetric {
                0
            } else {
                self.out.transpose_bytes()?
            },
        ])
    }

    /// Bytes the graph takes now.
    pub fn bytes(&self) -> u64 {
        self.ids.len() as u64 * 8 + self.out.bytes() + self.inn.get().map_or(0, |i| i.bytes())
    }

    /// Advisory size estimate for compatibility. Construction admits each actual
    /// capacity separately, including its overlapping temporary storage.
    pub fn estimate(n: u64, m: u64, weighted: bool) -> u64 {
        n.saturating_mul(16)
            .saturating_add(m.saturating_mul(if weighted { 28 } else { 20 }))
    }

    /// Number the nodes and build the rows from `raw`.
    pub fn build(raw: RawEdges, agg: Aggregate, par: &Par, ctl: &Control) -> Result<Graph> {
        // Caller-owned inputs already exist; admit their actual capacities on entry.
        let raw = OwnedRawEdges {
            nodes: Buffer::adopt(raw.nodes, ctl)?,
            src: Buffer::adopt(raw.src, ctl)?,
            dst: Buffer::adopt(raw.dst, ctl)?,
            w: raw.w.map(|w| Buffer::adopt(w, ctl)).transpose()?,
            symmetric: raw.symmetric,
        };
        Self::build_owned(raw, agg, par, ctl)
    }

    pub(crate) fn build_owned(
        raw: OwnedRawEdges,
        agg: Aggregate,
        par: &Par,
        ctl: &Control,
    ) -> Result<Graph> {
        raw.validate(ctl)?;
        let n = raw.nodes.len();
        if n >= u32::MAX as usize {
            return Err(super::bad("projection", "more than 4 billion nodes"));
        }
        let m = raw.src.len();
        let weighted = raw.w.is_some() || matches!(agg, Aggregate::Sum | Aggregate::Count);
        let mut keys = Buffer::filled("projection-keys", m, (u64::MAX, 0u32), ctl)?;
        let index = |id: u64| raw.nodes.binary_search(&id).map_or(u32::MAX, |i| i as u32);
        par.chunks_mut(&mut keys, 1 << 16, |lo, chunk| {
            ctl.check()?;
            for (i, k) in chunk.iter_mut().enumerate() {
                let (a, b) = (index(raw.src[lo + i]), index(raw.dst[lo + i]));
                if a != u32::MAX && b != u32::MAX {
                    *k = (
                        (a as u64) << 32 | b as u64,
                        raw.w.as_ref().map_or(0, |w| w[lo + i].to_bits()),
                    );
                }
            }
            Ok(())
        })?;
        let unweighted_input = raw.w.is_none();
        drop(raw.src);
        drop(raw.dst);
        drop(raw.w);
        ctl.check()?;
        par.sort(&mut keys);
        ctl.check()?;
        let kept = keys.partition_point(|k| k.0 != u64::MAX);
        keys.truncate(kept);
        if agg != Aggregate::None {
            let mut out = Buffer::new("projection-aggregate", keys.len(), ctl)?;
            let mut count = 0u32;
            let mut sum = 0f64;
            let mut last = u64::MAX;
            let fin = |out: &mut Buffer<(u64, u32)>, k: u64, count: u32, sum: f64| -> Result<()> {
                let v = if agg == Aggregate::Count {
                    count as f32
                } else {
                    sum as f32
                };
                if agg == Aggregate::Sum && !v.is_finite() {
                    return Err(super::bad(
                        "projection",
                        "summed edge weight is not finite as f32",
                    ));
                }
                out.push((k, v.to_bits()))
            };
            let mut work = Work::new(ctl)?;
            for &(k, wb) in &*keys {
                work.tick("projection-aggregate")?;
                let x = if unweighted_input {
                    1.0
                } else {
                    f32::from_bits(wb) as f64
                };
                if k != last {
                    if last != u64::MAX {
                        fin(&mut out, last, count, sum)?;
                    }
                    last = k;
                    count = 0;
                    sum = if agg == Aggregate::Max {
                        f64::NEG_INFINITY
                    } else {
                        0.0
                    };
                }
                count = count
                    .checked_add(1)
                    .ok_or_else(|| super::bad("projection", "aggregate count overflows"))?;
                if agg == Aggregate::Max {
                    sum = sum.max(x);
                } else {
                    sum += x;
                }
            }
            if last != u64::MAX {
                fin(&mut out, last, count, sum)?;
            }
            keys = out;
        }
        let offsets = n
            .checked_add(1)
            .ok_or_else(|| super::bad("projection", "offset count overflows"))?;
        let mut off = Buffer::filled("projection-offsets", offsets, 0u64, ctl)?;
        let mut adj = Buffer::new("projection-adjacency", keys.len(), ctl)?;
        let mut weights = weighted
            .then(|| Buffer::new("projection-weights", keys.len(), ctl))
            .transpose()?;
        let mut work = Work::new(ctl)?;
        for &(k, wb) in &*keys {
            work.tick("projection-csr")?;
            let row = (k >> 32) as usize + 1;
            off[row] = off[row]
                .checked_add(1)
                .ok_or_else(|| super::bad("projection", "row count overflows"))?;
            adj.push(k as u32)?;
            if let Some(w) = &mut weights {
                w.push(f32::from_bits(wb))?;
            }
        }
        for i in 0..n {
            work.tick("projection-prefix")?;
            off[i + 1] = off[i + 1]
                .checked_add(off[i])
                .ok_or_else(|| super::bad("projection", "offset sum overflows"))?;
        }
        drop(keys);
        ctl.check()?;
        // No fallible work follows the final detach. All retained capacities stay charged.
        Ok(Graph {
            ids: raw.nodes.into_output(),
            out: Csr {
                off: off.into_output(),
                adj: adj.into_output(),
                w: weights.map(Buffer::into_output),
            },
            symmetric: raw.symmetric,
            inn: OnceLock::new(),
            inn_init: parking_lot::Mutex::new(()),
        })
    }

    /// The graph read as undirected, simple and unweighted: rows without repeats or
    /// self-loops, holding an edge in both rows. The rows are charged to the call for
    /// the whole call.
    pub fn simple(&self, ctl: &Control) -> Result<Csr> {
        let n = self.n();
        let incoming = self.inn(ctl)?;
        let inn = &*incoming;
        let cap = self
            .out
            .nnz()
            .checked_mul(if self.symmetric { 1 } else { 2 })
            .ok_or_else(|| super::bad("projection", "edge count overflows"))?;
        let mut off = Buffer::<u64>::zeroed("simple-offsets", n + 1, ctl)?;
        let mut adj = Buffer::<u32>::new("simple-edges", cap, ctl)?;
        let mut work = Work::new(ctl)?;
        let mut counted = false;
        for u in 0..n as u32 {
            // The rows of a block of a symmetric graph are counted at once while they
            // stay within an interval. Other rows are counted one by one.
            if (u as usize).is_multiple_of(super::BLOCK) {
                let (lo, hi) = (u as usize, (u as usize + super::BLOCK).min(n));
                counted = self.symmetric && work.rows(&self.out.off, lo, hi, "simple-rows")?;
            }
            if !counted {
                let units = self.out.deg(u) + if self.symmetric { 0 } else { inn.deg(u) };
                work.tick_n(units + 1, "simple-rows")?;
            }
            let start = adj.len();
            if self.symmetric {
                for &v in self.out.row(u) {
                    if v != u {
                        adj.push(v)?;
                    }
                }
            } else {
                merge_rows(self.out.row(u), inn.row(u), &mut adj)?;
                // the merge may hold u itself
                let mut w = start;
                for r in start..adj.len() {
                    if adj[r] != u {
                        adj[w] = adj[r];
                        w += 1;
                    }
                }
                adj.truncate(w);
            }
            // sorted rows: drop repeats
            let mut last = u32::MAX;
            let mut w = start;
            for r in start..adj.len() {
                if adj[r] != last {
                    last = adj[r];
                    adj[w] = last;
                    w += 1;
                }
            }
            adj.truncate(w);
            off[u as usize + 1] = adj.len() as u64;
        }
        Ok(Csr {
            off: off.into_output(),
            adj: adj.into_output(),
            w: None,
        })
    }

    /// The graph read as undirected and weighted: rows without self-loops, parallel edges
    /// merged by summing their weights (1 for an edge without one). An edge of a directed
    /// graph is held in both rows. The rows are charged to the call for the whole call.
    pub fn weighted_sym(&self, ctl: &Control) -> Result<Csr> {
        let n = self.n();
        let incoming = self.inn(ctl)?;
        let inn = &*incoming;
        let cap = self
            .out
            .nnz()
            .checked_mul(if self.symmetric { 1 } else { 2 })
            .ok_or_else(|| super::bad("projection", "edge count overflows"))?;
        let mut off = Buffer::<u64>::zeroed("weighted-offsets", n + 1, ctl)?;
        let mut adj = Buffer::<u32>::new("weighted-edges", cap, ctl)?;
        let mut ws = Buffer::<f32>::new("weighted-weights", cap, ctl)?;
        let mut tmp: Growing<(u32, f32)> = Growing::new("weighted-row", ctl)?;
        let mut work = Work::new(ctl)?;
        for u in 0..n as u32 {
            tmp.clear();
            let rows: &[&Csr] = if self.symmetric {
                &[&self.out]
            } else {
                &[&self.out, inn]
            };
            for csr in rows {
                let (r, w) = (csr.row(u), csr.wrow(u));
                work.tick_n(r.len() + 1, "weighted-rows")?;
                tmp.reserve(r.len())?;
                for (i, &v) in r.iter().enumerate() {
                    if v != u {
                        tmp.push((v, w.map_or(1.0, |w| w[i])))?;
                    }
                }
            }
            tmp.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.to_bits().cmp(&b.1.to_bits())));
            let mut i = 0;
            while i < tmp.len() {
                let v = tmp[i].0;
                let mut s = 0f32;
                while i < tmp.len() && tmp[i].0 == v {
                    s += tmp[i].1;
                    i += 1;
                }
                adj.push(v)?;
                ws.push(s)?;
            }
            off[u as usize + 1] = adj.len() as u64;
        }
        Ok(Csr {
            off: off.into_output(),
            adj: adj.into_output(),
            w: Some(ws.into_output()),
        })
    }
}

/// Append the merge of two sorted rows.
fn merge_rows(a: &[u32], b: &[u32], out: &mut Buffer<u32>) -> Result<()> {
    if out.room() < a.len() + b.len() {
        return Err(super::bad("memory", "fixed buffer capacity exceeded"));
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] <= b[j] {
            out.push_within(a[i]);
            i += 1;
        } else {
            out.push_within(b[j]);
            j += 1;
        }
    }
    out.extend_within(a[i..].iter().chain(&b[j..]).copied())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    #[test]
    fn builds_sorted_rows() {
        let g = digraph(4, &[(2, 1), (0, 3), (0, 1), (2, 0)]);
        assert_eq!(g.out.row(0), &[1, 3]);
        assert_eq!(g.out.row(2), &[0, 1]);
        assert_eq!(g.inn(&Control::unlimited()).unwrap().row(1), &[0, 2]);
        assert_eq!(g.m(), 4);
    }

    #[test]
    fn edges_outside_the_nodes_are_dropped() {
        let raw = RawEdges {
            nodes: vec![10, 20],
            src: vec![10, 10, 30],
            dst: vec![20, 30, 10],
            w: None,
            symmetric: false,
        };
        let g = Graph::build(raw, Aggregate::None, &Par::single(), &Control::unlimited()).unwrap();
        assert_eq!(g.m(), 1);
        assert_eq!(g.ids, vec![10, 20]);
    }

    #[test]
    fn aggregates_parallel_edges() {
        let mk = |agg| {
            let raw = RawEdges {
                nodes: vec![0, 1],
                src: vec![0, 0, 0],
                dst: vec![1, 1, 1],
                w: Some(vec![1.0, 2.0, 4.0]),
                symmetric: false,
            };
            Graph::build(raw, agg, &Par::single(), &Control::unlimited()).unwrap()
        };
        assert_eq!(mk(Aggregate::None).m(), 3);
        assert_eq!(mk(Aggregate::Sum).out.w.unwrap(), vec![7.0]);
        assert_eq!(mk(Aggregate::Max).out.w.unwrap(), vec![4.0]);
        assert_eq!(mk(Aggregate::Count).out.w.unwrap(), vec![3.0]);
    }

    fn unit_parallel() -> RawEdges {
        RawEdges {
            nodes: vec![0, 1, 2],
            src: vec![0, 0, 0],
            dst: vec![1, 1, 2],
            ..Default::default()
        }
    }

    #[test]
    fn unit_sum_retains_weights_and_accounts_for_rows() {
        let g = Graph::build(
            unit_parallel(),
            Aggregate::Sum,
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
        assert_eq!(g.out.row(0), &[1, 2]);
        assert_eq!(g.out.wrow(0), Some([2.0, 1.0].as_slice()));
        // Three ids, four offsets, two neighbours and two retained f32 weights.
        assert_eq!(g.out.bytes(), 48);
        assert_eq!(g.bytes(), 72);
        assert_eq!(
            g.inn(&Control::unlimited()).unwrap().wrow(1),
            Some([2.0].as_slice())
        );
        assert_eq!(
            g.inn(&Control::unlimited()).unwrap().wrow(2),
            Some([1.0].as_slice())
        );
        assert_eq!(g.bytes(), 120);
    }

    #[test]
    fn unit_sum_reserves_actual_overlapping_capacities() {
        // Raw nodes+edges and padded key tuples overlap (24+24+24+48).
        // The later aggregation and CSR stages have no higher live capacity.
        let bytes = 120;
        let too_small = Control::new(Default::default(), None, bytes - 1);
        assert!(Graph::build(unit_parallel(), Aggregate::Sum, &Par::single(), &too_small).is_err());
        let exact = Control::new(Default::default(), None, bytes);
        Graph::build(unit_parallel(), Aggregate::Sum, &Par::single(), &exact).unwrap();
        assert_eq!(exact.peak(), bytes);
    }

    #[test]
    fn unit_aggregation_modes_and_empty_inputs() {
        for (agg, m, w) in [
            (Aggregate::None, 3, None),
            (Aggregate::Max, 2, None),
            (Aggregate::Count, 2, Some(vec![2.0, 1.0])),
        ] {
            let g =
                Graph::build(unit_parallel(), agg, &Par::single(), &Control::unlimited()).unwrap();
            assert_eq!(g.m(), m);
            assert_eq!(g.out.w, w);
        }
        for agg in [
            Aggregate::None,
            Aggregate::Sum,
            Aggregate::Max,
            Aggregate::Count,
        ] {
            let g = Graph::build(
                RawEdges::default(),
                agg,
                &Par::single(),
                &Control::unlimited(),
            )
            .unwrap();
            assert_eq!(g.out.off, vec![0]);
            assert!(g.out.adj.is_empty());
            assert_eq!(
                g.weighted(),
                matches!(agg, Aggregate::Sum | Aggregate::Count)
            );
        }
    }

    #[test]
    fn sum_refuses_overflow_in_stored_weights() {
        let raw = RawEdges {
            nodes: vec![0, 1],
            src: vec![0, 0],
            dst: vec![1, 1],
            w: Some(vec![f32::MAX, f32::MAX]),
            ..Default::default()
        };
        let e =
            Graph::build(raw, Aggregate::Sum, &Par::single(), &Control::unlimited()).unwrap_err();
        assert!(matches!(e, crate::Error::Invalid(_)), "{e}");
    }

    #[test]
    fn simple_view_merges_both_ways() {
        let g = digraph(3, &[(0, 1), (1, 0), (1, 1), (2, 1)]);
        let s = g.simple(&Control::unlimited()).unwrap();
        assert_eq!(s.row(0), &[1]);
        assert_eq!(s.row(1), &[0, 2]);
        assert_eq!(s.row(2), &[1]);
    }

    #[test]
    fn budget_fails_before_building() {
        let raw = RawEdges {
            nodes: (0..1000).collect(),
            src: vec![0; 10],
            dst: vec![1; 10],
            w: None,
            symmetric: false,
        };
        let ctl = Control::new(Default::default(), None, 100);
        assert!(Graph::build(raw, Aggregate::None, &Par::single(), &ctl).is_err());
    }
    #[test]
    fn r7_cold_incoming_respects_native_budget() {
        let g = digraph(4, &[(0, 1), (1, 2), (2, 3)]);
        let ctl = Control::new(Default::default(), None, 32);
        let result = crate::algo::labelprop::run(&g, 1, &Par::single(), &ctl);
        assert!(
            matches!(result, Err(crate::Error::BudgetExceeded(_))),
            "unexpected success or wrong error"
        );
        assert!(
            g.inn.get().is_none(),
            "must refuse before transpose allocation"
        );
    }

    #[test]
    fn r7_warm_incoming_respects_native_budget() {
        let g = digraph(4, &[(0, 1), (1, 2), (2, 3)]);
        let _ = g.inn(&Control::unlimited()).unwrap();
        let ctl = Control::new(Default::default(), None, 32);
        let result = crate::algo::labelprop::run(&g, 1, &Par::single(), &ctl);
        assert!(
            matches!(result, Err(crate::Error::BudgetExceeded(_))),
            "unexpected success or wrong error"
        );
    }

    #[test]
    fn r7_cancelled_native_does_not_initialize() {
        let g = digraph(4, &[(0, 1)]);
        let ctl = Control::new(
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            None,
            u64::MAX,
        );
        assert!(matches!(
            crate::algo::labelprop::run(&g, 1, &Par::single(), &ctl),
            Err(crate::Error::Cancelled)
        ));
        assert!(
            g.inn.get().is_none(),
            "cancelled call built incoming before checking"
        );
    }

    #[test]
    fn r7_expired_native_does_not_initialize() {
        let g = digraph(4, &[(0, 1)]);
        let ctl = Control::new(
            Default::default(),
            Some(std::time::Instant::now()),
            u64::MAX,
        );
        assert!(matches!(
            crate::algo::labelprop::run(&g, 1, &Par::single(), &ctl),
            Err(crate::Error::Timeout)
        ));
        assert!(
            g.inn.get().is_none(),
            "expired call built incoming before checking"
        );
    }
}

#[cfg(test)]
#[path = "incoming_tests.rs"]
mod incoming_tests;

#[cfg(test)]
mod r6_before {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    fn duplicate_capacity() -> RawEdges {
        let mut nodes = Vec::with_capacity(16384);
        nodes.extend([0, 1]);
        RawEdges {
            nodes,
            src: vec![0],
            dst: vec![1],
            w: None,
            symmetric: false,
        }
    }
    #[test]
    fn r6_native_raw_keys_overlap_is_admitted_before_allocation() {
        let raw = duplicate_capacity();
        let raw_bytes =
            (raw.nodes.capacity() * 8 + raw.src.capacity() * 8 + raw.dst.capacity() * 8) as u64;
        super::super::set_projection_observer(Some(Arc::new(move |event| {
            if let super::super::ProjectionEvent::Allocation("projection-keys", _, request, used) =
                event
            {
                assert!(
                    used >= raw_bytes + request,
                    "raw {raw_bytes} + requested keys {request}, charged {used}"
                );
            }
        })));
        let _ = Graph::build(raw, Aggregate::None, &Par::single(), &Control::unlimited()).unwrap();
        super::super::set_projection_observer(None);
    }
    #[test]
    fn r6_native_retains_actual_node_capacity() {
        let ctl = Control::unlimited();
        let g = Graph::build(duplicate_capacity(), Aggregate::None, &Par::single(), &ctl).unwrap();
        assert_eq!(ctl.observed_used(), g.base_bytes().unwrap());
    }
    #[test]
    fn r6_empty_budget_refuses_before_offset_allocation() {
        let allocated = Arc::new(AtomicBool::new(false));
        let a = allocated.clone();
        super::super::set_projection_observer(Some(Arc::new(move |event| {
            if let super::super::ProjectionEvent::Allocation("projection-offsets", ..) = event {
                a.store(true, Ordering::Relaxed);
            }
        })));
        let result = Graph::build(
            RawEdges::default(),
            Aggregate::None,
            &Par::single(),
            &Control::new(Default::default(), None, 0),
        );
        super::super::set_projection_observer(None);
        assert!(
            matches!(result, Err(crate::Error::BudgetExceeded(_))),
            "unexpected success or failure"
        );
        assert!(
            !allocated.load(Ordering::Relaxed),
            "offset allocated before admission"
        );
    }
    #[test]
    fn r6_cancelled_builder_restores_its_prior_ledger() {
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        super::super::set_projection_observer(Some(Arc::new(move |event| {
            if let super::super::ProjectionEvent::Allocation("projection-keys", ..) = event {
                f.store(true, Ordering::Relaxed);
            }
        })));
        let ctl = Control::new(flag, None, u64::MAX);
        let result = Graph::build(duplicate_capacity(), Aggregate::None, &Par::single(), &ctl);
        super::super::set_projection_observer(None);
        assert!(matches!(result, Err(crate::Error::Cancelled)));
        assert_eq!(
            ctl.remaining(),
            u64::MAX,
            "failed build leaked capacity charge"
        );
    }
}

#[cfg(test)]
mod owned_build_tests {
    use super::*;
    fn raw() -> RawEdges {
        RawEdges {
            nodes: vec![0, 1, 2],
            src: vec![0, 0, 1],
            dst: vec![1, 1, 2],
            w: Some(vec![1., 2., 3.]),
            symmetric: false,
        }
    }
    #[test]
    fn every_builder_allocation_failure_releases_inputs_and_scratch() {
        for name in [
            "projection-keys",
            "projection-aggregate",
            "projection-offsets",
            "projection-adjacency",
            "projection-weights",
        ] {
            let ctl = Control::new(Default::default(), None, 4096);
            let scope = memory::probe::install(|_| {}, Some(name));
            assert!(
                Graph::build(raw(), Aggregate::Sum, &Par::single(), &ctl).is_err(),
                "{name}"
            );
            drop(scope);
            assert_eq!(ctl.remaining(), 4096, "{name}");
        }
    }
    #[test]
    fn successful_ledger_is_base_capacity_and_empty_offset_is_real() {
        for agg in [
            Aggregate::None,
            Aggregate::Max,
            Aggregate::Count,
            Aggregate::Sum,
        ] {
            let ctl = Control::new(Default::default(), None, 4096);
            let graph = Graph::build(raw(), agg, &Par::single(), &ctl).unwrap();
            assert_eq!(4096 - ctl.remaining(), graph.base_bytes().unwrap());
            let before = ctl.remaining();
            let incoming = graph.inn(&ctl).unwrap();
            assert!(ctl.remaining() < before);
            drop(incoming);
            assert_eq!(ctl.remaining(), before);
        }
        let ctl = Control::new(Default::default(), None, 8);
        let graph = Graph::build(
            RawEdges {
                nodes: vec![],
                src: vec![],
                dst: vec![],
                w: None,
                symmetric: false,
            },
            Aggregate::None,
            &Par::single(),
            &ctl,
        )
        .unwrap();
        assert_eq!(graph.out.off, &[0]);
        assert_eq!(ctl.remaining(), 0);
        assert_eq!(graph.base_bytes().unwrap(), 8);
    }
    #[test]
    fn foreign_and_misaligned_owners_refuse_without_retained_charge() {
        let ctl = Control::new(Default::default(), None, 1024);
        let other = Control::unlimited();
        let owned = OwnedRawEdges {
            nodes: Buffer::adopt(vec![0, 1], &other).unwrap(),
            src: Buffer::adopt(vec![0], &ctl).unwrap(),
            dst: Buffer::adopt(vec![1], &ctl).unwrap(),
            w: None,
            symmetric: false,
        };
        assert!(Graph::build_owned(owned, Aggregate::None, &Par::single(), &ctl).is_err());
        assert_eq!(ctl.remaining(), 1024);
        let mut r = raw();
        r.dst.clear();
        assert!(Graph::build(r, Aggregate::None, &Par::single(), &ctl).is_err());
        assert_eq!(ctl.remaining(), 1024);
    }
}
