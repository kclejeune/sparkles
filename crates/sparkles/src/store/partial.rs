//! Partial compaction: a new generation that rewrites only the blocks of each
//! permutation that the delta touches, and copies the others as they are encoded.
//!
//! A delta key belongs to the first block whose last key is not below it (the last
//! block takes the keys past the end). Each run of consecutive blocks that holds delta
//! keys is decoded, merged with the delta and encoded again into as few blocks as its
//! rows need, of even size. Every other block is copied byte for byte, so the new
//! permutation reads the same as a full build of the same quads would, block boundaries
//! aside. The vocabulary is copied, so a delta that adds terms needs a full build: the
//! base vocabulary's ids follow the order of its terms, and a new term in the middle
//! would change the id of every term after it. The statistics are updated from the delta
//! and exact counts of the old generation.
//!
//! The C13 spec (Phase 2) has the design and the cost model.

use super::*;
use crate::builder::{CharSet, MAX_CHARSETS, PredicateStat, class_item};
use crate::index::{BLOCK_ROWS, BlockMeta, PermWriter};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

/// The block cache of the statistics update.
const STATS_CACHE_BYTES: u64 = 64 << 20;

/// The share of blocks above which an automatic choice rebuilds everything.
pub(crate) const MAX_REWRITE_SHARE: f64 = 0.5;
/// Blocks after a partial compaction, at most, as a multiple of the blocks a full build
/// would write. Past it the automatic choice rebuilds everything, which packs them again.
pub(crate) const MAX_FRAGMENTATION: f64 = 1.25;

/// Whether a compaction may rewrite only the blocks the delta touches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PartialMode {
    /// when the delta adds no terms and touches few enough blocks
    #[default]
    Auto,
    /// never: every compaction rebuilds the whole generation
    Off,
    /// whenever the delta adds no terms, however many blocks it touches
    Always,
}

impl PartialMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PartialMode::Auto => "auto",
            PartialMode::Off => "off",
            PartialMode::Always => "always",
        }
    }

    pub fn parse(s: &str) -> Option<PartialMode> {
        match s.trim() {
            "auto" => Some(PartialMode::Auto),
            "off" | "false" | "no" => Some(PartialMode::Off),
            "always" => Some(PartialMode::Always),
            _ => None,
        }
    }
}

/// What a permutation's new file is made of, in key order.
#[derive(Debug)]
enum Seg {
    /// an old block, copied
    Copy(usize),
    /// old blocks `lo..hi` merged with their delta keys into `rows` rows (with no old
    /// blocks, the inserted keys alone)
    Run { lo: usize, hi: usize, rows: u64 },
}

/// The blocks a partial compaction rewrites and copies.
#[derive(Debug, Default)]
pub(crate) struct PartialPlan {
    perms: Vec<Vec<Seg>>,
    /// old blocks rewritten
    pub rewritten: u64,
    /// old blocks in all
    pub blocks: u64,
    /// blocks the new permutations will have
    pub blocks_after: u64,
    /// blocks a full build would write
    pub blocks_full: u64,
}

impl PartialPlan {
    /// The share of old blocks rewritten.
    pub fn share(&self) -> f64 {
        self.rewritten as f64 / self.blocks.max(1) as f64
    }

    /// Blocks after the compaction as a multiple of a full build's.
    pub fn fragmentation(&self) -> f64 {
        self.blocks_after as f64 / self.blocks_full.max(1) as f64
    }
}

/// The plan of a partial compaction of `snap`, or why there can be none.
pub(crate) fn plan(snap: &Snapshot) -> std::result::Result<PartialPlan, String> {
    let g = &snap.generation;
    if g.dir.is_none() {
        return Err("the generation has no index files".into());
    }
    let new_terms = snap.delta.ins[Perm::Spo.index()]
        .iter()
        .filter(|k| k.iter().any(|&x| Id(x).tag() == Tag::Delta))
        .count();
    if new_terms > 0 {
        return Err(format!(
            "{new_terms} inserted quads use terms that are not in the vocabulary"
        ));
    }
    let mut plan = PartialPlan::default();
    for p in Perm::ALL {
        let idx = g.perm(p);
        let segs = plan_perm(idx, &snap.delta.ins[p.index()], &snap.delta.del[p.index()]);
        let mut rows = 0;
        for s in &segs {
            match s {
                Seg::Copy(b) => {
                    plan.blocks_after += 1;
                    rows += idx.blocks[*b].rows as u64;
                }
                Seg::Run { lo, hi, rows: n } => {
                    plan.rewritten += (hi - lo) as u64;
                    plan.blocks_after += n.div_ceil(BLOCK_ROWS as u64);
                    rows += n;
                }
            }
        }
        plan.blocks += idx.blocks.len() as u64;
        plan.blocks_full += rows.div_ceil(BLOCK_ROWS as u64);
        plan.perms.push(segs);
    }
    Ok(plan)
}

