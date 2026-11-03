//! Statistics of how the items of a node occur together, beyond the subject sets of
//! [`Stats::charsets`] (spec X15, correlation-aware estimation).
//!
//! The subject sets keep the [`MAX_CHARSETS`] sets of the most subjects. The subjects of
//! the other sets form the **remainder**, of which these statistics keep the number of
//! subjects and, per item, the subjects having it and its triples over them. A star's
//! rows over the remainder are then estimated with its items taken to be independent
//! within the remainder, and every triple of every item is accounted for.
//!
//! A **node set** is a characteristic set over both directions: the predicates a node is
//! the subject of (the predicate id, as in a subject set), the predicates it is the object
//! of (an [`in_item`]) and its classes (a [`class_item`]). Every node that is the subject
//! or the object of a quad has one, so the rows of a star of in-items and out-items on one
//! variable, such as an object join `?a p ?v . ?b q ?v` or a chain `?a p ?v . ?v q ?b`, can
//! be summed over the sets that hold it. They are counted after the permutations are
//! written, in one pass over SPO and OPS together, both sorted on the node. The same cap
//! and remainder apply to them. The sets kept do not depend on the thread count: past
//! [`MAX_NODE_SETS_SEEN`] distinct signatures a second pass counts those with the most
//! nodes by a sketch (see [`node_sets_seen`]).
//!
//! The member `correlations` of `stats.json` holds both, with a version of its own that
//! is read before anything else of it. A version this build does not read is refused:
//! the member is dropped with its reason, and the planner estimates as without it.
//! Releases before these statistics ignore the member.

use super::{CharSet, MAX_CHARSETS, class_item};
use crate::error::Result;
use crate::id::{self, Id, Tag};
use crate::index::{Perm, PermIndex};
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::de::{self, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::Path;

/// The version of `correlations` this build writes and reads (see
/// [`crate::formats::PLANNER_STATISTICS`]).
pub const VERSION: u32 = 1;

/// Node sets kept at most, those of the most nodes.
pub const MAX_NODE_SETS: usize = MAX_CHARSETS;

/// Distinct node sets counted while the pass runs, at most, over all its threads (the
/// default of [`super::BuildOptions::node_sets_seen`]). Past it the pass keeps the
/// best-ranked signatures (see [`Admitted`]), and the nodes of the others go to the
/// remainder with their items.
pub const MAX_NODE_SETS_SEEN: usize = 1 << 20;

/// The tag of an in-item: no term has it, and it sorts after every predicate and before
/// the classes.
const IN_TAG: u64 = 0xE;

/// The item standing for "is the object of predicate `p`" (a base vocabulary id) in a node
/// set.
pub fn in_item(p: u64) -> Option<u64> {
    (Id(p).tag() == Tag::Vocab).then_some(IN_TAG << id::PAYLOAD_BITS | (p & id::PAYLOAD_MASK))
}

/// Whether `item` is an in-item.
pub fn is_in_item(item: u64) -> bool {
    item >> id::PAYLOAD_BITS == IN_TAG
}

/// The holders (subjects or nodes) of the sets not kept, with per item the holders having
/// it and its triples over them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rest {
    pub n: u64,
    /// (item, holders, triples), sorted by item, without zero holders
    pub items: Vec<(u64, u64, u64)>,
}

impl Rest {
    /// The holders having `item` and its triples over them.
    pub fn item(&self, item: u64) -> Option<(u64, u64)> {
        self.items
            .binary_search_by_key(&item, |x| x.0)
            .ok()
            .map(|i| (self.items[i].1, self.items[i].2))
    }

    /// Bytes this holds beyond its own size.
    pub fn heap_bytes(&self) -> u64 {
        self.items.capacity() as u64 * 24
    }
}

/// A remainder while it is counted or changed: per item (holders, triples).
#[derive(Default)]
pub(crate) struct RestTally {
    pub n: i64,
    pub items: FxHashMap<u64, (i64, i64)>,
}

impl RestTally {
    pub fn of(r: &Rest) -> RestTally {
        RestTally {
            n: r.n as i64,
            items: r
                .items
                .iter()
                .map(|&(i, h, t)| (i, (h as i64, t as i64)))
                .collect(),
        }
    }

