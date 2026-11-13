//! The columnar derived tier: the facts the rules derive, in memory, by fact id and
//! sorted per predicate.
//!
//! A derived fact gets a dense `u32` id in the order of its first derivation, from the
//! graph's offset on, and the tier keeps its subject, object and predicate in one row of
//! 24 bytes by id ([`Row`]), so a fact is read from its id with one cache line, and the
//! facts of one merge are an interval of ids, which the semi-naive ranges of the engine
//! read. A hit in the graph's triple index checks its fact against the probed triple,
//! which three columns by id answered with three cache misses and the row answers with
//! one for most facts, since a row of 24 bytes straddles a 64-byte cache line in two of
//! eight places. Each predicate is a
//! relation with two orders of its ids, by subject then object and by object then
//! subject, as sorted runs ([`order::Order`], [`run::Run`]), and a list of its ids in
//! order. A probe with a bound predicate and subject searches the subject order, one with
//! a bound object the object order, and one with a bound predicate alone reads the list,
//! as it read the predicate's row list before.
//! Orders over the whole tier by subject and by object serve atoms with a variable
//! predicate and are built on demand.
//!
//! The row costs 24 bytes per fact, of which 4 are padding free for a later column, the
//! predicate's list of ids 4 and the two orders 10, with their fence keys, so a fact costs
//! 38 bytes in the tier, besides the graph's triple index that finds a fact by its triple.
//! The rows, lists and tables of the previous tier cost about 135 bytes.
//!
//! Deletion marks a fact dead in a bitmap. A dead fact stays in the orders, which skip
//! it, until a compaction drops it; a relation with a quarter of its facts dead is
//! rebuilt. A second bitmap marks the facts that count as explicit.

pub(crate) mod compact;
pub(crate) mod order;
pub(crate) mod run;

use crate::budget::{MemoryExceeded, Meter, table_bytes};
use order::Order;
use rayon::prelude::*;
use run::Run;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

pub(crate) type Triple = [u64; 3];

/// Batches smaller than this build their runs on the calling thread.
const PAR_MIN: usize = 1 << 14;

/// A fact: its subject, object and the index of its relation, in 24 bytes, so that the
/// three are read with one cache line in six of eight places and two otherwise. The last
/// four bytes are padding, free for a later column.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Row {
    pub s: u64,
    pub o: u64,
    pub ri: u32,
    _pad: u32,
}

impl Row {
    #[inline]
    fn new(s: u64, o: u64, ri: u32) -> Row {
        Row { s, o, ri, _pad: 0 }
    }
}

/// The row's bytes per fact.
pub(crate) const ROW: usize = size_of::<Row>();

/// Candidate ids for one access: pieces of sorted runs, or ranges of ids, in the order
/// the access visits them. A piece of a run that an id range meets only in part is
/// filtered by the range when visited, so [`len`](Self::len) is an upper bound of the
/// ids visited.
#[derive(Clone)]
pub(crate) struct Cands<'a> {
    parts: SmallVec<[Part<'a>; 4]>,
    lo: u32,
    hi: u32,
}

#[derive(Clone, Copy)]
enum Part<'a> {
    /// ids of a run, filtered by the range when the flag is set
    Ids(&'a [u32], bool),
    Range(u32, u32),
}