/// The block that delta key `k` belongs to, at or after block `b`.
fn block_of(blocks: &[BlockMeta], mut b: usize, k: &Key) -> usize {
    while b + 1 < blocks.len() && blocks[b].last < *k {
        b += 1;
    }
    b
}

fn plan_perm(idx: &PermIndex, ins: &OrdSet<Key>, del: &OrdSet<Key>) -> Vec<Seg> {
    let blocks = &idx.blocks;
    if blocks.is_empty() {
        return match ins.len() {
            0 => Vec::new(),
            n => vec![Seg::Run {
                lo: 0,
                hi: 0,
                rows: n as u64,
            }],
        };
    }
    // inserted minus deleted keys per touched block
    let mut touched: Vec<Option<i64>> = vec![None; blocks.len()];
    for (set, sign) in [(ins, 1i64), (del, -1)] {
        let mut b = 0;
        for k in set.iter() {
            b = block_of(blocks, b, k);
            *touched[b].get_or_insert(0) += sign;
        }
    }
    let mut segs = Vec::new();
    let mut run: Option<(usize, i64)> = None;
    for (b, t) in touched.iter().enumerate() {
        match t {
            Some(d) => {
                let r = run.get_or_insert((b, 0));
                r.1 += blocks[b].rows as i64 + d;
            }
            None => {
                if let Some((lo, rows)) = run.take() {
                    segs.push(Seg::Run {
                        lo,
                        hi: b,
                        rows: rows.max(0) as u64,
                    });
                }
                segs.push(Seg::Copy(b));
            }
        }
    }
    if let Some((lo, rows)) = run {
        segs.push(Seg::Run {
            lo,
            hi: blocks.len(),
            rows: rows.max(0) as u64,
        });
    }
    segs
}

/// Splits a run of `rows` rows into the fewest blocks of at most [`BLOCK_ROWS`] rows,
/// of sizes that differ by one at most.
struct Cuts {
    base: u64,
    extra: u64,
    block: u64,
    filled: u64,
}

impl Cuts {
    fn new(rows: u64) -> Cuts {
        let k = rows.div_ceil(BLOCK_ROWS as u64).max(1);
        Cuts {
            base: rows / k,
            extra: rows % k,
            block: 0,
            filled: 0,
        }
    }

    /// One more row: whether it ends a block.
    fn row(&mut self) -> bool {
        self.filled += 1;
        let size = self.base + u64::from(self.block < self.extra);
        if self.filled == size {
            self.block += 1;
            self.filled = 0;
            true
        } else {
            false
        }
    }
}