    /// Add `holders` holders of the set `items`, with `triples` per item over them, or
    /// take them out with `sign` -1.
    pub fn add(&mut self, holders: u64, items: &[u64], triples: &[u64], sign: i64) {
        self.n += sign * holders as i64;
        for (&i, &t) in items.iter().zip(triples) {
            let e = self.items.entry(i).or_default();
            e.0 += sign * holders as i64;
            e.1 += sign * t as i64;
        }
    }

    pub fn merge(&mut self, o: RestTally) {
        self.n += o.n;
        for (i, (h, t)) in o.items {
            let e = self.items.entry(i).or_default();
            e.0 += h;
            e.1 += t;
        }
    }

    pub fn finish(self) -> Rest {
        let mut items: Vec<(u64, u64, u64)> = self
            .items
            .into_iter()
            .filter(|&(_, (h, _))| h > 0)
            .map(|(i, (h, t))| (i, h as u64, t.max(0) as u64))
            .collect();
        items.sort_unstable_by_key(|x| x.0);
        Rest {
            n: self.n.max(0) as u64,
            items,
        }
    }
}

/// Sets counted by their items: holders and triples per item.
pub(crate) type SetMap = FxHashMap<Box<[u64]>, (u64, Vec<u64>)>;

/// The sets of `sets` with the most holders, at most `cap`, most first; the others are
/// added to `rest`.
pub(crate) fn keep_top(sets: SetMap, cap: usize, rest: &mut RestTally) -> Vec<CharSet> {
    let mut v: Vec<CharSet> = sets
        .into_iter()
        .map(|(preds, (subjects, triples))| CharSet {
            preds: preds.into_vec(),
            subjects,
            triples,
        })
        .collect();
    v.sort_unstable_by(|a, b| {
        b.subjects
            .cmp(&a.subjects)
            .then_with(|| a.preds.cmp(&b.preds))
    });
    for c in v.iter().skip(cap) {
        rest.add(c.subjects, &c.preds, &c.triples, 1);
    }
    v.truncate(cap);
    v
}

/// The statistics of [`Stats::correlations`](super::Stats::correlations).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Correlations {
    /// [`VERSION`]; written first
    pub version: u32,
    /// the subjects whose characteristic sets are not in `Stats::charsets`
    pub subject_rest: Rest,
    /// the node sets, most nodes first (the `subjects` of each counts its nodes)
    pub node_sets: Vec<CharSet>,
    /// the nodes whose node sets are not in `node_sets`
    pub node_rest: Rest,
}

impl Correlations {
    /// Bytes this holds beyond its own size.
    pub fn heap_bytes(&self) -> u64 {
        self.subject_rest.heap_bytes()
            + self.node_rest.heap_bytes()
            + self.node_sets.capacity() as u64 * std::mem::size_of::<CharSet>() as u64
            + self
                .node_sets
                .iter()
                .map(|s| (s.preds.capacity() + s.triples.capacity()) as u64 * 8)
                .sum::<u64>()
    }
}

/// `stats.json`'s `correlations`: absent (statistics written before it, or a build that
/// leaves it out), present, or refused because of a version this build does not read.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum CorrelationSlot {
    #[default]
    Absent,
    Present(Box<Correlations>),
    /// the version found, or 0 for a member without one
    Refused(u32),
}

impl CorrelationSlot {
    pub fn is_absent(&self) -> bool {
        matches!(self, CorrelationSlot::Absent)
    }

    /// Whether writing the statistics leaves the member out: when it is absent, or
    /// refused, so that the next full build writes it again.
    pub fn is_unwritten(&self) -> bool {
        !matches!(self, CorrelationSlot::Present(_))
    }

    /// The statistics, when this build reads them.
    pub fn get(&self) -> Option<&Correlations> {
        match self {
            CorrelationSlot::Present(c) => Some(c),
            _ => None,
        }
    }

    pub fn get_mut(&mut self) -> Option<&mut Correlations> {
        match self {
            CorrelationSlot::Present(c) => Some(c),
            _ => None,
        }
    }

