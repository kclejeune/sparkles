//! Reports measured from the index statistics ([`Basis::Statistics`]), and the objects of
//! one predicate measured on demand ([`predicate_objects`]).
//!
//! A full report reads every quad of the selection twice, which takes minutes and
//! gigabytes on a store of a billion quads. The index build already counts the quads,
//! distinct subjects and distinct objects of each predicate and the instances of each
//! class. [`crate::sparql::stats::exact_counts`] corrects those counts exactly for the
//! base quads of graphs the selection leaves out and for the delta, with one index probe
//! per value they hold, as it does for the planner. A report built from them reads no
//! index rows for the observed layer when the store has no delta and the selection holds
//! every graph. Its declared layer is read as a full report reads it.
//!
//! The kinds of a predicate's objects are not in the statistics. [`predicate_objects`]
//! measures them for one predicate: exactly, with the same pass a full report makes, when
//! the predicate has at most `max_rows` quads, and otherwise from counts of the id ranges
//! that hold each kind of term, less the predicate's quads in graphs the selection leaves
//! out, and a sample of the literals the vocabulary stores.

use super::{
    Basis, Budget, GraphFilter, Kind, KindCount, ObjectKinds, Observed, PredAcc, SchemaError,
    SchemaOptions, Selected, in_phase, is_iri, kind_of_key, literal_suffix, object_pass,
};
use crate::id::{Id, PAYLOAD_MASK, Tag};
use crate::index::{Key, Perm};
use crate::sparql::plan::GraphFilter as Graphs;
use crate::sparql::stats::{CountKey, Measure, exact_counts};
use crate::store::{Chunk, Delta, Snapshot};
use rustc_hash::FxHashMap;
use std::collections::BTreeMap;
use std::sync::Arc;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// The most base quads of graphs outside the selection that a report from the statistics
/// takes out. Each costs a probe of the index for every distinct value it holds, so a
/// selection that leaves out more is scanned instead, and a small selection of a large
/// store is one.
pub(super) const UNREAD_MAX: u64 = 1_000_000;

/// Whether a report of `opts` over `snap` is measured from the statistics.
pub(super) fn applies(snap: &Snapshot, opts: &SchemaOptions, sel: &Selected) -> bool {
    let Some(above) = opts.statistics_above else {
        return false;
    };
    if snap.len() <= above || opts.subject_classes || opts.term_totals {
        return false;
    }
    sampleable(snap, opts, &sel.observed)
}

/// Whether counts over every graph, less the quads of the graphs `f` leaves out, measure
/// the selection: the caller reads every graph, the build kept its statistics, and the
/// graphs left out hold at most [`UNREAD_MAX`] base quads.
fn sampleable(snap: &Snapshot, opts: &SchemaOptions, f: &GraphFilter) -> bool {
    opts.graphs.as_ref().is_none_or(|a| a.reads_all())
        && has_statistics(snap)
        && unread_quads(snap, f) <= UNREAD_MAX
}

/// Whether the base index has the statistics a report reads: those of its predicates
/// and, when it has `rdf:type` quads, those of its classes.
fn has_statistics(snap: &Snapshot) -> bool {
    let generation = &snap.generation;
    let stats = &generation.stats;
    if generation.meta.quads == 0 {
        return true;
    }
    if stats.predicates.is_empty() {
        return false;
    }
    match snap.lookup_iri(RDF_TYPE) {
        Some(t) if stats.predicate(t.0).is_some_and(|s| s.count > 0) => !stats.classes.is_empty(),
        _ => true,
    }
}

/// The quads of the base in graphs that `f` leaves out.
fn unread_quads(snap: &Snapshot, f: &GraphFilter) -> u64 {
    snap.generation
        .stats
        .graphs
        .iter()
        .filter(|(g, _)| !f.accepts(*g))
        .map(|(_, n)| n)
        .sum()
}