/// Write permutation `idx.perm` of the new generation in `dir` by `segs`, with the delta
/// keys `ins` and `del` of that permutation. Returns its rows.
fn rewrite(
    dir: &Path,
    idx: &PermIndex,
    segs: &[Seg],
    ins: &OrdSet<Key>,
    del: &OrdSet<Key>,
    interrupt: &crate::builder::InterruptFn,
) -> Result<u64> {
    let mut w = PermWriter::create(dir, idx.perm)?;
    let mut ins = ins.iter().peekable();
    let mut del = del.iter().peekable();
    let nb = idx.blocks.len();
    let mut decoded = 0usize;
    for seg in segs {
        match *seg {
            Seg::Copy(b) => w.push_raw(&idx.blocks[b], idx.raw_block(b)?)?,
            Seg::Run { lo, hi, rows } => {
                let mut cuts = Cuts::new(rows);
                let mut written = 0u64;
                let mut emit = |w: &mut PermWriter, k: Key| -> Result<()> {
                    w.push(k)?;
                    written += 1;
                    if cuts.row() {
                        w.end_block()?;
                    }
                    Ok(())
                };
                if nb == 0 {
                    for k in ins.by_ref() {
                        emit(&mut w, *k)?;
                    }
                }
                for b in lo..hi {
                    decoded += 1;
                    if decoded.is_multiple_of(16) {
                        interrupt()?;
                    }
                    let last = (b + 1 < nb).then(|| idx.blocks[b].last);
                    let mine = |k: &&Key| last.is_none_or(|l| **k <= l);
                    let block = idx.decode_block(b)?;
                    for i in 0..block.len() {
                        let k = block.key(i);
                        while let Some(n) = ins.next_if(|n| mine(n) && **n < k) {
                            emit(&mut w, *n)?;
                        }
                        if del.next_if(|d| **d == k).is_none() {
                            emit(&mut w, k)?;
                        }
                    }
                    while let Some(n) = ins.next_if(mine) {
                        emit(&mut w, *n)?;
                    }
                    if let Some(d) = del.peek().filter(|d| mine(d)) {
                        return Err(Error::Corrupt(format!(
                            "{}: deleted key {d:?} is not in block {b}",
                            idx.perm.name()
                        )));
                    }
                }
                if written != rows {
                    return Err(Error::Corrupt(format!(
                        "{}: blocks {lo}..{hi} gave {written} rows, {rows} expected",
                        idx.perm.name()
                    )));
                }
                w.end_block()?;
            }
        }
    }
    if ins.peek().is_some() || del.peek().is_some() {
        return Err(Error::Corrupt(format!(
            "{}: delta keys past the last block",
            idx.perm.name()
        )));
    }
    w.finish(dir, idx.perm)
}