    /// Why the statistics are not used, when a stats file holds them in a form this build
    /// does not read.
    pub fn refusal(&self) -> Option<String> {
        match self {
            CorrelationSlot::Refused(0) => Some(format!(
                "stats.json: correlations have no version; Sparkles {} reads version {VERSION}",
                crate::formats::SPARKLES_VERSION
            )),
            CorrelationSlot::Refused(v) => Some(format!(
                "stats.json: correlations have version {v}; Sparkles {} reads version {VERSION}, \
                 so the planner does without them until the next full compaction writes them again",
                crate::formats::SPARKLES_VERSION
            )),
            _ => None,
        }
    }

    pub fn heap_bytes(&self) -> u64 {
        match self {
            CorrelationSlot::Present(c) => {
                std::mem::size_of::<Correlations>() as u64 + c.heap_bytes()
            }
            _ => 0,
        }
    }
}

impl Serialize for CorrelationSlot {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            CorrelationSlot::Present(c) => c.serialize(s),
            // a refused or absent member is not written again
            _ => s.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for CorrelationSlot {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        d.deserialize_map(SlotVisitor)
    }
}

struct SlotVisitor;

impl<'de> Visitor<'de> for SlotVisitor {
    type Value = CorrelationSlot;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("the correlations of the planner statistics")
    }

    fn visit_unit<E: de::Error>(self) -> std::result::Result<CorrelationSlot, E> {
        Ok(CorrelationSlot::Absent)
    }

    fn visit_map<A: MapAccess<'de>>(
        self,
        mut m: A,
    ) -> std::result::Result<CorrelationSlot, A::Error> {
        // the version comes first in files this build writes; members before it are
        // kept as values until it is known
        let mut version: Option<u32> = None;
        let mut early: Vec<(String, serde_json::Value)> = Vec::new();
        let mut c = Correlations::default();
        while let Some(k) = m.next_key::<String>()? {
            if k == "version" {
                let v: u32 = m.next_value()?;
                if v != VERSION {
                    while m.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                    return Ok(CorrelationSlot::Refused(v));
                }
                version = Some(v);
                for (k, value) in early.drain(..) {
                    set_member(&mut c, &k, value).map_err(de::Error::custom)?;
                }
                continue;
            }
            if version.is_none() {
                early.push((k, m.next_value()?));
                continue;
            }
            match k.as_str() {
                "subjectRest" => c.subject_rest = m.next_value()?,
                "nodeSets" => c.node_sets = m.next_value()?,
                "nodeRest" => c.node_rest = m.next_value()?,
                // members added later within this version are additive
                _ => {
                    m.next_value::<IgnoredAny>()?;
                }
            }
        }
        match version {
            Some(v) => {
                c.version = v;
                Ok(CorrelationSlot::Present(Box::new(c)))
            }
            None => Ok(CorrelationSlot::Refused(0)),
        }
    }
}

fn set_member(c: &mut Correlations, k: &str, v: serde_json::Value) -> serde_json::Result<()> {
    match k {
        "subjectRest" => c.subject_rest = serde_json::from_value(v)?,
        "nodeSets" => c.node_sets = serde_json::from_value(v)?,
        "nodeRest" => c.node_rest = serde_json::from_value(v)?,
        _ => {}
    }
    Ok(())
}

/// The node set of one node from its rows: `out` its SPO rows as (predicate, object),
/// `inn` the predicates of its OPS rows with a count of rows each, both in index order.
/// Returns the items with the triples of each.
pub(crate) fn node_set(
    out: impl Iterator<Item = (u64, u64)>,
    inn: impl Iterator<Item = (u64, u64)>,
    rdf_type: Option<u64>,
    items: &mut Vec<u64>,
    counts: &mut Vec<u64>,
) {
    let mut t = NodeTally::default();
    for (p, o) in out {
        t.out(p, o, rdf_type);
    }
    for (p, n) in inn {
        t.inn(p, n);
    }
    t.finish(items, counts);
}

/// The items of one node counted row by row, so that a node with many rows holds one
/// count per item and not its rows: its outgoing rows in SPO order, then its incoming
/// rows in OPS order.
#[derive(Default)]
struct NodeTally {
    items: Vec<u64>,
    counts: Vec<u64>,
    classes: Vec<(u64, u64)>,
}