/// Every graph of the base and of the delta's inserted quads, in id order.
fn every_graph(snap: &Snapshot) -> Vec<u64> {
    let mut out: Vec<u64> = snap.generation.stats.graphs.iter().map(|g| g.0).collect();
    let ins = &snap.delta.ins[Perm::Gspo.index()];
    let mut from = 0u64;
    while let Some(k) = Delta::key_range(ins, [from, 0, 0, 0], [u64::MAX; 4]).next() {
        out.push(k[0]);
        match k[0].checked_add(1) {
            Some(n) => from = n,
            None => break,
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The planner's graph filter for a selection's.
fn graphs_of(snap: &Snapshot, f: &GraphFilter) -> Graphs {
    match f {
        GraphFilter::All => Graphs::All,
        GraphFilter::Set(s) => {
            let mut v = s.clone();
            v.sort_unstable();
            v.dedup();
            Graphs::Set(v)
        }
        GraphFilter::AllExcept(x) => {
            Graphs::Set(every_graph(snap).into_iter().filter(|g| g != x).collect())
        }
    }
}

/// The observed layer from the statistics of the base index, corrected exactly for the
/// graphs the selection leaves out and for the delta (see [`crate::sparql::stats`]), or
/// `None` when that correction would cost more than reading the store.
pub(super) fn observed(
    snap: &Snapshot,
    f: &GraphFilter,
    budget: &Budget,
) -> crate::Result<Option<Observed>> {
    budget.check()?;
    let graphs = graphs_of(snap, f);
    let rdf_type = snap.lookup_iri(RDF_TYPE).map(|i| i.0);
    let check = || budget.check();
    // the work a full report does: two passes over the quads
    let work = snap.len().saturating_mul(2);
    let count = |perm: Perm, prefix: Vec<u64>, measure: Measure| {
        let key = CountKey {
            perm,
            prefix,
            grouped: true,
            measure,
            graphs: graphs.clone(),
        };
        exact_counts(snap, &key, rdf_type, work, true, &check)
    };
    let Some(rows) = count(Perm::Pso, Vec::new(), Measure::Rows)? else {
        return Ok(None);
    };
    let Some(subjects) = count(Perm::Pso, Vec::new(), Measure::Distinct(2))? else {
        return Ok(None);
    };
    let Some(objects) = count(Perm::Pos, Vec::new(), Measure::Distinct(2))? else {
        return Ok(None);
    };
    let mut preds: FxHashMap<u64, PredAcc> = FxHashMap::default();
    for &(p, n) in &rows.counts {
        preds.insert(
            p,
            PredAcc {
                triples: n,
                ..Default::default()
            },
        );
    }
    for &(p, n) in &subjects.counts {
        if let Some(a) = preds.get_mut(&p) {
            a.distinct_subjects = n;
        }
    }
    for &(p, n) in &objects.counts {
        if let Some(a) = preds.get_mut(&p) {
            a.distinct_objects = n;
        }
    }
    budget.check()?;
    // classes: the distinct (class, instance) pairs of `rdf:type`
    let mut class_instances: FxHashMap<u64, u64> = FxHashMap::default();
    if let Some(t) = rdf_type.filter(|t| preds.contains_key(t)) {
        let Some(classes) = count(Perm::Pos, vec![t], Measure::Distinct(3))? else {
            return Ok(None);
        };
        for &(c, n) in &classes.counts {
            if is_iri(snap, c) {
                class_instances.insert(c, n);
            }
        }
    }
    Ok(Some(Observed {
        preds,
        class_instances,
        joins: None,
        term_totals: None,
    }))
}

/// The objects of one predicate, and how they were measured.
#[derive(Clone, Debug)]
pub struct PredicateObjects {
    /// Exact when `sampled` is `None`. Otherwise the triples of IRIs, blank nodes, triple
    /// terms and inline literals (numbers, booleans and dates) are exact quad counts,
    /// and the vocabulary literals' total is split among datatypes and languages in the
    /// proportions of the sample. Distinct counts are then 0, as they are not measured.
    pub objects: ObjectKinds,
    /// The number of literal objects of the vocabulary read to split their total, when
    /// the objects were not all read.
    pub sampled: Option<u64>,
}

/// Windows of the sample, spread evenly over the rows of the vocabulary literals.
const WINDOWS: u64 = 16;

/// The most vocabulary literals a sample reads. Each is a lookup in the vocabulary, so
/// the sample stays small: 8,192 rows put a group of 1% of the literals within about a
/// fifth of its share.
const SAMPLE_ROWS: u64 = 8192;

/// The objects of predicate `iri` in the selection of `opts`, or `None` when the
/// predicate does not occur. A predicate with at most `max_rows` quads, or a selection
/// that does not hold every quad of the store, is read in full.
pub fn predicate_objects(
    snap: &Arc<Snapshot>,
    iri: &str,
    opts: &SchemaOptions,
    max_rows: u64,
) -> Result<Option<PredicateObjects>, SchemaError> {
    let budget = Budget {
        deadline: opts.deadline,
        cancel: opts.cancel.as_deref(),
    };
    let Some(p) = snap.lookup_iri(iri).map(|i| i.0) else {
        return Ok(None);
    };
    let phase = || format!("reading the objects of <{iri}>");
    let masked = match &opts.graphs {
        Some(a) => a.masked(snap)?,
        None => snap.clone(),
    };
    let snap: &Snapshot = &masked;
    let sel = Selected::resolve(snap, opts)?;
    let total = in_phase(snap.count(Perm::Pos, &[p]), phase)?;
    if total == 0 {
        return Ok(None);
    }
    if total <= max_rows || !sampleable(snap, opts, &sel.observed) {
        let src = super::Src::direct(snap);
        let mut acc = PredAcc::default();
        in_phase(
            object_pass(&src, p, &sel.observed, &budget, &mut acc, None),
            phase,
        )?;
        if acc.distinct_objects == 0 {
            return Ok(None);
        }
        return Ok(Some(PredicateObjects {
            objects: acc.observed().objects,
            sampled: None,
        }));
    }
    in_phase(sampled(snap, p, &sel.observed, &budget, max_rows), phase).map(Some)
}

/// What the objects of a predicate in graphs outside the selection add to the counts of
/// [`sampled`], by the id range that holds them.
#[derive(Default)]
struct Unread {
    /// by [`Tag`] for the tags outside the vocabulary
    tags: [u64; 12],
    literals: u64,
    triple_terms: u64,
    iris: u64,
}

impl Unread {
    /// The objects of `p` in the graphs of the snapshot that `f` leaves out, read from
    /// their quads.
    fn read(snap: &Snapshot, p: u64, f: &GraphFilter, budget: &Budget) -> crate::Result<Unread> {
        let mut out = Unread::default();
        let vocab = &snap.generation.vocab;
        let (first_triple, first_iri) = (vocab.first_triple(), vocab.first_iri());
        for g in every_graph(snap) {
            if f.accepts(g) {
                continue;
            }
            snap.scan(Perm::Gspo, &[g], |c| {
                budget.check()?;
                let mut each = |k: &Key| {
                    if k[2] != p {
                        return;
                    }
                    let o = Id(k[3]);
                    match o.tag() {
                        Tag::Vocab if o.payload() < first_triple => out.literals += 1,
                        Tag::Vocab if o.payload() < first_iri => out.triple_terms += 1,
                        Tag::Vocab => out.iris += 1,
                        t => out.tags[t as usize] += 1,
                    }
                };
                match c {
                    Chunk::Block(b, s, e) => (s..e).for_each(|i| each(&b.key(i))),
                    Chunk::Row(k) => each(&k),
                }
                Ok(true)
            })?;
        }
        Ok(out)
    }
}

/// The inclusive key range of `POS[p]` whose objects have ids in `[lo, hi]`.
fn objects_between(p: u64, lo: Id, hi: Id) -> (Key, Key) {
    ([p, lo.0, 0, 0], [p, hi.0, u64::MAX, u64::MAX])
}

/// The objects of predicate `p` in the selection `f`: counts of the id ranges over every
/// graph less the objects in the graphs `f` leaves out, which [`sampleable`] keeps few,
/// with the vocabulary literals split by a sample of them in every graph.
fn sampled(
    snap: &Snapshot,
    p: u64,
    f: &GraphFilter,
    budget: &Budget,
    max_rows: u64,
) -> crate::Result<PredicateObjects> {
    // the windows are read once: they do not replace the blocks that queries use
    let reading = snap.without_cache_fill();
    let snap = &reading;
    let unread = Unread::read(snap, p, f, budget)?;
    let vocab = &snap.generation.vocab;
    let tag_range = |t: Tag| (Id::new(t, 0), Id::new(t, PAYLOAD_MASK));
    let count = |(lo, hi): (Id, Id)| -> crate::Result<u64> {
        if lo.0 > hi.0 {
            return Ok(0);
        }
        let (a, b) = objects_between(p, lo, hi);
        snap.count_between(Perm::Pos, a, b)
    };
    let (first_triple, first_iri) = (vocab.first_triple(), vocab.first_iri());
    let vocab_id = |i: u64| Id::new(Tag::Vocab, i);
    let literal_range = (vocab_id(0), vocab_id(first_triple.wrapping_sub(1)));
    let literals = if first_triple == 0 {
        0
    } else {
        count(literal_range)?.saturating_sub(unread.literals)
    };
    let triple_terms = if first_iri > first_triple {
        count((vocab_id(first_triple), vocab_id(first_iri - 1)))?
            .saturating_sub(unread.triple_terms)
    } else {
        0
    };
    let iris = count((vocab_id(first_iri), vocab_id(PAYLOAD_MASK)))?.saturating_sub(unread.iris);
    let in_tag = |t: Tag| -> crate::Result<u64> {
        Ok(count(tag_range(t))?.saturating_sub(unread.tags[t as usize]))
    };
    let blanks = in_tag(Tag::BNode)?;
    let mut acc = PredAcc::default();
    let mut add = |kind: Kind, n: u64| {
        if n > 0 {
            acc.object(kind, n as i64, 0);
        }
    };
    add(Kind::Iri, iris);
    add(Kind::Blank, blanks);
    add(Kind::Triple, triple_terms);
    for t in [
        Tag::Int,
        Tag::Decimal,
        Tag::Double,
        Tag::Bool,
        Tag::DateTime,
        Tag::Date,
    ] {
        budget.check()?;
        let n = in_tag(t)?;
        if n > 0
            && let Some(kind) = super::quick_kind(snap, Id::new(t, 0).0)
        {
            add(kind, n);
        }
    }
    // terms the delta added: each is read, up to `max_rows` of them
    let delta_terms = in_tag(Tag::Delta)?;
    if delta_terms > 0 {
        let (a, b) = objects_between(p, Id::new(Tag::Delta, 0), Id::new(Tag::Delta, PAYLOAD_MASK));
        let mut read = 0u64;
        let mut by: BTreeMap<Vec<u8>, u64> = BTreeMap::new();
        let mut kinds = [0u64; 3];
        snap.scan_between(Perm::Pos, a, b, |c| {
            budget.check()?;
            let mut each = |k: &Key| {
                if !f.accepts(k[3]) {
                    return;
                }
                read += 1;
                match snap.key(Id(k[1])).map(|key| kind_of_key(&key)) {
                    Some(Kind::Iri) => kinds[0] += 1,
                    Some(Kind::Blank) => kinds[1] += 1,
                    Some(Kind::Triple) => kinds[2] += 1,
                    Some(Kind::Literal(s)) => *by.entry(s.into_owned()).or_default() += 1,
                    _ => {}
                }
            };
            match c {
                Chunk::Block(b, s, e) => (s..e).for_each(|i| each(&b.key(i))),
                Chunk::Row(k) => each(&k),
            }
            Ok(read < max_rows)
        })?;
        let scale = |n: u64| scaled(n, read, delta_terms);
        add(Kind::Iri, scale(kinds[0]));
        add(Kind::Blank, scale(kinds[1]));
        add(Kind::Triple, scale(kinds[2]));
        for (suffix, n) in by {
            add(Kind::Literal(suffix.into()), scale(n));
        }
    }
    // the vocabulary's literals: a sample of windows spread over their rows
    let mut sample = 0u64;
    if literals > 0 {
        let rows = max_rows.min(SAMPLE_ROWS);
        let by = literal_sample(snap, p, literal_range, budget, rows, &mut sample)?;
        let mut left = literals;
        let n_groups = by.len();
        for (i, (suffix, n)) in by.into_iter().enumerate() {
            // the last group takes what rounding left, so that the groups add up
            let share = if i + 1 == n_groups {
                left
            } else {
                scaled(n, sample, literals).min(left)
            };
            left -= share;
            add(Kind::Literal(suffix.into()), share);
        }
    }
    let mut objects = acc.observed().objects;
    // `observed` keeps kinds with distinct objects only: these have triples alone
    let kind = |n: u64| {
        (n > 0).then_some(KindCount {
            triples: n,
            distinct: 0,
        })
    };
    objects.iri = kind(acc.iri.triples);
    objects.blank = kind(acc.blank.triples);
    objects.triple_term = kind(acc.triple.triples);
    Ok(PredicateObjects {
        objects,
        sampled: Some(sample),
    })
}

/// `n` of `of` read, scaled to `total`.
fn scaled(n: u64, of: u64, total: u64) -> u64 {
    if of == 0 {
        return 0;
    }
    ((n as u128 * total as u128).div_ceil(of as u128)) as u64
}

/// The key suffixes (datatype or language) of a sample of the vocabulary literals that
/// are objects of `p`: up to [`WINDOWS`] windows of rows that start at evenly spaced row
/// ranks of the base index, read until `max_rows` rows in all.
fn literal_sample(
    snap: &Snapshot,
    p: u64,
    (lo, hi): (Id, Id),
    budget: &Budget,
    max_rows: u64,
    sample: &mut u64,
) -> crate::Result<BTreeMap<Vec<u8>, u64>> {
    let (a, b) = objects_between(p, lo, hi);
    let base = snap.perm(Perm::Pos);
    let (first, last) = base.key_block_range(&a, &b);
    // the window starts: for each rank, the block that holds it, and in that block an
    // object id interpolated between the block's first and last object in the range
    let mut starts: Vec<Key> = Vec::new();
    if first < last {
        let r0 = base.blocks[first].row_start;
        let r1 = base.blocks[last - 1].row_start + base.blocks[last - 1].rows as u64;
        for w in 0..WINDOWS {
            let rank = r0 + (r1 - r0) * w / WINDOWS;
            let i = first + base.blocks[first..last].partition_point(|m| m.row_start <= rank) - 1;
            let m = &base.blocks[i];
            let (lo_o, hi_o) = (m.first.max(a)[1], m.last.min(b)[1]);
            let within = (rank - m.row_start).min(m.rows as u64);
            let o = lo_o
                + ((hi_o.saturating_sub(lo_o)) as u128 * within as u128 / m.rows.max(1) as u128)
                    as u64;
            let k = [p, o, 0, 0].max(a);
            if starts.last().is_none_or(|l| *l < k) {
                starts.push(k);
            }
        }
    } else {
        starts.push(a);
    }
    let per_window = (max_rows / starts.len() as u64).max(1);
    let mut ids: Vec<(u64, u64)> = Vec::new(); // (vocabulary index, rows)
    let mut done: Option<Key> = None;
    for (w, &start) in starts.iter().enumerate() {
        // each window ends before the next one starts, so its read-ahead covers its own
        // blocks and not the rest of the predicate
        let end = match starts.get(w + 1) {
            Some(&next) => match prev_key(next) {
                Some(k) => k.min(b),
                None => break,
            },
            None => b,
        };
        let from = match done {
            Some(d) if d >= start => match next_key(d) {
                Some(k) => k,
                None => break,
            },
            _ => start,
        };
        if from > end {
            continue;
        }
        let mut read = 0u64;
        snap.scan_between(Perm::Pos, from, end, |c| {
            budget.check()?;
            let room = (per_window - read) as usize;
            let mut each = |k: &Key| {
                read += 1;
                done = Some(*k);
                match ids.last_mut() {
                    Some((o, n)) if *o == Id(k[1]).payload() => *n += 1,
                    _ => ids.push((Id(k[1]).payload(), 1)),
                }
            };
            match c {
                Chunk::Block(blk, s, e) => {
                    let e = e.min(s + room);
                    (s..e).for_each(|i| each(&blk.key(i)));
                }
                Chunk::Row(k) => each(&k),
            }
            Ok(read < per_window)
        })?;
        *sample += read;
    }
    // the windows read objects in id order, so the ids are sorted and may repeat only
    // where two windows met
    ids.sort_unstable_by_key(|x| x.0);
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ids.len());
    for (o, n) in ids {
        match merged.last_mut() {
            Some((x, m)) if *x == o => *m += n,
            _ => merged.push((o, n)),
        }
    }
    let only: Vec<u64> = merged.iter().map(|x| x.0).collect();
    let mut by: BTreeMap<Vec<u8>, u64> = BTreeMap::new();
    let mut i = 0usize;
    snap.generation.vocab.get_sorted(&only, |id, key| {
        while i < merged.len() && merged[i].0 < id {
            i += 1;
        }
        if i < merged.len() && merged[i].0 == id {
            *by.entry(literal_suffix(key).to_vec()).or_default() += merged[i].1;
            i += 1;
        }
    });
    Ok(by)
}

/// The key before `k` in key order.
fn prev_key(mut k: Key) -> Option<Key> {
    for c in k.iter_mut().rev() {
        if *c == 0 {
            *c = u64::MAX;
        } else {
            *c -= 1;
            return Some(k);
        }
    }
    None
}

/// The key after `k` in key order.
fn next_key(mut k: Key) -> Option<Key> {
    for c in k.iter_mut().rev() {
        if *c == u64::MAX {
            *c = 0;
        } else {
            *c += 1;
            return Some(k);
        }
    }
    None
}

/// The basis a report of these options over this snapshot would have, without
/// computing it.
pub fn basis_for(snap: &Snapshot, opts: &SchemaOptions) -> Result<Basis, SchemaError> {
    let sel = Selected::resolve(snap, opts)?;
    Ok(if applies(snap, opts, &sel) {
        Basis::Statistics
    } else {
        Basis::Exact
    })
}