/// Write the generation of `snap` (base and delta) to `dir` by `plan`: the vocabulary
/// copied, the permutations rewritten where the delta touches them, the statistics
/// updated.
pub(crate) fn write(
    snap: &Snapshot,
    plan: &PartialPlan,
    dir: &Path,
    next_bnode: u64,
    prefixes: BTreeMap<String, String>,
    interrupt: &crate::builder::InterruptFn,
) -> Result<IndexMeta> {
    let g = &snap.generation;
    let from = g
        .dir
        .as_ref()
        .ok_or_else(|| Error::invalid("the generation has no index files"))?;
    std::fs::create_dir_all(dir)?;
    for f in ["vocab.dat", "vocab.off", "vocab.idx"] {
        match std::fs::copy(from.join(f), dir.join(f)) {
            Ok(_) => File::open(dir.join(f))?.sync_all()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    interrupt()?;
    // the permutations and the statistics, which read only the old generation
    let (rows, stats) = rayon::join(
        || -> Result<Vec<u64>> {
            Perm::ALL
                .par_iter()
                .zip(&plan.perms)
                .map(|(&p, segs)| {
                    rewrite(
                        dir,
                        g.perm(p),
                        segs,
                        &snap.delta.ins[p.index()],
                        &snap.delta.del[p.index()],
                        interrupt,
                    )
                })
                .collect()
        },
        || update_stats(snap),
    );
    let (rows, stats) = (rows?, stats?);
    let quads = rows[Perm::Spo.index()];
    if let Some((p, n)) = Perm::ALL.iter().zip(&rows).find(|(_, n)| **n != quads) {
        return Err(Error::Corrupt(format!(
            "permutation {} has {n} rows, spo {quads}",
            p.name()
        )));
    }
    interrupt()?;
    if stats.quads != quads {
        return Err(Error::Corrupt(format!(
            "the statistics count {} quads, the permutations {quads}",
            stats.quads
        )));
    }
    let meta = IndexMeta {
        format_version: crate::builder::FORMAT_VERSION,
        quads,
        terms: g.meta.terms,
        next_bnode: next_bnode.max(g.meta.next_bnode),
        prefixes,
        created: crate::builder::now_rfc3339(),
    };
    write_synced(
        &dir.join("stats.json"),
        &serde_json::to_vec(&stats).unwrap(),
    )?;
    write_synced(
        &dir.join("meta.json"),
        &serde_json::to_vec_pretty(&meta).unwrap(),
    )?;
    sync_dir(dir)?;
    Ok(meta)
}

/// Inserted and deleted delta keys per distinct prefix of `len` columns.
fn groups(ins: &OrdSet<Key>, del: &OrdSet<Key>, len: usize) -> BTreeMap<Key, (u64, u64)> {
    let mut m: BTreeMap<Key, (u64, u64)> = BTreeMap::new();
    let prefix = |k: &Key| pad(&k[..len], 0);
    for k in ins.iter() {
        m.entry(prefix(k)).or_default().0 += 1;
    }
    for k in del.iter() {
        m.entry(prefix(k)).or_default().1 += 1;
    }
    m
}

/// The statistics of `snap`'s base merged with its delta, as a full build would count
/// them. The characteristic sets beyond [`MAX_CHARSETS`] are the exception: a full build
/// keeps the most common sets among those it sees, and this keeps the most common among
/// the old ones and those the delta makes.
pub(crate) fn update_stats(snap: &Snapshot) -> Result<Stats> {
    let g = &snap.generation;
    let old = &g.stats;
    // probes come in key order: a small cache of its own decodes each block once, and
    // keeps the old generation's blocks out of the store's cache
    let cache = BlockCache::new(STATS_CACHE_BYTES);
    let d = &snap.delta;
    let set = |p: Perm| (&d.ins[p.index()], &d.del[p.index()]);
    let mut st = old.clone();
    st.quads = old.quads + d.inserts() as u64 - d.deletes() as u64;
    // how many distinct prefixes appear and disappear: each changed one's old count
    // and its new
    let distinct = |p: Perm, len: usize, keep: &dyn Fn(&Key) -> bool| -> Result<Vec<(Key, i64)>> {
        let (i, x) = set(p);
        let mut out = Vec::new();
        for (k, (n_ins, n_del)) in groups(i, x, len) {
            if !keep(&k) {
                continue;
            }
            let before = g.perm(p).count(&cache, &k[..len])?;
            let after = (before + n_ins).saturating_sub(n_del);
            match (before > 0, after > 0) {
                (false, true) => out.push((k, 1)),
                (true, false) => out.push((k, -1)),
                _ => {}
            }
        }
        Ok(out)
    };
    let all = |_: &Key| true;
    let sum = |v: &[(Key, i64)]| v.iter().map(|(_, n)| n).sum::<i64>();
    let add = |n: u64, by: i64| (n as i64 + by).max(0) as u64;
    st.distinct_subjects = add(old.distinct_subjects, sum(&distinct(Perm::Spo, 1, &all)?));
    st.distinct_objects = add(old.distinct_objects, sum(&distinct(Perm::Osp, 1, &all)?));
    // predicates: quads, distinct subjects and objects
    let mut preds: BTreeMap<u64, PredicateStat> =
        old.predicates.iter().map(|p| (p.p, p.clone())).collect();
    {
        let (i, x) = set(Perm::Pso);
        for (k, (n_ins, n_del)) in groups(i, x, 1) {
            let e = preds.entry(k[0]).or_insert(PredicateStat {
                p: k[0],
                count: 0,
                distinct_subjects: 0,
                distinct_objects: 0,
            });
            e.count = (e.count + n_ins).saturating_sub(n_del);
        }
    }
    for (k, by) in distinct(Perm::Pso, 2, &all)? {
        if let Some(e) = preds.get_mut(&k[0]) {
            e.distinct_subjects = add(e.distinct_subjects, by);
        }
    }
    for (k, by) in distinct(Perm::Pos, 2, &all)? {
        if let Some(e) = preds.get_mut(&k[0]) {
            e.distinct_objects = add(e.distinct_objects, by);
        }
    }
    st.predicates = preds.into_values().filter(|p| p.count > 0).collect();
    st.distinct_predicates = st.predicates.len() as u64;
    // graphs
    let mut graphs: BTreeMap<u64, u64> = old.graphs.iter().copied().collect();
    {
        let (i, x) = set(Perm::Gspo);
        for (k, (n_ins, n_del)) in groups(i, x, 1) {
            let e = graphs.entry(k[0]).or_default();
            *e = (*e + n_ins).saturating_sub(n_del);
        }
    }
    st.graphs = graphs.into_iter().filter(|(_, n)| *n > 0).collect();
    // classes: distinct instances by rdf:type
    let rdf_type = g
        .vocab
        .find(&id::iri_key(oxrdf::vocab::rdf::TYPE.as_str()))
        .ok()
        .map(|i| Id::vocab(i).0);
    if let Some(t) = rdf_type {
        let mut classes: BTreeMap<u64, u64> = old.classes.iter().copied().collect();
        for (k, by) in distinct(Perm::Pos, 3, &|k: &Key| k[0] == t)? {
            let e = classes.entry(k[1]).or_default();
            *e = add(*e, by);
        }
        let mut v: Vec<(u64, u64)> = classes.into_iter().filter(|(_, n)| *n > 0).collect();
        v.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
        st.classes = v;
    }
    update_charsets(snap, &cache, rdf_type, &mut st)?;
    Ok(st)
}

/// The characteristic set of one subject's quads in SPO order, as the builder counts it.
fn charset_of(rows: &[Key], rdf_type: Option<u64>) -> (Vec<u64>, Vec<u64>) {
    let mut cur: Vec<(u64, u64)> = Vec::new();
    let mut classes: Vec<(u64, u64)> = Vec::new();
    for k in rows {
        match cur.last_mut() {
            Some((p, c)) if *p == k[1] => *c += 1,
            _ => cur.push((k[1], 1)),
        }
        if Some(k[1]) == rdf_type
            && let Some(item) = class_item(k[2])
        {
            match classes.last_mut() {
                Some((x, c)) if *x == item => *c += 1,
                _ => classes.push((item, 1)),
            }
        }
    }
    cur.append(&mut classes);
    cur.into_iter().unzip()
}

fn update_charsets(
    snap: &Snapshot,
    cache: &BlockCache,
    rdf_type: Option<u64>,
    st: &mut Stats,
) -> Result<()> {
    let d = &snap.delta;
    let (ins, del) = (&d.ins[Perm::Spo.index()], &d.del[Perm::Spo.index()]);
    if ins.is_empty() && del.is_empty() {
        return Ok(());
    }
    let mut sets: FxHashMap<Vec<u64>, (u64, Vec<u64>)> = std::mem::take(&mut st.charsets)
        .into_iter()
        .map(|c| (c.preds, (c.subjects, c.triples)))
        .collect();
    let mut others = st.charset_others;
    let spo = snap.generation.perm(Perm::Spo);
    let mut rows: Vec<Key> = Vec::new();
    let mut merged: Vec<Key> = Vec::new();
    for s in groups(ins, del, 1).into_keys() {
        rows.clear();
        spo.for_each_range(cache, &[s[0]], |b, lo, hi| {
            rows.extend((lo..hi).map(|i| b.key(i)));
            Ok(())
        })?;
        let (preds, counts) = charset_of(&rows, rdf_type);
        if !preds.is_empty() {
            match sets.get_mut(&preds) {
                Some((n, t)) => {
                    *n -= 1;
                    for (x, c) in t.iter_mut().zip(&counts) {
                        *x = x.saturating_sub(*c);
                    }
                    if *n == 0 {
                        sets.remove(&preds);
                    }
                }
                None => others = others.saturating_sub(1),
            }
        }
        merged.clear();
        let mut add = Delta::range(ins, &[s[0]]).peekable();
        let mut gone = Delta::range(del, &[s[0]]).peekable();
        for k in &rows {
            while let Some(n) = add.next_if(|n| *n < k) {
                merged.push(*n);
            }
            if gone.next_if(|x| *x == k).is_none() {
                merged.push(*k);
            }
        }
        merged.extend(add.copied());
        let (preds, counts) = charset_of(&merged, rdf_type);
        if !preds.is_empty() {
            match sets.get_mut(&preds) {
                Some((n, t)) => {
                    *n += 1;
                    for (x, c) in t.iter_mut().zip(&counts) {
                        *x += c;
                    }
                }
                None => {
                    sets.insert(preds, (1, counts));
                }
            }
        }
    }
    let mut v: Vec<CharSet> = sets
        .into_iter()
        .map(|(preds, (subjects, triples))| CharSet {
            preds,
            subjects,
            triples,
        })
        .collect();
    v.sort_unstable_by(|a, b| {
        b.subjects
            .cmp(&a.subjects)
            .then_with(|| a.preds.cmp(&b.preds))
    });
    others += v.iter().skip(MAX_CHARSETS).map(|c| c.subjects).sum::<u64>();
    v.truncate(MAX_CHARSETS);
    st.charsets = v;
    st.charset_others = others;
    Ok(())
}