impl NodeTally {
    fn clear(&mut self) {
        self.items.clear();
        self.counts.clear();
        self.classes.clear();
    }

    fn bump(&mut self, item: u64, n: u64) {
        match self.items.last() {
            Some(&x) if x == item => *self.counts.last_mut().unwrap() += n,
            _ => {
                self.items.push(item);
                self.counts.push(n);
            }
        }
    }

    fn out(&mut self, p: u64, o: u64, rdf_type: Option<u64>) {
        self.bump(p, 1);
        if Some(p) == rdf_type
            && let Some(c) = class_item(o)
        {
            match self.classes.last_mut() {
                Some((x, n)) if *x == c => *n += 1,
                _ => self.classes.push((c, 1)),
            }
        }
    }

    fn inn(&mut self, p: u64, n: u64) {
        if let Some(i) = in_item(p) {
            self.bump(i, n);
        }
    }

    /// The items, sorted (predicates, then incoming items, then classes), with the
    /// triples of each.
    fn finish(&mut self, items: &mut Vec<u64>, counts: &mut Vec<u64>) {
        items.clear();
        counts.clear();
        items.extend_from_slice(&self.items);
        counts.extend_from_slice(&self.counts);
        for &(c, n) in &self.classes {
            items.push(c);
            counts.push(n);
        }
        self.clear();
    }
}

/// What one piece of the node pass counted.
#[derive(Default)]
struct Piece {
    sets: SetMap,
    rest: RestTally,
}

/// Count the node sets of the generation in `dir`, whose SPO and OPS are written, on
/// `threads` threads, with at most `seen` distinct sets counted while a pass runs.
///
/// The result does not depend on `threads` or on the order the threads meet the nodes.
/// When the data has at most `seen` distinct signatures, one pass counts them all
/// exactly. Otherwise the first pass also fills a count-min sketch of the nodes per
/// signature, and a second pass counts exactly the `seen` signatures with the most nodes
/// by the sketch, ties broken by a stable hash of the signature. Either way every kept
/// signature holds all its nodes and the remainder all the others.
pub(crate) fn node_sets_seen(
    dir: &Path,
    rdf_type: Option<u64>,
    threads: usize,
    seen: usize,
    interrupted: &(dyn Fn() -> Result<()> + Sync),
) -> Result<(Vec<CharSet>, Rest)> {
    let spo = PermIndex::open(dir, Perm::Spo)?;
    let ops = PermIndex::open(dir, Perm::Ops)?;
    // pieces of the node ids, split at block boundaries of both permutations
    let mut cuts: Vec<u64> = spo
        .blocks
        .iter()
        .chain(ops.blocks.iter())
        .map(|m| m.first[0])
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let pieces = (threads.max(1) * 8).min(cuts.len().max(1));
    let bounds: Vec<u64> = (0..pieces)
        .map(|i| cuts.get(i * cuts.len() / pieces).copied().unwrap_or(0))
        .collect();
    let ranges: Vec<(u64, Option<u64>)> = (0..bounds.len())
        .map(|i| {
            let lo = if i == 0 { 0 } else { bounds[i] };
            (lo, bounds.get(i + 1).copied())
        })
        .filter(|(lo, hi)| hi.is_none_or(|h| h > *lo))
        .collect();
    // a sketch of about one counter per four rows of either permutation, up to 2^20 a row
    let rows: u64 = spo
        .blocks
        .iter()
        .chain(ops.blocks.iter())
        .map(|m| m.rows as u64)
        .sum();
    let sketch = Sketch::new(((rows / 4) as usize).clamp(1 << 10, 1 << 20));
    let first = count_pass(
        &spo,
        &ops,
        &ranges,
        threads,
        rdf_type,
        seen,
        Rank::Hash,
        Some(&sketch),
        interrupted,
    )?;
    let (sets, rest) = match first {
        Ok(done) => done,
        // more signatures than `seen`: count again, the best-ranked by the sketch
        Err(()) => match count_pass(
            &spo,
            &ops,
            &ranges,
            threads,
            rdf_type,
            seen,
            Rank::Sketch(&sketch),
            None,
            interrupted,
        )? {
            Ok(done) => done,
            Err(()) => unreachable!("a pass without a sketch to fill finishes"),
        },
    };
    let mut rest = rest;
    let kept = keep_top(sets, MAX_NODE_SETS, &mut rest);
    Ok((kept, rest.finish()))
}