impl<'a> Cands<'a> {
    pub fn empty() -> Cands<'a> {
        Cands::with_bounds(0, 0)
    }

    /// The ids `[lo, hi)`, or nothing when `lo >= hi`.
    pub fn range(lo: u32, hi: u32) -> Cands<'a> {
        let mut c = Cands::with_bounds(lo, hi);
        if lo < hi {
            c.parts.push(Part::Range(lo, hi));
        }
        c
    }

    /// These ids, all of them.
    pub fn ids(ids: &'a [u32]) -> Cands<'a> {
        let mut c = Cands::with_bounds(0, u32::MAX);
        c.push_ids(ids, false);
        c
    }

    fn with_bounds(lo: u32, hi: u32) -> Cands<'a> {
        Cands {
            parts: SmallVec::new(),
            lo,
            hi,
        }
    }

    #[inline]
    pub(super) fn push_ids(&mut self, ids: &'a [u32], filtered: bool) {
        if !ids.is_empty() {
            self.parts.push(Part::Ids(ids, filtered));
        }
    }

    /// The ids visited at most.
    #[inline]
    pub fn len(&self) -> usize {
        self.parts
            .iter()
            .map(|p| match p {
                Part::Ids(s, _) => s.len(),
                Part::Range(a, b) => (b - a) as usize,
            })
            .sum()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// The ids, in order.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        let (lo, hi) = (self.lo, self.hi);
        self.parts.iter().flat_map(move |p| match *p {
            Part::Ids(s, false) => Iter::Ids(s.iter()),
            Part::Ids(s, true) => Iter::Filtered(s.iter(), lo, hi),
            Part::Range(a, b) => Iter::Range(a..b),
        })
    }

    /// Calls `f` with the ids in order until it returns false, and returns false then.
    /// An evaluation's scan visits its candidates this way: each piece is a plain loop
    /// over a slice or a range, so the loop around a candidate is a few instructions
    /// and the processor keeps the memory reads of several candidates in flight.
    #[inline(always)]
    pub fn try_each(&self, mut f: impl FnMut(u32) -> bool) -> bool {
        let (lo, hi) = (self.lo, self.hi);
        for p in &self.parts {
            match *p {
                Part::Ids(s, false) => {
                    for &id in s {
                        if !f(id) {
                            return false;
                        }
                    }
                }
                Part::Ids(s, true) => {
                    for &id in s {
                        if id >= lo && id < hi && !f(id) {
                            return false;
                        }
                    }
                }
                Part::Range(a, b) => {
                    for id in a..b {
                        if !f(id) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// The first `n` candidates and the rest.
    pub fn split_at(&self, n: usize) -> (Cands<'a>, Cands<'a>) {
        let (mut head, mut tail) = (
            Cands::with_bounds(self.lo, self.hi),
            Cands::with_bounds(self.lo, self.hi),
        );
        let mut left = n;
        for p in &self.parts {
            let (a, b) = p.split_at(left);
            left -= a.len();
            head.add(a);
            tail.add(b);
        }
        (head, tail)
    }

    /// Pieces of at most `n` candidates.
    pub fn chunks(&self, n: usize) -> Vec<Cands<'a>> {
        let n = n.max(1);
        let mut out = Vec::new();
        let mut cur = Cands::with_bounds(self.lo, self.hi);
        let mut room = n;
        for p in &self.parts {
            let mut p = *p;
            loop {
                let (a, b) = p.split_at(room);
                room -= a.len();
                cur.add(a);
                if room == 0 {
                    out.push(std::mem::replace(
                        &mut cur,
                        Cands::with_bounds(self.lo, self.hi),
                    ));
                    room = n;
                }
                if b.len() == 0 {
                    break;
                }
                p = b;
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }

    fn add(&mut self, p: Part<'a>) {
        if p.len() > 0 {
            self.parts.push(p);
        }
    }
}

impl<'a> Part<'a> {
    fn len(&self) -> usize {
        match self {
            Part::Ids(s, _) => s.len(),
            Part::Range(a, b) => (b - a) as usize,
        }
    }

    fn split_at(self, n: usize) -> (Part<'a>, Part<'a>) {
        let n = n.min(self.len());
        match self {
            Part::Ids(s, f) => {
                let (a, b) = s.split_at(n);
                (Part::Ids(a, f), Part::Ids(b, f))
            }
            Part::Range(a, b) => (Part::Range(a, a + n as u32), Part::Range(a + n as u32, b)),
        }
    }
}

enum Iter<'a> {
    Ids(std::slice::Iter<'a, u32>),
    Filtered(std::slice::Iter<'a, u32>, u32, u32),
    Range(std::ops::Range<u32>),
}

impl Iterator for Iter<'_> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        match self {
            Iter::Ids(it) => it.next().copied(),
            Iter::Filtered(it, lo, hi) => it.find(|&&id| id >= *lo && id < *hi).copied(),
            Iter::Range(r) => r.next(),
        }
    }
}

/// The facts of one predicate: its two orders and its dead facts.
pub(crate) struct Rel {
    pub p: u64,
    /// the ids, ascending, as the row list of a predicate was before the tier: a probe
    /// with the predicate alone reads them, and a count over an id range is a binary
    /// search in them
    pub ids: Vec<u32>,
    /// by subject, then id
    pub subj: Order,
    /// by object, then id
    pub obj: Order,
    /// dead facts still in the orders and the ids
    pub dead: u32,
}

impl Rel {
    /// The ids in `[lo, hi)`.
    #[inline]
    fn ids_in(&self, lo: u32, hi: u32) -> &[u32] {
        let a = if lo == 0 {
            0
        } else {
            self.ids.partition_point(|&id| id < lo)
        };
        let b = self.ids.partition_point(|&id| id < hi);
        &self.ids[a..b.max(a)]
    }
}

pub(crate) struct Tier {
    /// the first id: ids before it are the base tier's
    offset: u32,
    /// the facts by id, each with the index of its relation in `rels`
    rows: Vec<Row>,
    rels: Vec<Rel>,
    /// the predicate of each relation, by its index in `rels`: a dense copy of `Rel::p`,
    /// so that reading a fact's triple reads eight bytes of one small array and not a
    /// field of the relation's large record
    preds: Vec<u64>,
    rel_of: FxHashMap<u64, u32>,
    /// by subject and by object over the whole tier, built on demand
    by_s: Option<Order>,
    by_o: Option<Order>,
    /// one bit per fact, set when it is dead (empty while none is)
    pub dead: Vec<u64>,
    dead_rows: u32,
    /// one bit per fact, set when it counts as explicit
    pub flags: Vec<u64>,
    /// a compaction takes no run with an id of this or more: the start of the delta of
    /// an iteration in progress
    floor: u32,
}

impl Tier {
    pub fn new(offset: u32) -> Tier {
        Tier {
            offset,
            rows: Vec::new(),
            rels: Vec::new(),
            preds: Vec::new(),
            rel_of: FxHashMap::default(),
            by_s: None,
            by_o: None,
            dead: Vec::new(),
            dead_rows: 0,
            flags: Vec::new(),
            floor: u32::MAX,
        }
    }

    /// Protect the facts from id `floor` on from compaction, until the next call: an
    /// iteration reads them as its delta, and a run that a compaction merged across the
    /// delta's start would be read in part and counted by a scan.
    pub fn set_compaction_floor(&mut self, floor: u32) {
        self.floor = floor;
    }

    /// Lower the floor to `id` when it is above: a batch added between runs may be the
    /// delta a later run starts from, so later batches leave its runs alone until the
    /// engine sets the floor again.
    pub fn lower_floor(&mut self, id: u32) {
        self.floor = self.floor.min(id);
    }

    pub fn set_offset(&mut self, offset: u32) {
        debug_assert!(self.rows.is_empty());
        self.offset = offset;
    }

    #[inline]
    pub fn offset(&self) -> u32 {
        self.offset
    }

    /// The id after the last fact.
    #[inline]
    pub fn len(&self) -> u32 {
        self.offset + self.rows.len() as u32
    }

    /// The facts, dead ones included.
    #[inline]
    pub fn count(&self) -> usize {
        self.rows.len()
    }

    pub fn capacity(&self) -> usize {
        self.rows.capacity()
    }

    pub fn reserve_exact(&mut self, n: usize) {
        self.rows.reserve_exact(n);
    }

    #[inline]
    fn at(&self, id: u32) -> usize {
        (id - self.offset) as usize
    }

    #[inline]
    pub fn triple(&self, id: u32) -> Triple {
        let r = &self.rows[self.at(id)];
        [r.s, self.preds[r.ri as usize], r.o]
    }

    /// Whether fact `id` is the triple `t`: one read of the row and of its relation's
    /// predicate.
    #[inline]
    pub fn eq(&self, id: u32, t: &Triple) -> bool {
        let r = &self.rows[self.at(id)];
        // one test of the three, without a branch on each component read from the row,
        // which the processor would otherwise follow before it issues the next probe
        (r.s == t[0]) & (r.o == t[2]) & (self.preds[r.ri as usize] == t[1])
    }

    /// The facts in id order, dead ones included.
    pub fn facts(&self) -> impl Iterator<Item = Triple> + '_ {
        self.facts_range(0, self.rows.len())
    }

    /// The facts at row indexes `[a, b)`, in id order, dead ones included.
    pub fn facts_range(&self, a: usize, b: usize) -> impl Iterator<Item = Triple> + '_ {
        self.rows[a..b]
            .iter()
            .map(|r| [r.s, self.preds[r.ri as usize], r.o])
    }

    /// The bytes the relations and their table grow by when a fact with predicate `p`
    /// is pushed: none when the predicate has a relation.
    #[inline]
    pub fn rel_growth(&self, p: u64) -> usize {
        if self.rel_of.contains_key(&p) {
            return 0;
        }
        let mut n = 0;
        if self.rels.len() == self.rels.capacity() {
            n += self.rels.capacity().max(4) * size_of::<Rel>();
        }
        if self.preds.len() == self.preds.capacity() {
            n += self.preds.capacity().max(4) * size_of::<u64>();
        }
        if self.rel_of.len() == self.rel_of.capacity() {
            n += table_bytes(
                crate::budget::table_grown(self.rel_of.capacity(), self.rel_of.len(), 1),
                size_of::<(u64, u32)>(),
            );
        }
        n
    }

    /// Append a fact; the caller indexes it ([`index_new`](Self::index_new)) and
    /// charges [`rel_growth`](Self::rel_growth) first.
    #[inline]
    pub fn push(&mut self, t: Triple) {
        let ri = match self.rel_of.get(&t[1]) {
            Some(&i) => i,
            None => {
                let i = self.rels.len() as u32;
                self.rels.push(Rel {
                    p: t[1],
                    ids: Vec::new(),
                    subj: Order::default(),
                    obj: Order::default(),
                    dead: 0,
                });
                self.preds.push(t[1]);
                self.rel_of.insert(t[1], i);
                i
            }
        };
        self.rows.push(Row::new(t[0], t[2], ri));
    }

    #[inline]
    pub fn alive(&self, id: u32) -> bool {
        let i = self.at(id);
        self.dead
            .get(i / 64)
            .is_none_or(|w| w & (1 << (i % 64)) == 0)
    }

    /// The bytes the dead bitmap allocates to hold a bit for `id`, and the bytes of the
    /// allocation it releases then: both zero when the bit fits.
    pub fn dead_growth(&self, id: u32) -> (usize, usize) {
        bitmap_growth(&self.dead, self.at(id))
    }

    /// Mark a live fact dead.
    pub fn kill(&mut self, id: u32) {
        let i = self.at(id);
        set_bit(&mut self.dead, i, true);
        self.dead_rows += 1;
        self.rels[self.rows[i].ri as usize].dead += 1;
        for o in [&mut self.by_s, &mut self.by_o].into_iter().flatten() {
            o.dead += 1;
        }
    }

    pub fn dead_rows(&self) -> u32 {
        self.dead_rows
    }

    #[inline]
    pub fn flagged(&self, id: u32) -> bool {
        let i = self.at(id);
        self.flags
            .get(i / 64)
            .is_some_and(|w| w & (1 << (i % 64)) != 0)
    }

    /// The bytes the flags allocate to hold a bit for `id`, and the bytes of the
    /// allocation they release then: both zero when the bit fits.
    pub fn flag_growth(&self, id: u32) -> (usize, usize) {
        bitmap_growth(&self.flags, self.at(id))
    }

    pub fn set_flag(&mut self, id: u32, on: bool) {
        let i = self.at(id);
        if !on && self.flags.len() <= i / 64 {
            return;
        }
        set_bit(&mut self.flags, i, on);
    }

    #[cfg(test)]
    pub fn rels(&self) -> &[Rel] {
        &self.rels
    }

    #[inline]
    fn rel(&self, p: u64) -> Option<&Rel> {
        self.rel_of.get(&p).map(|&i| &self.rels[i as usize])
    }

    pub fn predicate_count(&self) -> usize {
        self.rels.len()
    }

    /// Calls `f` with each predicate of a fact from id `lo` on.
    pub fn predicates_since(&self, lo: u32, mut f: impl FnMut(u64)) {
        for r in &self.rels {
            if r.ids.last().is_some_and(|&id| id >= lo) {
                f(r.p);
            }
        }
    }

    /// Facts with predicate `p` and an id in `[lo, hi)`, dead ones included.
    pub fn count_p(&self, p: u64, lo: u32, hi: u32) -> usize {
        let lo = lo.max(self.offset);
        if lo >= hi {
            return 0;
        }
        self.rel(p).map_or(0, |r| r.ids_in(lo, hi).len())
    }

    /// Distinct subjects and objects of predicate `p`, upper bounds.
    pub fn pstats(&self, p: u64) -> (usize, usize) {
        self.rel(p)
            .map_or((0, 0), |r| (r.subj.distinct(), r.obj.distinct()))
    }

    /// Distinct subjects, when the order by subject exists.
    pub fn distinct_subjects(&self) -> Option<usize> {
        self.by_s.as_ref().map(Order::distinct)
    }

    pub fn distinct_objects(&self) -> Option<usize> {
        self.by_o.as_ref().map(Order::distinct)
    }

    pub fn has_order(&self, subjects: bool) -> bool {
        if subjects {
            self.by_s.is_some()
        } else {
            self.by_o.is_some()
        }
    }

    /// Candidate ids for a pattern with the given bound positions and an id in
    /// `[lo, hi)`: a superset, which the caller checks component by component. A
    /// pattern with every position bound is the triple index's, not the tier's.
    pub fn cands(
        &self,
        s: Option<u64>,
        p: Option<u64>,
        o: Option<u64>,
        lo: u32,
        hi: u32,
    ) -> Cands<'_> {
        let lo = lo.max(self.offset);
        if lo >= hi {
            return Cands::empty();
        }
        let off = self.offset;
        let rows = &self.rows;
        let key_s = move |id: u32| rows[(id - off) as usize].s;
        let key_o = move |id: u32| rows[(id - off) as usize].o;
        let mut c = Cands::with_bounds(lo, hi);
        match (s, p, o) {
            (Some(s), Some(p), None) => {
                if let Some(r) = self.rel(p) {
                    r.subj.probe(s, lo, hi, &key_s, &mut c);
                }
            }
            (None, Some(p), Some(o)) => {
                if let Some(r) = self.rel(p) {
                    r.obj.probe(o, lo, hi, &key_o, &mut c);
                }
            }
            (None, Some(p), None) => {
                if let Some(r) = self.rel(p) {
                    c.push_ids(r.ids_in(lo, hi), false);
                }
            }
            (Some(s), None, o) => match (&self.by_s, o, &self.by_o) {
                (Some(m), _, _) => m.probe(s, lo, hi, &key_s, &mut c),
                (None, Some(o), Some(m)) => m.probe(o, lo, hi, &key_o, &mut c),
                _ => return Cands::range(lo, hi),
            },
            (None, None, Some(o)) => match &self.by_o {
                Some(m) => m.probe(o, lo, hi, &key_o, &mut c),
                None => return Cands::range(lo, hi),
            },
            (None, None, None) | (Some(_), Some(_), Some(_)) => return Cands::range(lo, hi),
        }
        c
    }

    /// The heap of the rows, the relations with their orders, the orders over the whole
    /// tier and the bitmaps, by capacity.
    pub fn heap_bytes(&self) -> usize {
        let mut n = self.rows.capacity() * ROW;
        n += self.rels.capacity() * size_of::<Rel>();
        n += self.preds.capacity() * size_of::<u64>();
        n += self
            .rels
            .iter()
            .map(|r| r.ids.capacity() * size_of::<u32>() + r.subj.heap_bytes() + r.obj.heap_bytes())
            .sum::<usize>();
        n += table_bytes(self.rel_of.capacity(), size_of::<(u64, u32)>());
        for o in [&self.by_s, &self.by_o].into_iter().flatten() {
            n += o.heap_bytes();
        }
        n += (self.dead.capacity() + self.flags.capacity()) * size_of::<u64>();
        n
    }

    /// Build the order by subject (`subjects`) or by object over the whole tier, its
    /// bytes charged to `m` before it is built.
    pub fn ensure_order(
        &mut self,
        subjects: bool,
        m: &mut Meter<'_>,
    ) -> Result<(), MemoryExceeded> {
        if self.has_order(subjects) {
            return Ok(());
        }
        let n = self.rows.len();
        let live = n - self.dead_rows as usize;
        let off = self.offset;
        let (rows, dead) = (&self.rows, &self.dead);
        let keep = |id: u32| alive_in(dead, (id - off) as usize);
        // the live ids, the sort's buffers and the run, then the run's list
        m.add(live * size_of::<u32>() + compact::sort_bytes(live) + Run::bytes_for(live))?;
        let mut ids: Vec<u32> = Vec::with_capacity(live);
        ids.extend((off..off + n as u32).filter(|&id| keep(id)));
        let mut order = Order::default();
        if subjects {
            let key = |id: u32| rows[(id - off) as usize].s;
            order.push_charged(compact::sorted(&ids, &key), m)?;
            self.by_s = Some(order);
        } else {
            let key = |id: u32| rows[(id - off) as usize].o;
            order.push_charged(compact::sorted(&ids, &key), m)?;
            self.by_o = Some(order);
        }
        drop(ids);
        m.sub(live * size_of::<u32>() + compact::sort_bytes(live));
        Ok(())
    }

    /// Index the facts from row index `start` on: compact the runs each of their
    /// relations has below the floor, then append the new facts as one run per order,
    /// sorted. Growth is charged to `m` (and its strictness) before it is allocated.
    pub fn index_new(&mut self, start: usize, m: &mut Meter<'_>) -> Result<(), MemoryExceeded> {
        let n = self.rows.len() - start;
        if n == 0 {
            return Ok(());
        }
        let off = self.offset;
        let floor = self.floor;
        let first = off + start as u32;
        // the new ids grouped by relation: a counting sort
        let nrels = self.rels.len();
        // the grouped ids, the two counting arrays and the tasks' results
        let scratch = n * size_of::<u32>()
            + 2 * (nrels + 1) * size_of::<usize>()
            + nrels * size_of::<Result<usize, MemoryExceeded>>();
        m.add(scratch)?;
        let mut counts = vec![0usize; nrels + 1];
        for r in &self.rows[start..] {
            counts[r.ri as usize + 1] += 1;
        }
        for i in 0..nrels {
            counts[i + 1] += counts[i];
        }
        let mut at = counts.clone();
        let mut grouped = vec![0u32; n];
        for (j, r) in self.rows[start..].iter().enumerate() {
            grouped[at[r.ri as usize]] = first + j as u32;
            at[r.ri as usize] += 1;
        }
        let Tier {
            rows,
            rels,
            by_s,
            by_o,
            dead,
            ..
        } = self;
        let (rows, dead): (&[Row], &[u64]) = (rows, dead);
        let keep = move |id: u32| alive_in(dead, (id - off) as usize);
        // each order sorts by its key and then id, so that the ids a probe finds ascend
        // within a run
        let key_s = move |id: u32| rows[(id - off) as usize].s;
        let key_o = move |id: u32| rows[(id - off) as usize].o;
        let (budget, strict) = (m.budget(), m.strict());
        let grouped_ref = &grouped;
        let counts = &counts;
        let t0 = std::time::Instant::now();
        // the bytes the task charged and the entries its compactions rewrote
        let task = |(ri, rel): (usize, &mut Rel)| -> Result<(usize, usize), MemoryExceeded> {
            let mut meter = Meter::new(budget, strict);
            let new = &grouped_ref[counts[ri]..counts[ri + 1]];
            let mut rewrote = 0;
            if floor > 0 {
                // a relation with a quarter of its facts dead is rebuilt without them
                let all = rel.dead > 0 && rel.dead as usize * 4 >= rel.subj.len();
                if all {
                    rel.ids.retain(|&id| keep(id));
                }
                let (d, w) = compaction(&mut rel.subj, floor, all, &key_s, &keep, &mut meter)?;
                rewrote += w;
                rewrote += compaction(&mut rel.obj, floor, all, &key_o, &keep, &mut meter)?.1;
                rel.dead -= d as u32;
            }
            if !new.is_empty() {
                // the ids, which ascend already, in a list that doubles
                let (len, cap) = (rel.ids.len(), rel.ids.capacity());
                if len + new.len() > cap {
                    let grown = (cap * 2).max(len + new.len()).max(16);
                    meter.add(grown * size_of::<u32>())?;
                    rel.ids.reserve_exact(grown - len);
                    meter.sub(cap * size_of::<u32>());
                }
                rel.ids.extend_from_slice(new);
                let scratch = compact::sort_bytes(new.len());
                meter.add(scratch + 2 * Run::bytes_for(new.len()))?;
                rel.subj
                    .push_charged(compact::sorted(new, &key_s), &mut meter)?;
                rel.obj
                    .push_charged(compact::sorted(new, &key_o), &mut meter)?;
                meter.sub(scratch);
            }
            Ok((meter.into_bytes(), rewrote))
        };
        let done: Vec<Result<(usize, usize), MemoryExceeded>> = if n < PAR_MIN {
            rels.iter_mut().enumerate().map(task).collect()
        } else {
            rels.par_iter_mut().enumerate().map(task).collect()
        };
        let mut first_err = None;
        let mut rewrote = 0;
        for d in done {
            match d {
                Ok((b, w)) => {
                    m.adopt(b);
                    rewrote += w;
                }
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        if let Some(e) = first_err {
            return Err(e);
        }
        let t_rels = std::time::Instant::now();
        // the orders over the whole tier take the new facts as one run each; the new
        // ids in order take the place of the grouped ones
        drop(grouped);
        let all: Vec<u32> = (first..off + (start + n) as u32).collect();
        for (order, subjects) in [(by_s, true), (by_o, false)] {
            let Some(order) = order else { continue };
            if floor > 0 {
                let whole = order.dead > 0 && order.dead as usize * 4 >= order.len();
                let (d, w) = if subjects {
                    compaction(order, floor, whole, &key_s, &keep, m)?
                } else {
                    compaction(order, floor, whole, &key_o, &keep, m)?
                };
                rewrote += w;
                order.dead -= d as u32;
            }
            let scratch = compact::sort_bytes(n);
            m.add(scratch + Run::bytes_for(n))?;
            if subjects {
                order.push_charged(compact::sorted(&all, &key_s), m)?;
            } else {
                order.push_charged(compact::sorted(&all, &key_o), m)?;
            }
            m.sub(scratch);
        }
        drop(all);
        m.sub(scratch);
        tracing::trace!(
            new = n,
            rewrote,
            rels_ms = (t_rels - t0).as_millis() as u64,
            whole_ms = t_rels.elapsed().as_millis() as u64,
            "index"
        );
        Ok(())
    }
}

/// Merge the runs the order's policy names below `floor`, or with `all` every run
/// below it, without dead facts. Returns the dead facts dropped and the entries read.
fn compaction(
    order: &mut Order,
    floor: u32,
    all: bool,
    key: &(impl Fn(u32) -> u64 + Sync),
    keep: &(impl Fn(u32) -> bool + Sync),
    m: &mut Meter<'_>,
) -> Result<(usize, usize), MemoryExceeded> {
    let Some(which) = order.compaction(floor, all) else {
        return Ok((0, 0));
    };
    let runs = &order.runs()[which.clone()];
    let total: usize = runs.iter().map(Run::len).sum();
    let old: usize = runs.iter().map(Run::heap_bytes).sum();
    let scratch = compact::merge_bytes(total);
    m.add(scratch + Run::bytes_for(total))?;
    let run = compact::merged(runs, key, keep);
    let dropped = total - run.len();
    order.replace(which, run);
    m.sub(scratch + old);
    Ok((dropped, total))
}

#[inline]
fn alive_in(dead: &[u64], i: usize) -> bool {
    dead.get(i / 64).is_none_or(|w| w & (1 << (i % 64)) == 0)
}

/// The words a bitmap of capacity `cap` grows to for word `w`.
fn bitmap_grown(cap: usize, w: usize) -> usize {
    (cap * 2).max(w + 1).max(4)
}

/// The allocation `bits` makes to hold bit `i`, while the old one is alive, and the old
/// one's bytes, released after: both zero when the capacity holds the bit.
fn bitmap_growth(bits: &Vec<u64>, i: usize) -> (usize, usize) {
    let (w, cap) = (i / 64, bits.capacity());
    if w < cap {
        (0, 0)
    } else {
        (
            bitmap_grown(cap, w) * size_of::<u64>(),
            cap * size_of::<u64>(),
        )
    }
}

fn set_bit(bits: &mut Vec<u64>, i: usize, on: bool) {
    let w = i / 64;
    if bits.len() <= w {
        let cap = bits.capacity();
        if w + 1 > cap {
            bits.reserve_exact(bitmap_grown(cap, w) - bits.len());
        }
        bits.resize(w + 1, 0);
    }
    if on {
        bits[w] |= 1 << (i % 64);
    } else {
        bits[w] &= !(1 << (i % 64));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Budget;

    fn tier(ts: &[Triple]) -> Tier {
        let mut t = Tier::new(0);
        for &x in ts {
            t.push(x);
        }
        let b = Budget::unlimited();
        let mut m = Meter::new(&b, true);
        t.index_new(0, &mut m).unwrap();
        t
    }

    fn ids(c: Cands<'_>) -> Vec<u32> {
        c.iter().collect()
    }

    #[test]
    fn probes_and_counts() {
        let mut t = tier(&[[1, 10, 2], [1, 10, 3], [2, 11, 3], [1, 12, 4], [5, 10, 2]]);
        assert_eq!(t.triple(3), [1, 12, 4]);
        assert_eq!(ids(t.cands(Some(1), Some(10), None, 0, 5)), [0, 1]);
        assert_eq!(ids(t.cands(Some(1), Some(10), None, 1, 5)), [1]);
        assert_eq!(ids(t.cands(None, Some(10), Some(2), 0, 5)), [0, 4]);
        assert_eq!(ids(t.cands(None, Some(10), Some(2), 0, 1)), [0]);
        assert_eq!(
            ids(t.cands(None, Some(10), Some(3), 0, 1)),
            Vec::<u32>::new()
        );
        assert_eq!(ids(t.cands(None, Some(11), None, 0, 5)), [2]);
        assert_eq!(t.count_p(10, 0, 5), 3);
        assert_eq!(t.count_p(10, 1, 4), 1);
        assert_eq!(t.pstats(10), (2, 2));
        assert_eq!(t.predicate_count(), 3);
        let mut since = Vec::new();
        t.predicates_since(3, |p| since.push(p));
        since.sort();
        assert_eq!(since, [10, 12]);
        // without an order by subject, a variable predicate scans the range
        assert_eq!(t.cands(Some(1), None, None, 0, 5).len(), 5);
        let b = Budget::unlimited();
        t.ensure_order(true, &mut Meter::new(&b, true)).unwrap();
        assert_eq!(ids(t.cands(Some(1), None, None, 0, 5)), [0, 1, 3]);
        assert_eq!(t.distinct_subjects(), Some(3));
        // more facts: a second run per order, and the order by subject follows
        t.push([1, 10, 1]);
        t.push([7, 13, 1]);
        t.index_new(5, &mut Meter::new(&b, true)).unwrap();
        assert_eq!(ids(t.cands(Some(1), Some(10), None, 0, 7)), [0, 1, 5]);
        assert_eq!(ids(t.cands(Some(1), None, None, 0, 7)), [0, 1, 3, 5]);
        assert_eq!(ids(t.cands(None, Some(13), None, 0, 7)), [6]);
        assert_eq!(t.count_p(10, 5, 7), 1);
    }

    #[test]
    fn dead_facts_are_dropped_by_compaction() {
        let mut t = tier(&[[1, 10, 2], [1, 10, 3], [2, 10, 4], [3, 10, 5]]);
        assert!(t.alive(1));
        t.kill(1);
        assert!(!t.alive(1) && t.alive(0));
        assert_eq!(t.dead_rows(), 1);
        assert_eq!(t.rels()[0].dead, 1);
        // a quarter dead: the next merge rebuilds the relation without the dead fact
        t.push([9, 10, 9]);
        let b = Budget::unlimited();
        t.index_new(4, &mut Meter::new(&b, true)).unwrap();
        assert_eq!(t.rels()[0].dead, 0);
        assert_eq!(t.rels()[0].subj.len(), 4);
        assert_eq!(ids(t.cands(Some(1), Some(10), None, 0, 5)), [0]);
        assert_eq!(t.count_p(10, 0, 5), 4);
        assert!(!t.alive(1));
    }

    #[test]
    fn flags_grow_as_charged() {
        let mut t = tier(&[[1, 10, 2]]);
        assert_eq!(t.flag_growth(0), (4 * 8, 0));
        assert_eq!(t.dead_growth(0), (4 * 8, 0));
        t.set_flag(0, true);
        assert!(t.flagged(0));
        assert_eq!(t.flag_growth(0), (0, 0));
        t.set_flag(0, false);
        assert!(!t.flagged(0));
    }

    #[test]
    fn cands_split_and_chunk() {
        let a: Vec<u32> = (0..10).collect();
        let mut c = Cands::range(0, 0);
        c.hi = 100;
        c.push_ids(&a, false);
        c.parts.push(Part::Range(50, 55));
        assert_eq!(c.len(), 15);
        let (h, t) = c.split_at(12);
        assert_eq!(ids(h), (0..10).chain(50..52).collect::<Vec<_>>());
        assert_eq!(ids(t), [52, 53, 54]);
        let chunks = c.chunks(4);
        assert_eq!(chunks.len(), 4);
        let all: Vec<u32> = chunks.iter().flat_map(|c| c.iter()).collect();
        assert_eq!(all, ids(c));
        assert_eq!(chunks[3].len(), 3);
    }
}