/// One pass of the node count over `ranges` on `threads` threads, admitting signatures by
/// `rank` and adding every node's signature to `fill`. `Err(())` when the data had more
/// than `seen` distinct signatures and `fill` was given, so that a second pass ranks them
/// by the filled sketch; the result of the pass otherwise, which is exact for the
/// signatures it admits whatever the order of the nodes.
#[allow(clippy::too_many_arguments)]
fn count_pass(
    spo: &PermIndex,
    ops: &PermIndex,
    ranges: &[(u64, Option<u64>)],
    threads: usize,
    rdf_type: Option<u64>,
    seen: usize,
    rank: Rank<'_>,
    fill: Option<&Sketch>,
    interrupted: &(dyn Fn() -> Result<()> + Sync),
) -> Result<std::result::Result<(SetMap, RestTally), ()>> {
    let admitted = Admitted {
        state: Mutex::new(AdmitState::default()),
        cap: seen,
        rank,
        overflowed: std::sync::atomic::AtomicBool::new(false),
    };
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Vec<Result<Piece>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads.max(1).min(ranges.len().max(1)))
            .map(|_| {
                s.spawn(|| -> Result<Piece> {
                    let mut acc = Piece::default();
                    let mut worst = None;
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(&(lo, hi)) = ranges.get(i) else {
                            return Ok(acc);
                        };
                        interrupted()?;
                        count_piece(
                            spo, ops, lo, hi, rdf_type, &admitted, fill, &mut worst, &mut acc,
                        )?;
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join().unwrap_or_else(|_| {
                    Err(crate::error::Error::Corrupt(
                        "node set pass panicked".into(),
                    ))
                })
            })
            .collect()
    });
    let overflowed = admitted
        .overflowed
        .load(std::sync::atomic::Ordering::Relaxed);
    if overflowed && fill.is_some() {
        // the results are dropped; a pass ranked by the sketch follows
        for r in results {
            r?;
        }
        return Ok(Err(()));
    }
    // the signatures admitted at the end are the `seen` best-ranked of the data, each
    // with all its nodes; a piece's sets admitted earlier and pushed out later go to the
    // remainder with the nodes that went there directly
    let state = admitted.state.into_inner();
    let mut sets = SetMap::default();
    let mut rest = RestTally::default();
    for r in results {
        let p = r?;
        rest.merge(p.rest);
        for (k, (n, t)) in p.sets {
            if !state.sigs.contains(&k) {
                rest.add(n, &k, &t, 1);
                continue;
            }
            match sets.get_mut(&k) {
                Some((m, u)) => {
                    *m += n;
                    for (x, y) in u.iter_mut().zip(&t) {
                        *x += y;
                    }
                }
                None => {
                    sets.insert(k, (n, t));
                }
            }
        }
    }
    Ok(Ok((sets, rest)))
}

/// The blocks of `perm` that hold keys whose first column is in `[lo, hi)`.
fn blocks_of(perm: &PermIndex, lo: u64, hi: Option<u64>) -> std::ops::Range<usize> {
    let b0 = perm.blocks.partition_point(|m| m.last[0] < lo);
    let b1 = match hi {
        Some(h) => perm.blocks.partition_point(|m| m.first[0] < h),
        None => perm.blocks.len(),
    };
    b0..b1.max(b0)
}

/// Rows of one permutation, read block by block, with the columns `cols` decoded.
struct Rows<'a> {
    perm: &'a PermIndex,
    blocks: std::ops::Range<usize>,
    cols: &'a [usize],
    cur: Vec<crate::index::Col>,
    i: usize,
    n: usize,
}

impl<'a> Rows<'a> {
    fn new(perm: &'a PermIndex, blocks: std::ops::Range<usize>, cols: &'a [usize]) -> Rows<'a> {
        Rows {
            perm,
            blocks,
            cols,
            cur: Vec::new(),
            i: 0,
            n: 0,
        }
    }

    /// The column values of the next row, or `None` at the end.
    fn next(&mut self, out: &mut [u64; 3]) -> Result<bool> {
        while self.i >= self.n {
            let Some(b) = self.blocks.next() else {
                return Ok(false);
            };
            self.cur.clear();
            for &c in self.cols {
                self.cur.push(self.perm.decode_col(b, c)?);
            }
            self.i = 0;
            self.n = self.perm.blocks[b].rows as usize;
        }
        for (k, c) in self.cur.iter().enumerate() {
            out[k] = c[self.i];
        }
        self.i += 1;
        Ok(true)
    }
}

/// Count the node sets of the nodes in `[lo, hi)` into `acc`, reading their outgoing
/// rows from SPO and their incoming rows from OPS together, in node order.
#[allow(clippy::too_many_arguments)]
fn count_piece(
    spo: &PermIndex,
    ops: &PermIndex,
    lo: u64,
    hi: Option<u64>,
    rdf_type: Option<u64>,
    admitted: &Admitted,
    fill: Option<&Sketch>,
    worst: &mut Option<Ranked>,
    acc: &mut Piece,
) -> Result<()> {
    let inside = |x: u64| x >= lo && hi.is_none_or(|h| x < h);
    let mut s = Rows::new(spo, blocks_of(spo, lo, hi), &[0, 1, 2]);
    let mut o = Rows::new(ops, blocks_of(ops, lo, hi), &[0, 1]);
    let (mut sr, mut or) = ([0u64; 3], [0u64; 3]);
    // the next row of each in the piece's range, skipping those before it
    let adv = |r: &mut Rows, k: &mut [u64; 3]| -> Result<bool> {
        loop {
            if !r.next(k)? {
                return Ok(false);
            }
            if k[0] < lo {
                continue;
            }
            return Ok(inside(k[0]));
        }
    };
    let mut s_ok = adv(&mut s, &mut sr)?;
    let mut o_ok = adv(&mut o, &mut or)?;
    let mut tally = NodeTally::default();
    let (mut items, mut counts) = (Vec::new(), Vec::new());
    while s_ok || o_ok {
        let node = match (s_ok, o_ok) {
            (true, true) => sr[0].min(or[0]),
            (true, false) => sr[0],
            _ => or[0],
        };
        while s_ok && sr[0] == node {
            tally.out(sr[1], sr[2], rdf_type);
            s_ok = adv(&mut s, &mut sr)?;
        }
        while o_ok && or[0] == node {
            tally.inn(or[1], 1);
            o_ok = adv(&mut o, &mut or)?;
        }
        tally.finish(&mut items, &mut counts);
        if items.is_empty() {
            continue;
        }
        if let Some(f) = fill {
            f.add(sig_hash(&items));
        }
        tally_node(admitted, worst, acc, &items, &counts);
    }
    Ok(())
}

/// A stable 64-bit hash of a signature, the same in every build and on every host.
fn sig_hash(items: &[u64]) -> u64 {
    // splitmix64's finalizer over a running sum, item by item
    let mix = |mut z: u64| {
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    items.iter().fold(items.len() as u64, |h, &x| {
        mix(h.wrapping_add(0x9e37_79b9_7f4a_7c15).wrapping_add(x) ^ h.rotate_left(17))
    })
}

/// A count-min sketch of the nodes per signature: four rows of `width` counters, each
/// signature counted in one counter of each row and estimated by the smallest. Its counts
/// are sums, the same whatever the order the threads add them in.
struct Sketch {
    width: usize,
    counts: Vec<std::sync::atomic::AtomicU64>,
}

impl Sketch {
    const ROWS: usize = 4;

    fn new(width: usize) -> Sketch {
        let width = width.next_power_of_two();
        Sketch {
            width,
            counts: (0..width * Self::ROWS)
                .map(|_| std::sync::atomic::AtomicU64::new(0))
                .collect(),
        }
    }

    fn slot(&self, h: u64, row: usize) -> usize {
        // double hashing: the low and high halves of the hash
        let step = (h >> 32) | 1;
        row * self.width
            + ((h as u32 as u64).wrapping_add(row as u64 * step) as usize & (self.width - 1))
    }

    fn add(&self, h: u64) {
        for row in 0..Self::ROWS {
            self.counts[self.slot(h, row)].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn estimate(&self, h: u64) -> u64 {
        (0..Self::ROWS)
            .map(|row| self.counts[self.slot(h, row)].load(std::sync::atomic::Ordering::Relaxed))
            .min()
            .unwrap_or(0)
    }
}

/// How a pass ranks the signatures it meets once more than its cap of them are counted:
/// by their hash alone, or by their nodes in a filled sketch, most first, ties broken by
/// the hash.
#[derive(Clone, Copy)]
enum Rank<'a> {
    Hash,
    Sketch(&'a Sketch),
}

/// A signature with its rank: smaller ranks better, and the signature itself breaks a tie
/// of hashes, so that the order is total.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Ranked {
    fewer: u64,
    hash: u64,
    sig: Box<[u64]>,
}

impl Rank<'_> {
    fn of(&self, items: &[u64]) -> (u64, u64) {
        let hash = sig_hash(items);
        let fewer = match self {
            Rank::Hash => 0,
            Rank::Sketch(s) => u64::MAX - s.estimate(hash),
        };
        (fewer, hash)
    }
}

/// The signatures the node pass counts as sets, shared by its threads: the `cap`
/// best-ranked of those met so far. A signature met while fewer than `cap` are admitted
/// is admitted, and once `cap` are, it is admitted only by pushing out the worst-ranked.
/// The worst admitted rank only improves, so a signature turned away or pushed out never
/// returns. A signature among the `cap` best-ranked of the whole data is therefore
/// admitted the first time any thread meets it and never pushed out, and holds all its
/// nodes, whichever order the threads meet them in. A partial compaction relies on that
/// when it removes a node's old signature.
struct Admitted<'a> {
    state: Mutex<AdmitState>,
    cap: usize,
    rank: Rank<'a>,
    /// whether a signature was turned away or pushed out
    overflowed: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct AdmitState {
    sigs: FxHashSet<Box<[u64]>>,
    /// the admitted signatures by rank, the worst on top
    ranked: std::collections::BinaryHeap<Ranked>,
}

impl Admitted<'_> {
    /// Whether `items` is admitted now. `worst` caches the worst admitted rank a thread
    /// last saw once `cap` are admitted: a signature ranked below it is turned away
    /// without the lock, since that rank only improves.
    fn admit(&self, items: &[u64], worst: &mut Option<Ranked>) -> bool {
        let (fewer, hash) = self.rank.of(items);
        let below = |w: &Ranked| (fewer, hash, items) > (w.fewer, w.hash, &w.sig[..]);
        if worst.as_ref().is_some_and(below) {
            self.overflowed
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return false;
        }
        let mut st = self.state.lock();
        if st.sigs.contains(items) {
            return true;
        }
        let me = Ranked {
            fewer,
            hash,
            sig: items.to_vec().into_boxed_slice(),
        };
        let admit = if st.sigs.len() < self.cap {
            true
        } else {
            self.overflowed
                .store(true, std::sync::atomic::Ordering::Relaxed);
            match st.ranked.peek() {
                Some(w) if me < *w => {
                    let out = st.ranked.pop().expect("a worst signature");
                    st.sigs.remove(&out.sig);
                    true
                }
                _ => false,
            }
        };
        if admit {
            st.sigs.insert(me.sig.clone());
            st.ranked.push(me);
        }
        if st.sigs.len() >= self.cap {
            *worst = st.ranked.peek().cloned();
        }
        admit
    }

    /// Move the sets of `acc` that were pushed out since the thread admitted them into its
    /// remainder. They never return, so a thread's sets stay within twice the cap however
    /// often the admitted signatures change.
    fn prune(&self, acc: &mut Piece) {
        let st = self.state.lock();
        let rest = &mut acc.rest;
        acc.sets.retain(|k, (n, t)| {
            let keep = st.sigs.contains(k);
            if !keep {
                rest.add(*n, k, t, 1);
            }
            keep
        });
    }
}

/// Count one node of signature `items`, with `counts` triples per item, into `acc`: into
/// its set when the signature is admitted, and into the remainder otherwise.
fn tally_node(
    admitted: &Admitted,
    worst: &mut Option<Ranked>,
    acc: &mut Piece,
    items: &[u64],
    counts: &[u64],
) {
    if let Some((n, t)) = acc.sets.get_mut(items) {
        *n += 1;
        for (x, c) in t.iter_mut().zip(counts) {
            *x += c;
        }
        return;
    }
    if !admitted.admit(items, worst) {
        acc.rest.add(1, items, counts, 1);
        return;
    }
    acc.sets
        .insert(items.to_vec().into_boxed_slice(), (1, counts.to_vec()));
    if acc.sets.len() >= admitted.cap.saturating_mul(2).max(2) {
        admitted.prune(acc);
    }
}

/// What is wrong with the shape of `c` among the statistics `s`, for `sparkles check`.
pub fn check(s: &super::Stats, c: &Correlations) -> Option<String> {
    let sets = |name: &str, v: &[CharSet]| -> Option<String> {
        for (i, set) in v.iter().enumerate() {
            if set.preds.len() != set.triples.len()
                || set.preds.windows(2).any(|w| w[0] >= w[1])
                || set.subjects == 0
                || set.triples.iter().any(|&t| t < set.subjects)
            {
                return Some(format!("{name} set {i} is malformed"));
            }
        }
        None
    };
    let rest = |name: &str, r: &Rest| -> Option<String> {
        if r.items.windows(2).any(|w| w[0].0 >= w[1].0)
            || r.items.iter().any(|&(_, h, t)| h == 0 || h > r.n || t < h)
        {
            return Some(format!("the {name} remainder is malformed"));
        }
        None
    };
    sets("node", &c.node_sets)
        .or_else(|| rest("subject", &c.subject_rest))
        .or_else(|| rest("node", &c.node_rest))
        .or_else(|| {
            (c.subject_rest.n != s.charset_others).then(|| {
                format!(
                    "the subject remainder holds {} subjects, the statistics count {} outside the kept sets",
                    c.subject_rest.n, s.charset_others
                )
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admitted(cap: usize) -> Admitted<'static> {
        Admitted {
            state: Mutex::new(AdmitState::default()),
            cap,
            rank: Rank::Hash,
            overflowed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Distinct signatures, best-ranked by hash first.
    fn by_rank(n: u64) -> Vec<Vec<u64>> {
        let mut v: Vec<Vec<u64>> = (0..n).map(|i| vec![i + 1]).collect();
        v.sort_by_key(|s| sig_hash(s));
        v
    }

    #[test]
    fn a_signature_turned_away_without_the_lock_is_an_overflow() {
        let sigs = by_rank(2);
        // the better-ranked one first: the other is turned away by the thread's cached
        // worst rank, without the lock, and the pass must still see an overflow
        for order in [[0, 1], [1, 0]] {
            let a = admitted(1);
            let (mut worst, mut acc) = (None, Piece::default());
            for &i in &order {
                tally_node(&a, &mut worst, &mut acc, &sigs[i], &[1]);
            }
            assert!(
                a.overflowed.load(std::sync::atomic::Ordering::Relaxed),
                "order {order:?}"
            );
            assert_eq!(acc.sets.len() as i64 + acc.rest.n, 2);
            let kept: Vec<_> = a.state.lock().sigs.iter().cloned().collect();
            assert_eq!(kept, vec![sigs[0].clone().into_boxed_slice()]);
        }
    }

    #[test]
    fn pushed_out_signatures_leave_the_thread_sets() {
        // every signature ranks better than those before it, so each pushes one out
        let sigs = by_rank(10_000);
        let a = admitted(4);
        let (mut worst, mut acc) = (None, Piece::default());
        for s in sigs.iter().rev() {
            tally_node(&a, &mut worst, &mut acc, s, &[2]);
            assert!(acc.sets.len() <= 8, "{} sets held", acc.sets.len());
        }
        a.prune(&mut acc);
        let st = a.state.lock();
        assert_eq!(st.sigs.len(), 4);
        assert!(acc.sets.keys().all(|k| st.sigs.contains(k)));
        assert_eq!(acc.sets.len(), 4);
        assert_eq!(acc.rest.n, 10_000 - 4);
        let held: u64 = acc.sets.values().map(|(n, _)| n).sum();
        assert_eq!(held, 4);
        for s in &sigs[..4] {
            assert!(st.sigs.contains(&s[..]));
        }
    }
}
