//! The quad store: immutable base generation ⊕ persistent delta, with MVCC snapshots.
//!
//! * A **generation** (`<root>/gen-NNNN/`) is an immutable index built by the bulk
//!   [`Builder`]: sorted vocabulary + 7 permutations + stats. It also owns the append-only
//!   delta vocabulary and the write-ahead log for updates applied on top of it.
//! * The **delta** holds inserted / deleted quads per permutation in persistent
//!   (structurally shared) ordered sets, QLever `DeltaTriples`-style. Cloning a delta is
//!   O(1), so every committed write transaction publishes a new immutable [`Snapshot`];
//!   readers never block writers (Jena TDB2 MR+SW semantics).
//! * **Compaction** merges base ⊕ delta into a new generation and atomically switches
//!   `CURRENT` (TDB2 `Data-NNNN` compaction).

mod backup;
#[cfg(test)]
mod branch_ops_tests;
#[cfg(test)]
mod branch_tests;
mod branching;
mod changelog;
mod changes;
mod clone;
mod commit_graph;
mod compaction;
#[cfg(test)]
mod compaction_tests;
mod describe;
mod diff;
mod embed;
mod geo;
mod group;
mod history_query;
mod link;
mod mem_history;
#[cfg(test)]
mod memory_branch_tests;
mod merge;
mod partial;
mod patch_apply;
mod preview;
mod quota;
mod replay;
mod schedule;
#[cfg(feature = "text")]
pub(crate) mod text_recovery;
// the geometry tests are the only users
#[cfg(all(test, feature = "geo"))]
mod test_support;
mod vector;
pub(crate) use embed::embed_query_text;
pub(crate) mod wal;
pub use backup::{
    BackupBranch, BackupCapture, CapturedFile, FileKind, FileSource, LeaseGuard,
    MEMORY_CAPTURE_PREFIX, MemoryCaptureOptions,
};
pub(crate) use branching::write_initial_table;
pub use branching::{
    BRANCHES_DIR, BRANCHES_FILE, BranchCommit, BranchSet, BranchStore, read_branch_table,
};
pub use changelog::{
    CHANGE_LOG_FILE, CHANGES_DIR, ChangeCommit, ChangeLog, ChangeLogSettings, ChangeLogStatus,
    Unrecorded, UnrecordedReason,
};
pub use changes::{ChangePage, ChangesOptions, CommitChanges};
pub use clone::{CloneMethod, CloneMode, CloneOptions, CloneReport};
pub use commit_graph::{CommitGraph, CommitGraphOptions, GraphBranch, GraphCommit, GraphCursor};
pub use compaction::{
    Blocker, COMPACTION_FILE, CompactOptions, CompactReport, CompactionMeasures, CompactionPolicy,
    CompactionSettings, REBUILD_BUCKETS, RebuildHistogram, RebuildReason, SETTING_NAMES, Trigger,
    TriggerKind,
};
pub use describe::DESCRIBE_FILE;
pub use diff::{Diff, DiffMethod, DiffOp, DiffOptions, StateMark, key_id};
pub use history_query::{HistoryBound, HistoryChange, HistoryQuery, HistoryResult};
pub use link::{Linked, read_link_file};
pub use merge::{INFERRED_GRAPH, conflict_error};
pub use partial::PartialMode;
pub use patch_apply::{PatchOptions, PatchOutcome, parse_commit_iri};
pub use quota::{QUOTA_FILE, QuotaSource, QuotaStatus};

use crate::builder::{BuildOptions, Builder, IndexMeta, Slot, Stats};
use crate::codec::{Codec, Level};
use crate::commit::{
    self, Catalog, CommitInfo, CommitKind, CommitPage, CommitRange, ForkedFrom, Receipt,
};
use crate::error::{Error, Result};
use crate::id::{self, Id, Tag};
use crate::index::{Block, BlockCache, Key, Perm, PermIndex, pad};
use crate::io::Source;
use crate::vocab::{DeltaVocab, Vocab};
use arc_swap::ArcSwap;
use imbl::OrdSet;
use oxrdf::{BlankNode, GraphName, NamedNode, NamedOrBlankNode, Quad, Term};
use parking_lot::{Mutex, MutexGuard};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Immutable base index generation.
pub struct Generation {
    /// unique within the process: two openings of one directory differ
    pub uid: u64,
    pub name: String,
    pub dir: Option<PathBuf>,
    pub vocab: Arc<Vocab>,
    pub perms: Vec<PermIndex>,
    pub stats: Stats,
    pub meta: IndexMeta,
    pub dvocab: DeltaVocab,
    /// keeps a temporary directory alive for in-memory stores with a bulk-built base
    _tmp: Option<tempfile::TempDir>,
    /// A memory branch retains its immutable base mappings and temporary owner.
    _base: Option<Arc<Generation>>,
    /// packed vectors of the base index, built on first search
    pub vectors: crate::vector::GenerationVectors,
    /// the spatial index's geometry column and base tree for this generation
    pub geo: crate::geo::GenerationGeo,
    /// counts from the statistics without the quads of graphs a query does not read
    pub counts: crate::sparql::stats::CountCache,
    /// the characteristic sets of the statistics by predicate, built on first use
    pub charsets: std::sync::OnceLock<crate::sparql::charsets::CharIndex>,
    /// where commits end in this generation's write-ahead log (`None` until built)
    pub(crate) wal_index: Mutex<Option<wal::WalIndex>>,
    /// a branch's linked generation: the upstream files it reads and its base delta
    pub(crate) link: Option<Arc<link::Linked>>,
}

impl Generation {
    fn empty(dvocab: DeltaVocab) -> Generation {
        Generation {
            uid: crate::index::next_uid(),
            name: "mem".into(),
            dir: None,
            vocab: Arc::new(Vocab::empty()),
            perms: Perm::ALL.iter().map(|&p| PermIndex::empty(p)).collect(),
            stats: Stats::default(),
            meta: IndexMeta::default(),
            dvocab,
            _tmp: None,
            _base: None,
            vectors: Default::default(),
            geo: Default::default(),
            counts: Default::default(),
            charsets: Default::default(),
            wal_index: Mutex::new(None),
            link: None,
        }
    }

    fn memory_branch(base: &Arc<Generation>, vocab_len: u64) -> Generation {
        let dvocab = base.dvocab.fork_memory(vocab_len);
        Generation {
            uid: crate::index::next_uid(),
            name: "mem".into(),
            dir: None,
            vocab: base.vocab.clone(),
            perms: base.perms.clone(),
            stats: base.stats.clone(),
            meta: base.meta.clone(),
            dvocab,
            _tmp: None,
            _base: Some(base.clone()),
            vectors: Default::default(),
            geo: Default::default(),
            counts: Default::default(),
            charsets: Default::default(),
            wal_index: Mutex::new(None),
            link: None,
        }
    }

    /// A generation no longer written (not `CURRENT`), opened for reading past states.
    pub(crate) fn open_sealed(dir: &Path, name: &str) -> Result<Generation> {
        Self::open_with(
            dir,
            name,
            DeltaVocab::open_read_only(&dir.join("delta.vocab"))?,
        )
    }

    fn open(dir: &Path, name: &str, persistent: bool) -> Result<Generation> {
        let dvocab = if persistent {
            DeltaVocab::open(&dir.join("delta.vocab"))?
        } else {
            DeltaVocab::in_memory()
        };
        Self::open_with(dir, name, dvocab)
    }

    fn open_with(dir: &Path, name: &str, dvocab: DeltaVocab) -> Result<Generation> {
        let meta: IndexMeta = serde_json::from_slice(&std::fs::read(dir.join("meta.json"))?)
            .map_err(|e| Error::Corrupt(format!("meta.json: {e}")))?;
        if meta.format_version != crate::builder::FORMAT_VERSION {
            return Err(Error::Corrupt(format!(
                "{} has index format {} but this build uses {}; dump and reload the data",
                dir.display(),
                meta.format_version,
                crate::builder::FORMAT_VERSION
            )));
        }
        let stats: Stats = serde_json::from_slice(&std::fs::read(dir.join("stats.json"))?)
            .map_err(|e| Error::Corrupt(format!("stats.json: {e}")))?;
        Ok(Generation {
            uid: crate::index::next_uid(),
            name: name.to_string(),
            dir: Some(dir.to_path_buf()),
            vocab: Arc::new(Vocab::open(dir)?),
            perms: Perm::ALL
                .iter()
                .map(|&p| PermIndex::open(dir, p))
                .collect::<Result<_>>()?,
            stats,
            meta,
            dvocab,
            _tmp: None,
            _base: None,
            vectors: Default::default(),
            geo: Default::default(),
            counts: Default::default(),
            charsets: Default::default(),
            wal_index: Mutex::new(None),
            link: None,
        })
    }

    #[inline]
    pub fn perm(&self, p: Perm) -> &PermIndex {
        &self.perms[p.index()]
    }

    pub fn disk_bytes(&self) -> u64 {
        self.vocab.disk_bytes() + self.perms.iter().map(|p| p.disk_bytes()).sum::<u64>()
    }
}

/// Inserted / deleted quads per permutation (keys in permutation order).
#[derive(Clone, Default)]
pub struct Delta {
    pub ins: [OrdSet<Key>; 7],
    pub del: [OrdSet<Key>; 7],
}

impl Delta {
    pub fn inserts(&self) -> usize {
        self.ins[0].len()
    }
    pub fn deletes(&self) -> usize {
        self.del[0].len()
    }
    pub fn is_empty(&self) -> bool {
        self.ins[0].is_empty() && self.del[0].is_empty()
    }
    /// The keys of `set` that start with `prefix`, in order.
    pub(crate) fn range<'a>(
        set: &'a OrdSet<Key>,
        prefix: &[u64],
    ) -> impl Iterator<Item = &'a Key> + 'a {
        Self::key_range(set, pad(prefix, 0), pad(prefix, u64::MAX))
    }
    fn key_range(set: &OrdSet<Key>, lo: Key, hi: Key) -> impl Iterator<Item = &Key> + '_ {
        set.range((Bound::Included(lo), Bound::Included(hi)))
    }
}

/// A consistent, immutable view of the store.
#[derive(Clone)]
pub struct Snapshot {
    /// Dataset family identity (shared by its branches and derived views).
    pub dataset_id: uuid::Uuid,
    pub generation: Arc<Generation>,
    pub delta: Delta,
    pub version: u64,
    pub cache: Arc<BlockCache>,
    /// query result cache shared by all snapshots of the store
    pub results: Arc<crate::sparql::cache::ResultCache>,
    /// delta-vocabulary size visible to this snapshot
    pub dvocab_len: u64,
    /// the commit (`seq`) this snapshot reflects
    pub commit: u64,
    /// full-text search state at this commit (datasets with full-text search)
    pub text: Option<Arc<crate::text::TextView>>,
    /// the spatial index as of this commit (datasets with a spatial index)
    pub geo: Option<Arc<crate::geo::GeoView>>,
    pub union_default_graph: bool,
    /// largest sum of input vertices of one geometry operation in queries on this
    /// snapshot ([`StoreOptions::geo_op_vertices`])
    pub geo_op_vertices: u64,
    /// per-predicate statistics of the delta (computed lazily, once per snapshot)
    pub delta_stats:
        Arc<std::sync::OnceLock<rustc_hash::FxHashMap<u64, crate::builder::PredicateStat>>>,
    /// a past state (see [`Store::snapshot_at`]), not the live one
    pub historical: bool,
    /// exact counts from the statistics corrected for this snapshot's delta, worked out
    /// once per snapshot
    pub counts: Arc<crate::sparql::stats::CountCache>,
    /// the quads a triple-level access view hides, when this snapshot is such a view's
    /// (they are in the delta as deletions; see [`crate::access::triples`])
    pub mask: Option<Arc<crate::access::Mask>>,
    /// the store's change log, which history queries read (see [`ChangeLog`])
    pub change_log: Option<Arc<ChangeLog>>,
}

/// The kind of term an id stands for (see [`Snapshot::term_kind`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TermKind {
    Iri,
    BNode,
    Literal,
    /// an RDF 1.2 triple term
    Triple,
    /// not a term of the store (UNDEF, specials, local ids)
    Other,
}

impl TermKind {
    /// The kind of a vocabulary key, from its first byte.
    fn of_key_byte(b: u8) -> TermKind {
        match b {
            b'<' => TermKind::Iri,
            b'"' => TermKind::Literal,
            b'(' => TermKind::Triple,
            b'_' => TermKind::BNode,
            _ => TermKind::Other,
        }
    }
}

/// The first row in `[s, e)` of a block with every column decoded whose key is not less
/// than `k`.
fn first_row_from(b: &Block, s: usize, e: usize, k: &Key) -> usize {
    let (mut lo, mut hi) = (s, e);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if b.key(mid) < *k {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// A contiguous run of rows produced by a scan.
pub enum Chunk<'a> {
    /// rows `[start, end)` of a base block (no delta changes in between)
    Block(&'a Block, usize, usize),
    /// a single row from a merge with the delta
    Row(Key),
}

impl Snapshot {
    /// The same snapshot, reading through the block cache without filling it: for full
    /// scans such as exports, which would evict the blocks queries use.
    pub fn without_cache_fill(&self) -> Snapshot {
        Snapshot {
            cache: Arc::new(self.cache.read_through()),
            ..self.clone()
        }
    }

    #[inline]
    pub fn perm(&self, p: Perm) -> &PermIndex {
        self.generation.perm(p)
    }

    /// Planner statistics for a predicate: base-index statistics combined with the
    /// inserted quads of the delta (deleted quads are ignored — estimates only).
    pub fn predicate_stat(&self, p: u64) -> Option<crate::builder::PredicateStat> {
        let base = self.generation.stats.predicate(p).cloned();
        if self.delta.ins[0].is_empty() {
            return base;
        }
        let delta = self.delta_stats.get_or_init(|| {
            let mut m: rustc_hash::FxHashMap<u64, crate::builder::PredicateStat> =
                Default::default();
            // PSO: count and distinct subjects; POS: distinct objects
            let mut prev: Option<Key> = None;
            for k in self.delta.ins[Perm::Pso.index()].iter() {
                let e = m
                    .entry(k[0])
                    .or_insert_with(|| crate::builder::PredicateStat {
                        p: k[0],
                        ..Default::default()
                    });
                e.count += 1;
                if prev.is_none_or(|q| q[0] != k[0] || q[1] != k[1]) {
                    e.distinct_subjects += 1;
                }
                prev = Some(*k);
            }
            prev = None;
            for k in self.delta.ins[Perm::Pos.index()].iter() {
                if prev.is_none_or(|q| q[0] != k[0] || q[1] != k[1])
                    && let Some(e) = m.get_mut(&k[0])
                {
                    e.distinct_objects += 1;
                }
                prev = Some(*k);
            }
            m
        });
        match (base, delta.get(&p)) {
            (b, None) => b,
            (None, Some(d)) => Some(d.clone()),
            (Some(mut b), Some(d)) => {
                b.count += d.count;
                b.distinct_subjects += d.distinct_subjects;
                b.distinct_objects += d.distinct_objects;
                Some(b)
            }
        }
    }

    /// Approximate number of quads (exact unless the delta is inconsistent).
    pub fn len(&self) -> u64 {
        self.generation.meta.quads + self.delta.inserts() as u64 - self.delta.deletes() as u64
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // ---------------------------------------------------------------- terms ------

    /// Id of a term if it exists in this snapshot (inline, base vocab or delta vocab).
    pub fn lookup_term(&self, t: &Term) -> Option<Id> {
        match t {
            Term::BlankNode(b) => parse_bnode_label(b.as_str()),
            _ => {
                if let Some(id) = id::inline_id(t) {
                    return Some(id);
                }
                self.lookup_key(&id::term_key(t))
            }
        }
    }

    pub fn lookup_iri(&self, iri: &str) -> Option<Id> {
        self.lookup_key(&id::iri_key(iri))
    }

    pub fn lookup_key(&self, key: &[u8]) -> Option<Id> {
        if let Ok(i) = self.generation.vocab.find(key) {
            return Some(Id::vocab(i));
        }
        self.generation
            .dvocab
            .find(key)
            .filter(|&i| i < self.dvocab_len)
            .map(Id::delta)
    }

    /// Vocabulary key of a non-inline id.
    pub fn key(&self, id: Id) -> Option<Cow<'_, [u8]>> {
        match id.tag() {
            Tag::Vocab => self.generation.vocab.get(id.payload()).map(Cow::Owned),
            Tag::Delta => self.generation.dvocab.get(id.payload()).map(Cow::Owned),
            _ => None,
        }
    }

    /// Decode a stored id into an RDF term (`None` for UNDEF, specials and local ids).
    pub fn term(&self, id: Id) -> Option<Term> {
        match id.tag() {
            Tag::Bool | Tag::Int | Tag::Double | Tag::Decimal | Tag::DateTime | Tag::Date => {
                id::inline_to_literal(id).map(Term::Literal)
            }
            Tag::BNode => Some(Term::BlankNode(bnode_for(id))),
            Tag::Vocab => self
                .generation
                .vocab
                .get_with(id.payload(), id::key_to_term),
            Tag::Delta => self.key(id).map(|k| id::key_to_term(&k)),
            _ => None,
        }
    }

    /// The kind of term an id stands for, without decoding the term: from the tag of
    /// inline ids, the sort order of the base vocabulary, and the first byte of a delta
    /// key. [`TermKind::Other`] for UNDEF, specials, local ids and unknown ids.
    pub fn term_kind(&self, id: Id) -> TermKind {
        match id.tag() {
            Tag::Bool | Tag::Int | Tag::Double | Tag::Decimal | Tag::DateTime | Tag::Date => {
                TermKind::Literal
            }
            Tag::BNode => TermKind::BNode,
            Tag::Vocab => {
                let v = &self.generation.vocab;
                match id.payload() {
                    i if i >= v.len() => TermKind::Other,
                    i if v.is_iri(i) => TermKind::Iri,
                    i if v.is_triple(i) => TermKind::Triple,
                    _ => TermKind::Literal,
                }
            }
            Tag::Delta if id.payload() < self.dvocab_len => self
                .generation
                .dvocab
                .with(|d| d.get(id.payload()).and_then(|k| k.first().copied()))
                .map_or(TermKind::Other, TermKind::of_key_byte),
            _ => TermKind::Other,
        }
    }

    /// The base-vocabulary ids `[lo, hi)` of the IRIs that start with `prefix`. IRIs
    /// added since the last rebuild have delta ids, outside this range: test those by
    /// their string.
    pub fn iri_prefix_range(&self, prefix: &str) -> (Id, Id) {
        let (lo, hi) = self.generation.vocab.prefix_range(&id::iri_key(prefix));
        (Id::vocab(lo), Id::vocab(hi))
    }

    // ---------------------------------------------------------------- scans ------

    /// Visit all quads whose permuted key starts with `prefix`, in key order, merging
    /// the base index with the delta. The callback returns `false` to stop early.
    pub fn scan(
        &self,
        perm: Perm,
        prefix: &[u64],
        f: impl FnMut(Chunk<'_>) -> Result<bool>,
    ) -> Result<()> {
        self.scan_between(perm, pad(prefix, 0), pad(prefix, u64::MAX), f)
    }

    /// Visit all quads whose permuted key lies in `[lo, hi]`, in key order, merging the
    /// base index with the delta (see [`scan`](Self::scan)).
    pub fn scan_between(
        &self,
        perm: Perm,
        lo: Key,
        hi: Key,
        f: impl FnMut(Chunk<'_>) -> Result<bool>,
    ) -> Result<()> {
        self.scan_between_cols(perm, lo, hi, crate::index::ALL_COLS, f)
    }

    /// [`scan_between`](Self::scan_between) for a reader that only looks at the key
    /// columns in `mask` of base blocks: the others may be left undecoded (and read as
    /// 0). A block with delta changes in it, or between it and the block before, has
    /// every column decoded, because the merge compares full keys.
    pub fn scan_between_cols(
        &self,
        perm: Perm,
        lo: Key,
        hi: Key,
        mask: crate::index::ColMask,
        mut f: impl FnMut(Chunk<'_>) -> Result<bool>,
    ) -> Result<()> {
        let pi = perm.index();
        let (ins_set, del_set) = (&self.delta.ins[pi], &self.delta.del[pi]);
        let mut ins = Delta::key_range(ins_set, lo, hi).peekable();
        let mut del = Delta::key_range(del_set, lo, hi).peekable();
        let changed = ins.peek().is_some() || del.peek().is_some();
        let base = self.perm(perm);
        // the delta keys a block's rows are merged with lie after the block before it
        let mask_of = |b: usize| {
            if !changed {
                return mask;
            }
            let from = match b.checked_sub(1) {
                Some(p) => Bound::Excluded(base.blocks[p].last),
                None => Bound::Unbounded,
            };
            let to = Bound::Included(base.blocks[b].last);
            if ins_set.range((from, to)).next().is_some()
                || del_set.range((from, to)).next().is_some()
            {
                crate::index::ALL_COLS
            } else {
                mask
            }
        };
        let mut stop = false;
        if base.rows > 0 {
            let r = base.for_each_key_range_masked(&self.cache, &lo, &hi, mask_of, |blk, s, e| {
                if stop {
                    return Ok(!stop);
                }
                // with undecoded columns read as 0 this is at most the true last key, and
                // such a block has no delta key at or before its last row left to merge
                let last = blk.key(e - 1);
                let ins_hit = ins.peek().is_some_and(|k| **k <= last);
                let del_hit = del.peek().is_some_and(|k| **k <= last);
                if !ins_hit && !del_hit {
                    if !f(Chunk::Block(blk, s, e))? {
                        stop = true;
                    }
                    return Ok(!stop);
                }
                // merge: the rows before the next delta key go out as one slice, found by
                // binary search, then the inserted key or the deleted row
                let mut i = s;
                loop {
                    let next_ins = ins.peek().filter(|k| ***k <= last).map(|k| **k);
                    let next_del = del.peek().filter(|k| ***k <= last).map(|k| **k);
                    let (k, inserted) = match (next_ins, next_del) {
                        (None, None) => break,
                        (Some(a), Some(d)) if d < a => (d, false),
                        (Some(a), _) => (a, true),
                        (None, Some(d)) => (d, false),
                    };
                    let j = first_row_from(blk, i, e, &k);
                    if i < j && !f(Chunk::Block(blk, i, j))? {
                        stop = true;
                        return Ok(!stop);
                    }
                    i = j;
                    if inserted {
                        // an inserted key is not in the base, so row `j` comes after it
                        if !f(Chunk::Row(k))? {
                            stop = true;
                            return Ok(!stop);
                        }
                        ins.next();
                    } else {
                        // a deleted key is in the base (one that is not is skipped)
                        if i < e && blk.key(i) == k {
                            i += 1;
                        }
                        del.next();
                    }
                }
                if i < e && !f(Chunk::Block(blk, i, e))? {
                    stop = true;
                }
                Ok(!stop)
            });
            r?;
        }
        if !stop {
            for k in ins {
                if !f(Chunk::Row(*k))? {
                    break;
                }
            }
        }
        Ok(())
    }

    /// The base blocks holding keys in the ranges `[lo, hi]` (sorted and disjoint),
    /// visited in parallel: `f(block, s, e)` for the rows `[s, e)` of a block in a range,
    /// at most `piece` rows at a time (the columns in `mask` are decoded), with the
    /// results in key order. `None` when the delta inserts or deletes a key in a range,
    /// whose merge with the blocks is sequential
    /// ([`scan_between_cols`](Self::scan_between_cols) reads those).
    pub fn par_blocks_in_ranges<T: Send>(
        &self,
        perm: Perm,
        ranges: &[(Key, Key)],
        mask: crate::index::ColMask,
        piece: usize,
        f: impl Fn(&Block, usize, usize) -> Result<T> + Sync + Send,
    ) -> Result<Option<Vec<T>>> {
        use rayon::prelude::*;
        let pi = perm.index();
        let touched = |&(lo, hi): &(Key, Key)| {
            Delta::key_range(&self.delta.ins[pi], lo, hi)
                .next()
                .is_some()
                || Delta::key_range(&self.delta.del[pi], lo, hi)
                    .next()
                    .is_some()
        };
        if ranges.iter().any(touched) {
            return Ok(None);
        }
        let base = self.perm(perm);
        let visits: Vec<(usize, usize)> = ranges
            .iter()
            .enumerate()
            .flat_map(|(r, (lo, hi))| {
                let (b0, b1) = base.key_block_range(lo, hi);
                (b0..b1).map(move |b| (r, b))
            })
            .collect();
        visits
            .into_par_iter()
            .map(|(r, b)| {
                let (lo, hi) = &ranges[r];
                let m = &base.blocks[b];
                let whole = m.first >= *lo && m.last <= *hi;
                let bounds = crate::index::bound_cols(lo, hi);
                let blk = self
                    .cache
                    .get_cols(base, b, if whole { mask } else { mask | bounds })?;
                let (s, e) = if whole {
                    (0, blk.len())
                } else {
                    blk.key_range(lo, hi)
                };
                let piece = piece.max(1);
                let starts: Vec<usize> = (s..e.max(s + 1)).step_by(piece).collect();
                starts
                    .into_par_iter()
                    .map(|a| f(&blk, a.min(e), (a + piece).min(e)))
                    .collect::<Result<Vec<T>>>()
            })
            .collect::<Result<Vec<Vec<T>>>>()
            .map(|parts| Some(parts.into_iter().flatten().collect()))
    }

    /// Collect full keys for a prefix (convenience; engine uses [`scan`](Self::scan)).
    pub fn scan_keys(&self, perm: Perm, prefix: &[u64]) -> Result<Vec<Key>> {
        let mut out = Vec::new();
        self.scan(perm, prefix, |c| {
            match c {
                Chunk::Block(b, s, e) => out.extend((s..e).map(|i| b.key(i))),
                Chunk::Row(k) => out.push(k),
            }
            Ok(true)
        })?;
        Ok(out)
    }

    /// Exact number of quads with a key prefix.
    pub fn count(&self, perm: Perm, prefix: &[u64]) -> Result<u64> {
        let base = self.perm(perm).count(&self.cache, prefix)?;
        let pi = perm.index();
        let ins = Delta::range(&self.delta.ins[pi], prefix).count() as u64;
        let del = Delta::range(&self.delta.del[pi], prefix).count() as u64;
        Ok((base + ins).saturating_sub(del))
    }

    /// Cheap estimate of the number of quads with a key prefix (no block decoding).
    pub fn estimate(&self, perm: Perm, prefix: &[u64]) -> u64 {
        let base = self.perm(perm).estimate(prefix);
        let pi = perm.index();
        if self.delta.is_empty() {
            return base;
        }
        let ins = Delta::range(&self.delta.ins[pi], prefix)
            .take(10_000)
            .count() as u64;
        base + ins
    }

    /// Exact number of quads with keys in `[lo, hi]` (at most two block decodes).
    pub fn count_between(&self, perm: Perm, lo: Key, hi: Key) -> Result<u64> {
        let base = self.perm(perm).count_between(&self.cache, &lo, &hi)?;
        let pi = perm.index();
        let ins = Delta::key_range(&self.delta.ins[pi], lo, hi).count() as u64;
        let del = Delta::key_range(&self.delta.del[pi], lo, hi).count() as u64;
        Ok((base + ins).saturating_sub(del))
    }

    pub fn contains(&self, quad: &[Id; 4]) -> Result<bool> {
        let k = Perm::Spo.to_key(quad);
        let pi = Perm::Spo.index();
        if self.delta.ins[pi].contains(&k) {
            return Ok(true);
        }
        if self.delta.del[pi].contains(&k) {
            return Ok(false);
        }
        self.perm(Perm::Spo).contains(&self.cache, &k)
    }

    /// Distinct values of the first key column (e.g. graph names from GSPO), by skipping.
    pub fn distinct_first(&self, perm: Perm) -> Result<Vec<u64>> {
        let mut out = Vec::new();
        let mut cur: Option<u64> = None;
        loop {
            let from = match cur {
                None => 0,
                Some(u64::MAX) => break,
                Some(c) => c + 1,
            };
            // find the first key >= [from, 0, 0, 0]
            let mut next: Option<u64> = None;
            let base = self.perm(perm);
            let b = base.blocks.partition_point(|m| m.last[0] < from);
            if b < base.blocks.len() {
                let blk = self.cache.get(base, b)?;
                let i = blk.cols[0].partition_point(|&v| v < from);
                if i < blk.len() {
                    next = Some(blk.cols[0][i]);
                }
            }
            let pi = perm.index();
            let ins_next = self.delta.ins[pi]
                .range((Bound::Included([from, 0, 0, 0]), Bound::Unbounded))
                .next()
                .map(|k| k[0]);
            next = match (next, ins_next) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let Some(v) = next else { break };
            if self.count(perm, &[v])? > 0 {
                out.push(v);
            }
            cur = Some(v);
        }
        Ok(out)
    }

    /// All named graph ids (excludes the default graph).
    pub fn graph_ids(&self) -> Result<Vec<Id>> {
        Ok(self
            .distinct_first(Perm::Gspo)?
            .into_iter()
            .map(Id)
            .filter(|&g| g != Id::DEFAULT_GRAPH)
            .collect())
    }

    /// Stream every quad as terms (dump / backup / compaction).
    pub fn for_each_quad(&self, f: impl FnMut(&[Id; 4]) -> Result<()>) -> Result<()> {
        self.for_each_quad_in(&[], f)
    }

    /// Stream the quads whose GSPO key starts with `prefix` (`[g]`: the quads of graph
    /// `g`), in GSPO order.
    pub fn for_each_quad_in(
        &self,
        prefix: &[u64],
        mut f: impl FnMut(&[Id; 4]) -> Result<()>,
    ) -> Result<()> {
        self.scan(Perm::Gspo, prefix, |c| {
            match c {
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        f(&Perm::Gspo.to_quad(&b.key(i)))?;
                    }
                }
                Chunk::Row(k) => f(&Perm::Gspo.to_quad(&k))?,
            }
            Ok(true)
        })
    }

    pub fn quad_to_terms(&self, q: &[Id; 4]) -> Option<Quad> {
        let s = match self.term(q[0])? {
            Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
            Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
            _ => return None,
        };
        let Term::NamedNode(p) = self.term(q[1])? else {
            return None;
        };
        let o = self.term(q[2])?;
        let g = if q[3] == Id::DEFAULT_GRAPH {
            GraphName::DefaultGraph
        } else {
            match self.term(q[3])? {
                Term::NamedNode(n) => GraphName::NamedNode(n),
                Term::BlankNode(b) => GraphName::BlankNode(b),
                _ => return None,
            }
        };
        Some(Quad::new(s, p, o, g))
    }
}

/// The blank node of an id, labelled by [`id::bnode_label`]: `b<hex>` for the store's
/// blank nodes, `q<hex>` for ones a query minted.
pub fn bnode_for(id: Id) -> BlankNode {
    BlankNode::new_unchecked(id::bnode_label(id.payload()))
}

/// The stored blank node a label names: only a label exactly as [`bnode_for`] writes it
/// for a stored node. Labels of blank nodes a query minted (`q<hex>`), other spellings of
/// the same number and any other label name no stored node.
pub fn parse_bnode_label(label: &str) -> Option<Id> {
    id::parse_bnode_payload(label)
        .filter(|p| p & Id::LOCAL_BNODE_BIT == 0)
        .map(Id::bnode)
}

// ===================================================================================
// Store
// ===================================================================================

#[derive(Clone, Debug)]
pub struct StoreOptions {
    pub cache_bytes: u64,
    /// Budget for cached query (sub)results; 0 disables the cache.
    pub result_cache_bytes: u64,
    /// Only results that took at least this long (ms) to compute are cached.
    pub result_cache_min_ms: f64,
    /// Budget for cached remote SERVICE results (`SERVICE <cache:…>`); 0 disables
    /// that cache.
    pub service_cache_bytes: u64,
    pub union_default_graph: bool,
    pub build: BuildOptions,
    /// Loads smaller than this many quads go through the transactional delta; larger
    /// loads trigger a rebuild (bulk path).
    pub bulk_threshold: u64,
    /// In-memory stores keep the metadata of this many most recent commits.
    pub memory_commit_ring: usize,
    /// Memory for materialized past states (point-in-time reads).
    pub history_cache_bytes: u64,
    /// Non-current generations named snapshots may keep (the retention window shares it).
    pub history_max_generations: usize,
    /// Named snapshots per dataset.
    pub max_snapshots: usize,
    /// Allow writes to a dataset that requires a write guard without installing one.
    pub unvalidated_writes: bool,
    /// Persistent stores: refuse a commit (and stop a rebuild) that would leave less free
    /// space than this on the store's file system ([`Error::StorageFull`]).
    pub min_free_disk_bytes: Option<u64>,
    /// In-memory stores: refuse a commit that would make the data larger than this
    /// (estimated: index files plus the in-memory delta and vocabulary).
    pub max_memory_bytes: Option<u64>,
    /// Persistent stores: the default storage quota on the on-disk bytes of the dataset
    /// directory (`None`: unlimited); a dataset's `quota.json` overrides it (see
    /// [`Store::set_quota`]). A commit that adds quads past it fails with
    /// [`Error::BudgetExceeded`] before anything is written.
    pub max_disk_bytes: Option<u64>,
    /// Prefixes per dataset (0: unlimited): [`Store::set_prefix`] refuses a new one
    /// past it, and the prefixes of loaded data stop being added.
    pub max_prefixes: usize,
    /// Memory for the spatial index (geometry column and trees); a build that would
    /// exceed it is refused and queries run without the index.
    pub geo_budget_bytes: u64,
    /// Largest sum of input vertices of one geometry operation (overlay, buffer, hull,
    /// relate); larger ones are a type error.
    pub geo_op_vertices: u64,
    /// Honour `queryRewrite` of a dataset's `geo.json`; `false` never rewrites
    /// topological properties (`serve --no-geo-rewrite`).
    pub geo_query_rewrite: bool,
    /// Persistent stores write the spatial index's files (`gen-NNNN/geo/`) after each
    /// build; `false` builds in memory only and writes nothing (read-only servers).
    pub geo_files: bool,
    /// Persistent stores write each vector index build to `gen-NNNN/vectors/`; `false`
    /// builds in memory only.
    pub vector_files: bool,
    /// Compute a change digest for every WAL commit (see
    /// [`annotations::change_digest`](crate::annotations::change_digest)). A persistent
    /// store remembers it: once on, later openings compute digests too.
    pub commit_digests: bool,
    /// Persistent stores: the most a write-ahead log grows ahead of its commits at once
    /// (0: no preallocation, each commit appends to the file). The log grows by zero
    /// bytes that are written and synced in advance, at first 64 KiB at a time and then
    /// by its size, up to this. A commit then overwrites bytes that are already
    /// allocated, and on ext4 and XFS its `fdatasync` does without a journal commit.
    /// That is several times faster, and it no longer waits behind the journal
    /// writes of other files on the same file system. The log's logical end is where
    /// its last commit ends: replay, readers and backups stop at the first zero
    /// record, and an open truncates the zero tail.
    pub wal_prealloc_bytes: u64,
    /// Experimental durable prefix grouping for ordinary persistent WAL writes.
    /// Off by default; unsupported operations drain and use the ordinary commit path.
    pub experimental_group_commit: bool,
    /// Record each commit's net changes in the change log ([`ChangeLog`]), which history
    /// queries read and which outlives compactions. A dataset's `changelog.json` can
    /// turn it off or on.
    pub change_log: bool,
    /// The most the change log may hold, in bytes on disk (0: unlimited); whole segments
    /// are dropped from the oldest. In-memory stores keep at most 64 MiB.
    pub change_log_max_bytes: u64,
    /// The size at which a change log segment is sealed and a new one started.
    pub change_log_segment_bytes: u64,
    /// A bulk commit's changes are recorded when the dataset was empty and the new state
    /// holds at most this many quads, or when both states together hold at most this
    /// many; a larger one is recorded with its counts only.
    pub change_log_bulk_max_quads: u64,
    /// Branches per dataset, `main` included ([`Store::create_branch`] refuses more).
    pub max_branches: usize,
    /// The most upstream log segments a branch's linked generation chains. A branch
    /// created from a commit whose chain would be longer is built as its own generation.
    pub max_branch_depth: usize,
}

/// Default of [`StoreOptions::change_log_max_bytes`]: 1 GiB.
pub const DEFAULT_CHANGE_LOG_MAX_BYTES: u64 = 1 << 30;

/// Default of [`StoreOptions::wal_prealloc_bytes`].
pub const DEFAULT_WAL_PREALLOC_BYTES: u64 = 4 << 20;

/// Default of [`StoreOptions::max_prefixes`].
pub const DEFAULT_MAX_PREFIXES: usize = 1000;
/// Longest prefix name, in bytes.
pub const MAX_PREFIX_NAME_BYTES: usize = 256;
/// Longest prefix IRI, in bytes.
pub const MAX_PREFIX_IRI_BYTES: usize = 4096;

impl Default for StoreOptions {
    fn default() -> Self {
        StoreOptions {
            cache_bytes: 1 << 30,
            result_cache_bytes: 512 << 20,
            result_cache_min_ms: 1.0,
            service_cache_bytes: 64 << 20,
            union_default_graph: false,
            build: BuildOptions::default(),
            bulk_threshold: 250_000,
            memory_commit_ring: 65_536,
            history_cache_bytes: 1 << 30,
            history_max_generations: 8,
            max_snapshots: 256,
            unvalidated_writes: false,
            min_free_disk_bytes: None,
            max_memory_bytes: None,
            max_disk_bytes: None,
            max_prefixes: DEFAULT_MAX_PREFIXES,
            geo_budget_bytes: 4 << 30,
            geo_op_vertices: 2_000_000,
            geo_query_rewrite: true,
            geo_files: true,
            vector_files: true,
            commit_digests: false,
            wal_prealloc_bytes: DEFAULT_WAL_PREALLOC_BYTES,
            experimental_group_commit: false,
            change_log: true,
            change_log_max_bytes: DEFAULT_CHANGE_LOG_MAX_BYTES,
            change_log_segment_bytes: 16 << 20,
            change_log_bulk_max_quads: 1_000_000,
            max_branches: crate::branch::DEFAULT_MAX_BRANCHES,
            max_branch_depth: crate::branch::DEFAULT_MAX_BRANCH_DEPTH,
        }
    }
}

struct WriterState {
    wal: Option<BufWriter<File>>,
    /// bytes of complete transactions in the current generation's WAL
    wal_len: u64,
    /// the WAL file's length: past `wal_len` it holds preallocated zero bytes
    wal_alloc: u64,
    next_bnode: u64,
    /// Latest admitted commit; speculative only while `staged` exists.
    /// General writer admission drains before exposing it as durable metadata.
    head: CommitInfo,
    /// a WAL or generation write failed after a commit started: refuse further writes
    poisoned: bool,
    /// Reused only for commits introducing persisted vocabulary entries.
    vocab_sync: crate::vocab::LazySync,
    /// Private speculative state: readers and receipts only see the synced prefix.
    staged: Option<Arc<Snapshot>>,
    /// the store is being dropped: a backup lease released later must not collect
    closed: bool,
    /// while a compaction runs: each commit's changes, for it to carry over
    tap: Option<compaction::Tap>,
}

impl WriterState {
    /// Cut the space preallocated after the last commit off the WAL, before the store
    /// closes or moves on to another generation's log: a log that is no longer
    /// written ends at its last commit (an open truncates it as well). Not after a
    /// failed write, whose bytes the next open sorts out.
    fn trim_wal(&mut self) {
        let (len, alloc) = (self.wal_len, self.wal_alloc);
        if !self.poisoned
            && alloc > len
            && let Some(wal) = self.wal.as_mut()
            && wal.flush().is_ok()
            && wal.get_ref().set_len(len).is_ok()
        {
            self.wal_alloc = len;
        }
    }
}

/// Commit metadata for a transaction that is committed by rebuilding the generation.
struct BulkCommit {
    kind: CommitKind,
    /// quads the transaction deleted (net) before its bulk batch
    net_del: u64,
    /// quads in the committed snapshot the transaction started from
    start_len: u64,
    /// the commit may change the default graph
    default_graph: bool,
}

/// Counts a writer waiting for the writer lock while it lives.
struct Waiting<'a>(&'a AtomicUsize);

impl<'a> Waiting<'a> {
    fn new(n: &'a AtomicUsize) -> Waiting<'a> {
        n.fetch_add(1, Ordering::Relaxed);
        Waiting(n)
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Whether loading `sources` may write to the default graph: a quad format may hold
/// default-graph quads, a triple format writes there unless a target graph is set.
fn sources_reach_default_graph(sources: &[Source]) -> bool {
    sources
        .iter()
        .any(|s| s.graph.is_none() || s.format.supports_datasets())
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct Store {
    root: Option<PathBuf>,
    opts: StoreOptions,
    group: Option<Box<group::Coordinator>>,
    // `current`, `writer`, `catalog`, `clock` and `history` are shared (weakly) with
    // backup lease guards, whose drop collects history without a `&Store`
    current: Arc<ArcSwap<Snapshot>>,
    writer: Arc<Mutex<WriterState>>,
    /// the current WAL's logical length ([`WriterState::wal_len`]), for readers that do
    /// not take the writer lock
    wal_end: AtomicU64,
    cache: Arc<BlockCache>,
    results: Arc<crate::sparql::cache::ResultCache>,
    prefixes: Mutex<BTreeMap<String, String>>,
    /// exclusive OS lock on `<root>/sparkles.lock` (TDB2 `tdb.lock`), held while open
    _lock: Option<File>,
    dataset_id: uuid::Uuid,
    /// `forkedFrom` of an in-memory clone (a persistent one keeps it in `dataset.json`)
    forked_mem: Option<ForkedFrom>,
    catalog: Arc<Mutex<Catalog>>,
    /// commit messages and change digests (lock order: writer, then annotations)
    annotations: Mutex<crate::annotations::Annotations>,
    /// test hook replacing the wall clock (milliseconds since the epoch)
    clock: Arc<Mutex<Option<Clock>>>,
    /// full-text index, when enabled for this dataset
    #[cfg(feature = "text")]
    text: Arc<arc_swap::ArcSwapOption<crate::text::TextIndex>>,
    #[cfg(not(feature = "text"))]
    text: arc_swap::ArcSwapOption<crate::text::TextIndex>,
    #[cfg(feature = "text")]
    text_recovery: Arc<arc_swap::ArcSwapOption<text_recovery::Recovery>>,
    #[cfg(feature = "text")]
    text_lifecycle: Mutex<()>,
    /// spatial index, when enabled for this dataset
    geo: arc_swap::ArcSwapOption<crate::geo::GeoIndex>,
    /// configured vector indexes (`vector.json`)
    vector: Arc<vector::VectorRegistry>,
    /// embeddings computed on write: the work of the indexes that name a provider
    embed: Arc<crate::vector::embed::Embedder>,
    /// pins, retention, generations and materialized past states (persistent stores)
    history: Option<Arc<Mutex<crate::history::HistoryState>>>,
    /// pins and the retention window of an in-memory store, as kept snapshots
    mem_history: Option<Mutex<crate::history::MemHistory>>,
    /// the net changes of every commit, kept across generations
    changelog: Option<Arc<ChangeLog>>,
    /// write guard checked before every commit (write-time validation)
    guard: parking_lot::RwLock<Option<Arc<dyn crate::guard::CommitGuard>>>,
    /// told the outcome of every guard decision (metrics)
    guard_observer: parking_lot::RwLock<Option<Arc<dyn crate::guard::GuardObserver>>>,
    /// `validation.json` asks for a guard: commits fail without one (fail closed)
    guard_required: AtomicBool,
    /// why the required guard is not installed, for the error of a refused commit
    guard_missing_reason: parking_lot::RwLock<Option<String>>,
    /// write transactions waiting for the writer lock
    writers_waiting: AtomicUsize,
    /// the newest published commit, for waiters on new commits (change feeds)
    commits: tokio::sync::watch::Sender<u64>,
    /// the storage quota and the measured size of the directory (a branch store shares
    /// its dataset's)
    quota: Arc<quota::Quota>,
    /// what the compaction policy looks at, and the dataset's own compaction settings
    compaction: compaction::Track,
    /// the dataset's DESCRIBE setting (`describe.json` of a persistent store)
    describe: parking_lot::RwLock<crate::sparql::describe::DescribeOptions>,
    /// the branch this store is, and the dataset's branches
    branching: branching::Branching,
    /// test hooks by failpoint name
    #[cfg(any(test, feature = "failpoints"))]
    failpoints: Mutex<BTreeMap<&'static str, backup::Failpoint>>,
}

pub(crate) const WAL_INSERT: u8 = 1;

/// Take the exclusive process lock of a database directory.
fn lock_dir(root: &Path) -> Result<File> {
    let path = root.join("sparkles.lock");
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    match f.try_lock() {
        Ok(()) => {
            f.set_len(0)?;
            writeln!(f, "{}", std::process::id())?;
            Ok(f)
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut pid = String::new();
            let _ = File::open(&path).and_then(|mut f| f.read_to_string(&mut pid));
            Err(Error::Locked {
                path: root.to_path_buf(),
                pid: pid.trim().parse().ok(),
            })
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}
pub(crate) const WAL_DELETE: u8 = 2;
pub(crate) const WAL_COMMIT: u8 = 3;
pub(crate) const WAL_REC: usize = 1 + 32;

impl Store {
    /// A fresh in-memory store (Jena `DatasetGraphFactory.createTxnMem()` equivalent).
    pub fn in_memory(opts: StoreOptions) -> Store {
        let root = CommitInfo {
            seq: 0,
            timestamp_ms: commit::now_ms(),
            kind: CommitKind::Create,
            inserted: 0,
            deleted: 0,
            quads: 0,
            generation: 0,
            bulk: false,
            exact: true,
            reconstructed: false,
            default_graph: true,
            unvalidated: false,
        };
        let gen_ = Arc::new(Generation::empty(DeltaVocab::in_memory()));
        Self::in_memory_from(
            opts,
            gen_,
            0,
            BTreeMap::new(),
            root,
            uuid::Uuid::new_v4(),
            None,
        )
    }

    /// An in-memory store whose base is the generation `gen_` (built in a temporary
    /// directory it owns), with root commit `root`: a new store, or an in-memory clone.
    pub(crate) fn in_memory_from(
        opts: StoreOptions,
        gen_: Arc<Generation>,
        next_bnode: u64,
        prefixes: BTreeMap<String, String>,
        root: CommitInfo,
        dataset_id: uuid::Uuid,
        forked_from: Option<ForkedFrom>,
    ) -> Store {
        let cache = Arc::new(BlockCache::new(opts.cache_bytes));
        let results = Arc::new(crate::sparql::cache::ResultCache::with_service(
            opts.result_cache_bytes,
            opts.result_cache_min_ms,
            opts.service_cache_bytes,
        ));
        let dvocab_len = gen_.dvocab.len();
        let mut limits = Store::log_limits(&opts);
        if limits.default_max_bytes == 0 || limits.default_max_bytes > 64 << 20 {
            limits.default_max_bytes = 64 << 20;
        }
        let changelog = Some(ChangeLog::memory(dataset_id, limits));
        let store = Store {
            root: None,
            group: None,
            current: Arc::new(ArcSwap::from_pointee(Snapshot {
                dataset_id,
                generation: gen_,
                delta: Delta::default(),
                version: 0,
                cache: cache.clone(),
                results: results.clone(),
                dvocab_len,
                commit: root.seq,
                text: None,
                geo: None,
                union_default_graph: opts.union_default_graph,
                geo_op_vertices: opts.geo_op_vertices,
                delta_stats: Default::default(),
                counts: Default::default(),
                mask: None,
                historical: false,
                change_log: changelog.clone(),
            })),
            writer: Arc::new(Mutex::new(WriterState {
                wal: None,
                wal_len: 0,
                wal_alloc: 0,
                next_bnode,
                head: root,
                poisoned: false,
                vocab_sync: Default::default(),
                staged: None,
                closed: false,
                tap: None,
            })),
            cache,
            results,
            prefixes: Mutex::new(prefixes),
            _lock: None,
            dataset_id,
            forked_mem: forked_from,
            catalog: Arc::new(Mutex::new(Catalog::memory(root, opts.memory_commit_ring))),
            annotations: Mutex::new(crate::annotations::Annotations::memory(opts.commit_digests)),
            clock: Arc::new(Mutex::new(None)),
            text: Default::default(),
            #[cfg(feature = "text")]
            text_recovery: Default::default(),
            #[cfg(feature = "text")]
            text_lifecycle: Default::default(),
            geo: Default::default(),
            vector: Default::default(),
            embed: Default::default(),
            history: None,
            mem_history: Some(Mutex::new(Default::default())),
            changelog,
            guard: parking_lot::RwLock::new(None),
            guard_observer: parking_lot::RwLock::new(None),
            guard_required: AtomicBool::new(false),
            guard_missing_reason: parking_lot::RwLock::new(None),
            writers_waiting: Default::default(),
            wal_end: AtomicU64::new(0),
            commits: tokio::sync::watch::Sender::new(root.seq),
            quota: Arc::new(quota::Quota::open(None, None).expect("no file to read in memory")),
            branching: Default::default(),
            compaction: compaction::Track::new(0, None, Some(root.timestamp_ms)),
            describe: Default::default(),
            #[cfg(any(test, feature = "failpoints"))]
            failpoints: Default::default(),
            opts,
        };
        let set = branching::BranchSet::memory(
            dataset_id,
            &store.opts,
            store.cache.clone(),
            store.quota.clone(),
        );
        set.set_main(&store.current);
        let mut store = store;
        store.branching.set = branching::SetRef::Owner(set);
        store.open_geo();
        store.digest_root(&root);
        store
    }

    /// Open (or create) a persistent store rooted at `root`.
    ///
    /// With the `text` feature, a configured missing/damaged text index recovers in
    /// the background. RDF queries/writes remain available; text queries report
    /// [`Error::TextUnavailable`] until a ready view is published. Healthy index
    /// verification and covered WAL catch-up still complete before this returns.
    pub fn open(root: &Path, opts: StoreOptions) -> Result<Store> {
        if branching::read_branch_file(root)?.is_some() {
            return Err(crate::branch::invalid_branch(format!(
                "{} is a branch of a dataset; open the dataset's directory and choose the branch",
                root.display()
            )));
        }
        Self::open_inner(root, opts, None)
    }

    /// Open the store of a branch (by the dataset's [`BranchSet`]).
    pub(crate) fn open_branch(
        root: &Path,
        opts: StoreOptions,
        ctx: branching::OpenCtx,
    ) -> Result<Store> {
        Self::open_inner(root, opts, Some(ctx))
    }

    fn open_inner(
        root: &Path,
        opts: StoreOptions,
        ctx: Option<branching::OpenCtx>,
    ) -> Result<Store> {
        commit::check_dataset_compatibility(root)?;
        std::fs::create_dir_all(root)?;
        let lock = lock_dir(root)?;
        commit::check_dataset_compatibility(root)?;
        let current_file = root.join("CURRENT");
        if !current_file.exists() {
            // a new database: its id, an empty generation holding the root commit, the
            // catalog, and CURRENT last (the commit point of the creation)
            let id = match commit::read_dataset_file(root)? {
                Some(id) => id,
                None => {
                    let id = uuid::Uuid::new_v4();
                    let bytes = commit::dataset_file_bytes(id, "create", commit::now_ms());
                    write_atomic(&root.join("dataset.json"), &bytes)?;
                    id
                }
            };
            let name = "gen-0001";
            let dir = root.join(name);
            Builder::new(&dir, opts.build.clone())?.finish()?;
            let first = CommitInfo {
                seq: 0,
                timestamp_ms: commit::now_ms(),
                kind: CommitKind::Create,
                inserted: 0,
                deleted: 0,
                quads: 0,
                generation: 1,
                bulk: false,
                exact: true,
                reconstructed: false,
                default_graph: true,
                unvalidated: false,
            };
            write_synced(
                &dir.join("commit.json"),
                &commit::gen_commit_bytes(id, "create", &first),
            )?;
            Catalog::create(&root.join("commits.bin"), id, first)?;
            sync_dir(&dir)?;
            sync_dir(root)?;
            write_atomic(&current_file, name.as_bytes())?;
        }
        let name = std::fs::read_to_string(&current_file)?.trim().to_string();
        let gen_no = commit::generation_number(&name);
        let cache = match &ctx {
            Some(c) => c.cache.clone(),
            None => Arc::new(BlockCache::new(opts.cache_bytes)),
        };
        let gen_dir = root.join(&name);
        let mut gen_ = match link::read_link(&gen_dir)? {
            Some(f) if ctx.is_some() => {
                Generation::open_linked(&gen_dir, &name, root, f, false, &cache)?
            }
            Some(_) => {
                return Err(Error::Corrupt(format!(
                    "{} is a linked generation outside a branch",
                    gen_dir.display()
                )));
            }
            None => Generation::open(&gen_dir, &name, true)?,
        };
        if let Some(c) = &ctx {
            for g in &c.share {
                gen_.share_blocks_with(g);
            }
        }
        let results = Arc::new(crate::sparql::cache::ResultCache::with_service(
            opts.result_cache_bytes,
            opts.result_cache_min_ms,
            opts.service_cache_bytes,
        ));
        let mut next_bnode = gen_.meta.next_bnode;
        // a branch numbers its blank nodes in its own range
        let bnode_floor = ctx
            .as_ref()
            .map_or(0, |c| crate::branch::bnode_range_start(c.ident.ordinal));
        next_bnode = next_bnode.max(bnode_floor);
        // prefixes.json holds the whole map once written (so removals persist); the
        // generation's own prefixes are used until then
        let mut prefixes = gen_.meta.prefixes.clone();
        if let Ok(p) = std::fs::read(root.join("prefixes.json"))
            && let Ok(p) = serde_json::from_slice::<BTreeMap<String, String>>(&p)
        {
            prefixes = p;
        }
        // The commit the generation's base index holds. A database from an older version
        // has no dataset.json yet: it gets a baseline root commit after replay.
        let known_id = commit::read_dataset_file(root)?;
        let migrating = known_id.is_none();
        let dataset_id = known_id.unwrap_or_else(uuid::Uuid::new_v4);
        let catalog_path = root.join("commits.bin");
        // WAL commits without metadata that precede the first one with it are part of a
        // baseline commit (written by an older version before the upgrade)
        let mut fold_legacy = true;
        let (base, rebased) = match commit::read_gen_commit(&root.join(&name))? {
            Some((id, c, origin)) if !migrating => {
                if id != dataset_id {
                    return Err(Error::Corrupt(format!(
                        "{name}/commit.json belongs to dataset {id}, not {dataset_id}"
                    )));
                }
                fold_legacy = origin == "baseline";
                (c, false)
            }
            // a generation without commit metadata (built by an older version): its
            // content becomes a baseline commit after the last cataloged one
            _ => {
                let prev = match commit::read_catalog(&catalog_path)? {
                    Some((id, recs)) if id == dataset_id && !migrating => recs.last().copied(),
                    _ => None,
                };
                let c = CommitInfo {
                    seq: prev.map_or(0, |c| c.seq + 1),
                    timestamp_ms: prev.map_or(0, |c| c.timestamp_ms).max(commit::now_ms()),
                    kind: CommitKind::Baseline,
                    inserted: gen_.meta.quads,
                    deleted: 0,
                    quads: gen_.meta.quads,
                    generation: gen_no,
                    bulk: false,
                    // counts relative to an earlier commit are unknown
                    exact: prev.is_none(),
                    reconstructed: false,
                    default_graph: true,
                    unvalidated: false,
                };
                (c, true)
            }
        };
        // replay the WAL (a linked generation's onto the delta of its starting commit)
        let wal_path = root.join(&name).join("wal.log");
        let mut delta = gen_.base_delta();
        let mut version = 0;
        let gen_ = Arc::new(gen_);
        let mut replayed: Vec<CommitInfo> = Vec::new();
        // the quads each replayed commit changed, for catching up the full-text index
        let text_on = cfg!(feature = "text") && root.join("text.json").exists();
        let mut wal_text: Vec<(u64, Vec<[Id; 4]>)> = Vec::new();
        // quads of the base plus WAL transactions folded into a baseline commit
        let mut base_quads =
            (gen_.meta.quads + delta.inserts() as u64).saturating_sub(delta.deletes() as u64);
        let mut wal_len = 0u64;
        let mut wal_index = wal::WalIndex::new(base.seq, fold_legacy);
        if wal_path.exists() {
            let mut buf = Vec::new();
            File::open(&wal_path)?.read_to_end(&mut buf)?;
            let from = ReplayFrom {
                generation: &gen_,
                cache: &cache,
                results: &results,
                base,
                gen_no,
                fold_legacy,
                next_bnode,
                keep_touched: text_on,
                path: &wal_path,
                start: gen_.base_delta(),
            };
            let r = replay_wal(&from, &buf, &mut |_, _| Ok(()))?;
            if r.good != buf.len() {
                OpenOptions::new()
                    .write(true)
                    .open(&wal_path)?
                    .set_len(r.good as u64)?;
            }
            wal_len = r.good as u64;
            wal_index = r.index;
            delta = r.delta;
            version = r.version;
            replayed = r.commits;
            base_quads = r.base_quads;
            next_bnode = r.next_bnode.max(bnode_floor);
            wal_text = r.touched;
        }
        let mut base = base;
        if rebased {
            // record the baseline: in the generation, the catalog, and (for an upgrade)
            // dataset.json last, whose presence marks the upgrade complete
            base.quads = base_quads;
            base.inserted = base_quads;
            write_atomic(
                &root.join(&name).join("commit.json"),
                &commit::gen_commit_bytes(dataset_id, "baseline", &base),
            )?;
        }
        let catalog = Catalog::open(&catalog_path, dataset_id, base, &replayed)?;
        if migrating {
            let bytes = commit::dataset_file_bytes(dataset_id, "baseline", base.timestamp_ms);
            write_atomic(&root.join("dataset.json"), &bytes)?;
        }
        // the catalog's record of the head: a commit that a compaction carried into this
        // generation's log keeps the generation it was made in
        let head = replayed
            .last()
            .map(|c| catalog.get(c.seq).filter(|r| r.seq == c.seq).unwrap_or(*c))
            .unwrap_or(base);
        let wal = wal::open_for_append(&wal_path)?;
        let dvocab_len = gen_.dvocab.len();
        *gen_.wal_index.lock() = Some(wal_index);
        let history = open_history(root, dataset_id, gen_no, head.seq, &catalog)?;
        let annotations =
            crate::annotations::Annotations::open(root, dataset_id, head.seq, opts.commit_digests)?;
        let changelog = Some(changelog::ChangeLog::open(
            root,
            dataset_id,
            Store::log_limits(&opts),
        )?);
        let quota = match &ctx {
            Some(c) => c.quota.clone(),
            None => Arc::new(quota::Quota::open(Some(root), opts.max_disk_bytes)?),
        };
        let mut branching = branching::Branching {
            merges: Mutex::new(branching::MergeLog::open(root, head.seq)?),
            ..Default::default()
        };
        match ctx {
            Some(c) => {
                *branching.name.get_mut() = c.ident.name.clone();
                branching.ident = Some(c.ident);
                branching.set = branching::SetRef::Member(c.set);
                branching.protected = AtomicBool::new(c.protected);
                branching.next_ordinal = AtomicU64::new(c.next_ordinal);
            }
            None => {
                let set = BranchSet::load(root, dataset_id, &opts, cache.clone(), quota.clone())?;
                branching.protected = AtomicBool::new(set.main_protected());
                branching.next_ordinal = AtomicU64::new(set.next_ordinal());
                branching.set = branching::SetRef::Owner(set);
            }
        }
        let store = Store {
            root: Some(root.to_path_buf()),
            group: opts
                .experimental_group_commit
                .then(|| Box::new(group::Coordinator::default())),
            current: Arc::new(ArcSwap::from_pointee(Snapshot {
                dataset_id: branching
                    .ident
                    .as_ref()
                    .map_or(dataset_id, |i| i.dataset_id),
                generation: gen_,
                delta,
                version,
                cache: cache.clone(),
                results: results.clone(),
                dvocab_len,
                commit: head.seq,
                text: None,
                geo: None,
                union_default_graph: opts.union_default_graph,
                geo_op_vertices: opts.geo_op_vertices,
                delta_stats: Default::default(),
                counts: Default::default(),
                mask: None,
                historical: false,
                change_log: changelog.clone(),
            })),
            writer: Arc::new(Mutex::new(WriterState {
                wal: Some(BufWriter::new(wal)),
                wal_len,
                wal_alloc: wal_len,
                next_bnode,
                head,
                poisoned: false,
                vocab_sync: Default::default(),
                staged: None,
                closed: false,
                tap: None,
            })),
            cache,
            results,
            prefixes: Mutex::new(prefixes),
            _lock: Some(lock),
            dataset_id,
            forked_mem: None,
            catalog: Arc::new(Mutex::new(catalog)),
            annotations: Mutex::new(annotations),
            clock: Arc::new(Mutex::new(None)),
            text: Default::default(),
            #[cfg(feature = "text")]
            text_recovery: Default::default(),
            #[cfg(feature = "text")]
            text_lifecycle: Default::default(),
            geo: Default::default(),
            vector: Default::default(),
            embed: Default::default(),
            history: Some(Arc::new(Mutex::new(history))),
            mem_history: None,
            changelog,
            guard: parking_lot::RwLock::new(None),
            guard_observer: parking_lot::RwLock::new(None),
            guard_required: AtomicBool::new(guard_required_by(root)),
            guard_missing_reason: parking_lot::RwLock::new(None),
            writers_waiting: Default::default(),
            wal_end: AtomicU64::new(wal_len),
            commits: tokio::sync::watch::Sender::new(head.seq),
            quota,
            branching,
            compaction: compaction::Track::new(
                base.seq,
                replayed.first().map(|c| c.timestamp_ms),
                Some(head.timestamp_ms),
            ),
            describe: parking_lot::RwLock::new(describe::read_settings(root)?),
            #[cfg(any(test, feature = "failpoints"))]
            failpoints: Default::default(),
            opts,
        };
        *store.compaction.settings.lock() = compaction::read_settings(root)?;
        // the merges this store made, and the holds branches place on it, before anything
        // is collected
        if let Some(set) = store.branching.set() {
            let recs = store.branching.merges.lock().recs.clone();
            set.note_merges(store.dataset_id, &recs, true);
            store.install_branch_holds(&set);
            if store.branching.ident.is_none() {
                set.set_main(&store.current);
            }
        }
        store.collect_history(gen_no, head.seq);
        store.release_link_if_rebuilt();
        if let Err(e) = store.recover_change_log() {
            // history queries report the commits it could not record
            tracing::warn!(target: "sparkles::store", error = %e, "could not recover the change log");
        }
        store.open_text(&wal_text)?;
        store.open_geo();
        store.open_vectors();
        if head.seq == 0 {
            store.digest_root(&head);
        }
        // All startup snapshot writers finish before text's background publisher
        // is admitted; open_geo must not overwrite a newly recovered text view.
        #[cfg(feature = "text")]
        if let Some(job) = store.text_recovery.load_full() {
            store.enqueue_text_recovery(&job);
        }
        Ok(store)
    }

    /// The dataset id (a UUID created with the database).
    pub fn dataset_id(&self) -> uuid::Uuid {
        self.dataset_id
    }

    // ---------------------------------------------------------------- history ------

    fn now_ms(&self) -> i64 {
        match &*self.clock.lock() {
            Some(c) => c(),
            None => commit::now_ms(),
        }
    }

    /// Remove the generations history no longer needs (at open).
    fn collect_history(&self, current: u32, head: u64) {
        if let Some(h) = &self.history {
            self.collect_locked(&mut h.lock(), current, head);
        }
    }

    /// Collect unneeded generations; call with the writer lock held (or at open).
    /// Lock order: writer, then history, then catalog.
    fn collect_locked(&self, h: &mut crate::history::HistoryState, current: u32, head: u64) {
        for dir in self.retire_locked(h, current, head) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Like [`collect_locked`](Self::collect_locked), but the unneeded generations are
    /// only renamed: the caller deletes the returned directories, after it released the
    /// writer lock.
    fn retire_locked(
        &self,
        h: &mut crate::history::HistoryState,
        current: u32,
        head: u64,
    ) -> Vec<PathBuf> {
        let Some(root) = &self.root else {
            return Vec::new();
        };
        self.refresh_branch_holds(h);
        collect_generations(
            root,
            self.now_ms(),
            &self.catalog.lock(),
            h,
            current,
            head,
            self.opts.history_max_generations,
        )
    }

    /// Bring the holds branches place on this store up to date in `h` (the dataset's
    /// branch table is the truth; the history state keeps a copy for collectors that
    /// run without the store).
    fn refresh_branch_holds(&self, h: &mut crate::history::HistoryState) {
        if let Some(set) = self.branching.set() {
            let (gens, pins) = set.holds_on(self.dataset_id);
            h.branch_gens = gens;
            h.branch_pins = pins;
        }
    }

    /// What a backup lease guard needs to collect history after it drops the lease.
    pub(crate) fn collector(&self) -> Option<Collector> {
        Some(Collector {
            root: self.root.clone()?,
            max_gens: self.opts.history_max_generations,
            current: Arc::downgrade(&self.current),
            writer: Arc::downgrade(&self.writer),
            catalog: Arc::downgrade(&self.catalog),
            clock: Arc::downgrade(&self.clock),
            history: Arc::downgrade(self.history.as_ref()?),
        })
    }

    fn history_gone(
        &self,
        h: &crate::history::HistoryState,
        seq: u64,
        head: u64,
        snapshot: Option<String>,
        metadata: Option<CommitInfo>,
    ) -> Error {
        let current = commit::generation_number(&self.snapshot().generation.name);
        let reconstructable = h.reconstructable(current, head);
        let oldest = reconstructable.first().map_or(String::new(), |r| {
            format!("; the oldest reconstructable commit is {}", r.0)
        });
        Error::HistoryGone(Box::new(crate::history::HistoryGone {
            message: format!("commit {seq} is no longer reconstructable{oldest}"),
            seq,
            head,
            snapshot,
            reconstructable,
            metadata,
        }))
    }

    /// Resolve a selector to a commit (metadata only): `NotFound` for a commit beyond the
    /// head, an unknown snapshot or a time before history; `HistoryGone` for a commit
    /// whose metadata is gone.
    pub fn resolve(&self, at: &crate::history::At) -> Result<crate::history::Resolved> {
        let head = self.head_commit();
        self.resolve_with(at, head)
    }

    fn resolve_with(
        &self,
        at: &crate::history::At,
        head: CommitInfo,
    ) -> Result<crate::history::Resolved> {
        use crate::history::At;
        let seq = match at {
            At::Head => head.seq,
            At::Commit(n) => *n,
            At::Time(ms) => {
                let cat = self.catalog.lock();
                match cat.at_time(*ms) {
                    Some(c) => c.seq,
                    None if self.branching.ident.is_some() => {
                        drop(cat);
                        return Err(self.inherited(at, 0));
                    }
                    None => {
                        let first = cat.first().map_or(String::new(), |c| {
                            format!(" (history starts at {})", c.timestamp())
                        });
                        return Err(Error::NotFound(format!(
                            "no commit at or before {}{first}",
                            commit::rfc3339_ms(*ms)
                        )));
                    }
                }
            }
            At::Snapshot(name) => self
                .history
                .as_ref()
                .and_then(|h| h.lock().pins.get(name).map(|p| p.seq))
                .or_else(|| {
                    self.mem_history
                        .as_ref()
                        .and_then(|h| h.lock().pins.get(name).map(|p| p.0.seq))
                })
                .ok_or_else(|| Error::NotFound(format!("no snapshot '{name}'")))?,
        };
        if seq > head.seq {
            return Err(Error::NotFound(format!(
                "no commit {seq} (head is {})",
                head.seq
            )));
        }
        // a branch shares the commits before its starting commit with its upstream
        if let Some(i) = &self.branching.ident {
            let before = match at {
                At::Time(ms) => self
                    .catalog
                    .lock()
                    .get(i.from.seq)
                    .is_some_and(|c| c.timestamp_ms > *ms),
                _ => seq < i.from.seq,
            };
            if before {
                return Err(self.inherited(at, seq));
            }
        }
        // History operations also acquire memory history before the catalog. Release
        // the catalog guard before following a missing record into memory history.
        let meta = self.catalog.lock().get(seq);
        let meta = meta.or_else(|| {
            self.mem_history
                .as_ref()
                .and_then(|m| m.lock().branch_bases.get(&seq).map(|(c, _)| *c))
        });
        let Some(commit) = meta else {
            let snapshot = match at {
                At::Snapshot(n) => Some(n.clone()),
                _ => None,
            };
            return Err(match &self.history {
                Some(h) => self.history_gone(&h.lock(), seq, head.seq, snapshot, None),
                None => self.mem_gone(seq, head.seq, snapshot, None),
            });
        };
        Ok(crate::history::Resolved {
            at: at.clone(),
            commit,
            head: head.seq,
            historical: seq != self.snapshot().commit,
        })
    }

    /// The error for a commit a branch store shares with its upstream: reads of it are
    /// served by the upstream (see [`Store::branch_snapshot_at`]).
    fn inherited(&self, at: &crate::history::At, seq: u64) -> Error {
        let i = self.branching.ident.as_ref().expect("a branch store");
        let mut e = crate::branch::BranchError {
            kind: crate::branch::BranchErrorKind::NotFound,
            code: "inherited-commit",
            message: format!(
                "{at} on branch {} is a commit of the history it shares with its upstream; read it there",
                self.branch_name()
            ),
            conflicts: None,
            candidates: Vec::new(),
            inherited: None,
        };
        let seq = match at {
            crate::history::At::Time(ms) => {
                // the upstream resolves the time
                e.inherited = Some(crate::branch::CommitRef {
                    branch_id: i.from.branch_id,
                    seq: i.from.seq,
                });
                let _ = ms;
                return Error::Branch(Box::new(e));
            }
            _ => seq,
        };
        e.inherited = Some(crate::branch::CommitRef {
            branch_id: i.from.branch_id,
            seq,
        });
        Error::Branch(Box::new(e))
    }

    /// The state at `at`: the live snapshot when it names the current state, otherwise
    /// the retained generation's base index with its WAL replayed through the commit
    /// (cached, within `history_cache_bytes`).
    pub fn snapshot_at(
        &self,
        at: &crate::history::At,
        o: &crate::history::HistoryOptions,
    ) -> Result<(Arc<Snapshot>, crate::history::Resolved)> {
        let r = self.resolve(at)?;
        let live = self.snapshot();
        if r.commit.seq == live.commit {
            return Ok((
                live,
                crate::history::Resolved {
                    historical: false,
                    ..r
                },
            ));
        }
        let seq = r.commit.seq;
        if let Some(mem) = &self.mem_history {
            let mut m = mem.lock();
            return match m.get(seq) {
                Some(snap) => {
                    m.hits += 1;
                    Ok((snap, r))
                }
                None => {
                    drop(m);
                    let name = match at {
                        crate::history::At::Snapshot(n) => Some(n.clone()),
                        _ => None,
                    };
                    Err(self.mem_gone(seq, r.head, name, Some(r.commit)))
                }
            };
        }
        let (Some(_), Some(hist)) = (&self.root, &self.history) else {
            return Err(Error::HistoryUnsupported(
                "point-in-time reads need a persistent dataset".into(),
            ));
        };
        let current = commit::generation_number(&live.generation.name);
        let snapshot_name = match at {
            crate::history::At::Snapshot(n) => Some(n.clone()),
            _ => None,
        };
        // one materialization at a time: concurrent requests for a state wait for it
        let mut h = hist.lock();
        let Some(owner) = h.owner(seq, current, r.head) else {
            return Err(self.history_gone(&h, seq, r.head, snapshot_name, Some(r.commit)));
        };
        if let Some(i) = h
            .cache
            .iter()
            .position(|c| c.generation == owner && c.seq == seq)
        {
            let e = h.cache.remove(i);
            let snap = e.snap.clone();
            h.cache.insert(0, e);
            h.hits += 1;
            return Ok((snap, r));
        }
        h.misses += 1;
        let entry = h.gens[&owner].clone();
        let generation = self.history_generation(&mut h, owner, current, &live, &entry)?;
        let budget = self.opts.history_cache_bytes;
        let t0 = std::time::Instant::now();
        let mut check = |d: &Delta| -> Result<()> {
            if o.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(Error::Cancelled);
            }
            if o.deadline.is_some_and(|t| std::time::Instant::now() > t) {
                return Err(Error::Timeout);
            }
            let need = delta_bytes(d);
            if need > budget {
                return Err(Error::BudgetExceeded(crate::Budget {
                    kind: crate::BudgetKind::Memory,
                    limit: budget,
                    requested: need,
                }));
            }
            Ok(())
        };
        let (delta, end) =
            self.materialize(&h, owner, &entry, &generation, &live, seq, &mut check)?;
        h.materializations += 1;
        h.materialize_nanos += t0.elapsed().as_nanos() as u64;
        let bytes = delta_bytes(&delta);
        let dvocab_len = generation.dvocab.len();
        let snap = Arc::new(Snapshot {
            dataset_id: self.owner_dataset_id(),
            generation,
            delta,
            version: 0,
            cache: self.cache.clone(),
            results: self.results.clone(),
            dvocab_len,
            commit: seq,
            text: None,
            geo: self.historical_geo(),
            union_default_graph: self.opts.union_default_graph,
            geo_op_vertices: self.opts.geo_op_vertices,
            delta_stats: Default::default(),
            counts: Default::default(),
            mask: None,
            historical: true,
            change_log: self.changelog.clone(),
        });
        h.cache.insert(
            0,
            crate::history::Cached {
                generation: owner,
                seq,
                snap: snap.clone(),
                bytes,
                end,
            },
        );
        // evict the least recently used states, warm pins' last
        let warm: Vec<(u32, u64)> = h
            .pins
            .values()
            .filter(|p| p.warm)
            .filter_map(|p| h.owner(p.seq, current, r.head).map(|o| (o, p.seq)))
            .collect();
        let mut total: u64 = h.cache.iter().map(|e| e.bytes).sum();
        while total > budget && h.cache.len() > 1 {
            let i = (0..h.cache.len())
                .rev()
                .find(|&i| !warm.contains(&(h.cache[i].generation, h.cache[i].seq)))
                .unwrap_or(h.cache.len() - 1);
            total -= h.cache.remove(i).bytes;
        }
        Ok((snap, r))
    }

    /// Materialize the warm pins' states that the history cache does not hold (a
    /// persistent store's; an in-memory store keeps its pinned states). Returns how many
    /// were built; a state that cannot be built is logged and skipped.
    pub fn warm_snapshots(&self) -> usize {
        let Some(hist) = &self.history else {
            return 0;
        };
        let warm: Vec<(String, u64)> = hist
            .lock()
            .pins
            .iter()
            .filter(|(_, p)| p.warm)
            .map(|(n, p)| (n.clone(), p.seq))
            .collect();
        let mut built = 0;
        for (name, seq) in warm {
            let before = hist.lock().materializations;
            match self.snapshot_at(&crate::history::At::Commit(seq), &Default::default()) {
                Ok(_) => built += usize::from(hist.lock().materializations > before),
                Err(e) => {
                    tracing::warn!(target: "sparkles::store", "warm snapshot {name} (commit {seq}): {e}")
                }
            }
        }
        built
    }

    /// Generation `owner`, open for reading: the live one if it is current, else from
    /// the open-generations list (at most two sealed generations stay open). Call with
    /// the history lock held, so collection cannot remove it while it is being opened.
    pub(crate) fn history_generation(
        &self,
        h: &mut crate::history::HistoryState,
        owner: u32,
        current: u32,
        live: &Snapshot,
        entry: &crate::history::GenEntry,
    ) -> Result<Arc<Generation>> {
        if owner == current {
            return Ok(live.generation.clone());
        }
        if let Some(i) = h.open.iter().position(|(n, _)| *n == owner) {
            let e = h.open.remove(i);
            let g = e.1.clone();
            h.open.insert(0, e);
            return Ok(g);
        }
        let g = match (link::read_link(&entry.dir)?, &self.root) {
            (Some(f), Some(root)) => Arc::new(Generation::open_linked(
                &entry.dir,
                &entry.name,
                root,
                f,
                true,
                &self.cache,
            )?),
            _ => Arc::new(Generation::open_sealed(&entry.dir, &entry.name)?),
        };
        h.open.insert(0, (owner, g.clone()));
        h.open.truncate(2);
        Ok(g)
    }

    /// The delta of commit `seq` in generation `owner`, and where the commit ends in
    /// its log. The state starts from whichever known state of the generation is
    /// nearest in the log: its base, a cached past state, or (in the current
    /// generation) the live state. Later states are reached by replaying the log
    /// forward, earlier ones by undoing it backward.
    #[allow(clippy::too_many_arguments)]
    fn materialize(
        &self,
        h: &crate::history::HistoryState,
        owner: u32,
        entry: &crate::history::GenEntry,
        generation: &Arc<Generation>,
        live: &Snapshot,
        seq: u64,
        check: &mut dyn FnMut(&Delta) -> Result<()>,
    ) -> Result<(Delta, wal::WalPoint)> {
        let path = entry.dir.join("wal.log");
        let target = wal_point(generation, entry, seq)?;
        let base = wal::WalPoint {
            seq: entry.base.seq,
            offset: 0,
            folding: entry.fold_legacy,
        };
        // (start, its delta): the base, cached states, the live state
        let mut best: (u64, wal::WalPoint, Option<&Delta>) = (target.offset, base, None);
        let live_point = if Arc::ptr_eq(&live.generation, generation) && !live.historical {
            Some(wal_point(generation, entry, live.commit)?)
        } else {
            None
        };
        for c in h.cache.iter().filter(|c| c.generation == owner) {
            let cost = c.end.offset.abs_diff(target.offset);
            if cost < best.0 {
                best = (cost, c.end, Some(&c.snap.delta));
            }
        }
        if let Some(p) = live_point {
            let cost = p.offset.abs_diff(target.offset);
            if cost < best.0 {
                best = (cost, p, Some(&live.delta));
            }
        }
        let (_, from, start) = best;
        let mut delta = start.cloned().unwrap_or_else(|| generation.base_delta());
        if from.offset <= target.offset {
            wal::apply_forward(
                &path,
                generation,
                &self.cache,
                &mut delta,
                from,
                target,
                check,
            )?;
            return Ok((delta, target));
        }
        match wal::apply_backward(
            &path,
            generation,
            &self.cache,
            &mut delta,
            target,
            from,
            check,
        ) {
            Ok(_) => Ok((delta, target)),
            Err(wal::Backward::Failed(e)) => Err(e),
            Err(wal::Backward::Legacy) => {
                // a legacy transaction has no number in the log: replay from the base
                let mut delta = generation.base_delta();
                wal::apply_forward(
                    &path,
                    generation,
                    &self.cache,
                    &mut delta,
                    base,
                    target,
                    check,
                )?;
                Ok((delta, target))
            }
        }
    }

    fn named(
        &self,
        h: &crate::history::HistoryState,
        name: &str,
        p: &crate::history::Pin,
        current: u32,
        head: u64,
    ) -> crate::history::NamedSnapshot {
        let owner = h.owner(p.seq, current, head);
        crate::history::NamedSnapshot {
            name: name.to_string(),
            seq: p.seq,
            commit: self.catalog.lock().get(p.seq),
            created_ms: p.created_ms,
            note: p.note.clone(),
            expires_ms: p.expires_ms,
            generation: owner.and_then(|o| h.gens.get(&o)).map(|g| g.name.clone()),
            reconstructable: owner.is_some(),
            warm: p.warm,
        }
    }

    /// The named snapshots, by commit then name.
    pub fn snapshots(&self) -> Vec<crate::history::NamedSnapshot> {
        let Some(hist) = &self.history else {
            return self.mem_snapshots();
        };
        let head = self.head_commit().seq;
        let current = commit::generation_number(&self.snapshot().generation.name);
        let h = hist.lock();
        let mut v: Vec<_> = h
            .pins
            .iter()
            .map(|(n, p)| self.named(&h, n, p, current, head))
            .collect();
        v.sort_by(|a, b| (a.seq, &a.name).cmp(&(b.seq, &b.name)));
        v
    }

    pub fn named_snapshot(&self, name: &str) -> Option<crate::history::NamedSnapshot> {
        self.snapshots().into_iter().find(|s| s.name == name)
    }

    /// Pin commit `at` under `name`. Returns the snapshot and whether it was created
    /// (`false`: the name already pinned that commit). The pin is durable on return.
    pub fn create_snapshot(
        &self,
        name: &str,
        at: &crate::history::At,
        note: Option<String>,
    ) -> Result<(crate::history::NamedSnapshot, bool)> {
        self.create_snapshot_with(name, at, note, None)
    }

    /// [`create_snapshot`](Self::create_snapshot) with an expiry: the pin lapses at
    /// `expires_ms` (milliseconds since the epoch), at the next
    /// [`history_tick`](Self::history_tick).
    pub fn create_snapshot_with(
        &self,
        name: &str,
        at: &crate::history::At,
        note: Option<String>,
        expires_ms: Option<i64>,
    ) -> Result<(crate::history::NamedSnapshot, bool)> {
        self.create_snapshot_opts(
            name,
            at,
            &crate::history::SnapshotOptions {
                note,
                expires_ms,
                warm: false,
            },
        )
    }

    /// Pin commit `at` under `name` with options (a note, an expiry, warm). A warm pin's
    /// state is materialized before this returns.
    pub fn create_snapshot_opts(
        &self,
        name: &str,
        at: &crate::history::At,
        o: &crate::history::SnapshotOptions,
    ) -> Result<(crate::history::NamedSnapshot, bool)> {
        let r = self.create_snapshot_inner(name, at, o)?;
        if o.warm && r.1 {
            self.warm_snapshots();
        }
        Ok(r)
    }

    fn create_snapshot_inner(
        &self,
        name: &str,
        at: &crate::history::At,
        o: &crate::history::SnapshotOptions,
    ) -> Result<(crate::history::NamedSnapshot, bool)> {
        let (note, expires_ms) = (o.note.clone(), o.expires_ms);
        if !crate::history::valid_name(name) {
            return Err(Error::invalid(format!(
                "invalid snapshot name {name:?}: letters, digits, '.', '_' and '-', 1 to 64, starting with a letter or digit"
            )));
        }
        if note.as_ref().is_some_and(|n| n.len() > 1024) {
            return Err(Error::invalid("the note is longer than 1024 bytes"));
        }
        if self.mem_history.is_some() {
            return self.mem_create_snapshot(name, at, o);
        }
        let (Some(root), Some(hist)) = (&self.root, &self.history) else {
            return Err(Error::HistoryUnsupported(
                "named snapshots need a persistent dataset".into(),
            ));
        };
        // no rebuild may run while the pin is being established
        let w = self.guarded_writer();
        let head = w.head;
        let r = self.resolve_with(at, head)?;
        let seq = r.commit.seq;
        let current = commit::generation_number(&self.snapshot().generation.name);
        let mut h = hist.lock();
        if let Some(p) = h.pins.get(name) {
            if p.seq == seq {
                return Ok((self.named(&h, name, p, current, head.seq), false));
            }
            return Err(Error::Conflict(format!(
                "snapshot '{name}' already pins commit {}",
                p.seq
            )));
        }
        if h.pins.len() >= self.opts.max_snapshots {
            return Err(Error::Conflict(format!(
                "history-limit: at most {} snapshots",
                self.opts.max_snapshots
            )));
        }
        let Some(owner) = h.owner(seq, current, head.seq) else {
            return Err(self.history_gone(&h, seq, head.seq, None, Some(r.commit)));
        };
        // the generations pins will hold: a pin inside the current generation (not at
        // its head) holds it once it is compacted away
        let mut held: std::collections::BTreeSet<u32> = h
            .pins
            .values()
            .filter_map(|p| h.owner(p.seq, current, head.seq))
            .collect();
        held.insert(owner);
        let pinned_current = h.pins.values().any(|p| p.seq != head.seq) || seq != head.seq;
        let count = held.iter().filter(|n| **n != current).count()
            + usize::from(held.contains(&current) && pinned_current);
        if count > self.opts.history_max_generations {
            return Err(Error::Conflict(format!(
                "history-limit: pins may hold at most {} generations",
                self.opts.history_max_generations
            )));
        }
        let pin = crate::history::Pin {
            seq,
            created_ms: self.now_ms(),
            note,
            expires_ms,
            warm: o.warm,
        };
        h.pins.insert(name.to_string(), pin.clone());
        if let Err(e) = crate::history::write_file(
            root,
            self.dataset_id,
            &h.pins,
            h.retention,
            &h.schedules,
            h.catalog,
        ) {
            h.pins.remove(name);
            return Err(e);
        }
        drop(w);
        Ok((self.named(&h, name, &pin, current, head.seq), true))
    }

    /// Remove a named snapshot and collect the generations only it held. Returns
    /// whether it existed.
    pub fn delete_snapshot(&self, name: &str) -> Result<bool> {
        let (Some(root), Some(hist)) = (&self.root, &self.history) else {
            return Ok(self.mem_delete_snapshot(name));
        };
        let w = self.guarded_writer();
        let current = commit::generation_number(&self.snapshot().generation.name);
        let mut h = hist.lock();
        let Some(pin) = h.pins.remove(name) else {
            return Ok(false);
        };
        if let Err(e) = crate::history::write_file(
            root,
            self.dataset_id,
            &h.pins,
            h.retention,
            &h.schedules,
            h.catalog,
        ) {
            h.pins.insert(name.to_string(), pin);
            return Err(e);
        }
        self.collect_locked(&mut h, current, w.head.seq);
        Ok(true)
    }

    pub fn retention(&self) -> crate::history::Retention {
        if let Some(m) = &self.mem_history {
            return m.lock().retention;
        }
        self.history
            .as_ref()
            .map(|h| h.lock().retention)
            .unwrap_or_default()
    }

    /// Set the retention window (durable), then collect what it no longer needs.
    pub fn set_retention(
        &self,
        r: crate::history::Retention,
    ) -> Result<crate::history::HistoryStatus> {
        if self.mem_history.is_some() {
            self.mem_set_retention(r);
            return Ok(self.history());
        }
        let (Some(root), Some(hist)) = (&self.root, &self.history) else {
            return Err(Error::HistoryUnsupported(
                "retention needs a persistent dataset".into(),
            ));
        };
        {
            let w = self.guarded_writer();
            let current = commit::generation_number(&self.snapshot().generation.name);
            let mut h = hist.lock();
            let old = h.retention;
            h.retention = r;
            if let Err(e) = crate::history::write_file(
                root,
                self.dataset_id,
                &h.pins,
                r,
                &h.schedules,
                h.catalog,
            ) {
                h.retention = old;
                return Err(e);
            }
            self.collect_locked(&mut h, current, w.head.seq);
        }
        Ok(self.history())
    }

    /// Set how long the commit catalog keeps the metadata of commits that can no longer
    /// be read ([`CatalogHorizon`](crate::history::CatalogHorizon)), durably, and prune
    /// what it no longer keeps.
    pub fn set_catalog_horizon(
        &self,
        c: crate::history::CatalogHorizon,
    ) -> Result<crate::history::HistoryStatus> {
        let (Some(root), Some(hist)) = (&self.root, &self.history) else {
            return Err(Error::HistoryUnsupported(
                "a catalog horizon needs a persistent dataset (an in-memory one keeps a ring of commits)".into(),
            ));
        };
        {
            let _w = self.guarded_writer();
            let mut h = hist.lock();
            let old = h.catalog;
            h.catalog = c;
            if let Err(e) = crate::history::write_file(
                root,
                self.dataset_id,
                &h.pins,
                h.retention,
                &h.schedules,
                c,
            ) {
                h.catalog = old;
                return Err(e);
            }
        }
        self.prune_commits()?;
        Ok(self.history())
    }

    /// Prune the commit records (and annotations) the catalog horizon no longer keeps:
    /// those of commits older than both the readable history and the horizon. Returns
    /// the records dropped (0 when the horizon is off).
    pub fn prune_commits(&self) -> Result<u64> {
        self.prune_commits_with(true)
    }

    /// [`prune_commits`](Self::prune_commits); unless `force`, only once at least 1,024
    /// records, and an eighth of them, can go, so a file is not rewritten for a few.
    pub(crate) fn prune_commits_with(&self, force: bool) -> Result<u64> {
        let Some(hist) = &self.history else {
            return Ok(0);
        };
        let w = self.guarded_writer();
        let head = w.head.seq;
        let now = self.now_ms();
        let cutoff = {
            let current = commit::generation_number(&self.snapshot().generation.name);
            let h = hist.lock();
            let cat = self.catalog.lock();
            let Some(by_horizon) = h.catalog.cutoff(head, now, &|ms| cat.first_at_or_after(ms))
            else {
                return Ok(0);
            };
            let oldest = h
                .reconstructable(current, head)
                .first()
                .map_or(head, |r| r.0);
            let cutoff = by_horizon.min(oldest).min(head);
            let first = cat.first().map_or(head, |c| c.seq);
            let n = cutoff.saturating_sub(first);
            let held = head.saturating_sub(first) + 1;
            if n == 0 || (!force && (n < 1024 || n < held / 8)) {
                return Ok(0);
            }
            cutoff
        };
        let n = self.catalog.lock().prune_before(cutoff)?;
        self.annotations
            .lock()
            .prune_before(cutoff, self.dataset_id)?;
        drop(w);
        if n > 0 {
            self.quota.invalidate();
            tracing::info!(target: "sparkles::store", pruned = n, first = cutoff, "pruned the commit catalog");
        }
        Ok(n)
    }

    /// Retained generations, readable commits, retention and cache counters.
    pub fn history(&self) -> crate::history::HistoryStatus {
        use crate::history::{HistoryGeneration, HistoryStatus, Hold};
        let head = self.head_commit().seq;
        let current = commit::generation_number(&self.snapshot().generation.name);
        let Some(hist) = &self.history else {
            let m = self.mem_history.as_ref().map(|m| m.lock());
            return HistoryStatus {
                head,
                reconstructable: m
                    .as_ref()
                    .map_or_else(|| vec![(head, head)], |m| m.reconstructable(head)),
                generations: Vec::new(),
                bytes: 0,
                retention: m.as_ref().map(|m| m.retention).unwrap_or_default(),
                snapshots: m.as_ref().map_or(0, |m| m.pins.len()),
                catalog: Default::default(),
                first_commit: self.catalog.lock().first().map_or(head, |c| c.seq),
                cache_entries: m.as_ref().map_or(0, |m| m.window.len()),
                cache_bytes: 0,
                hits: m.as_ref().map_or(0, |m| m.hits),
                misses: 0,
                materializations: 0,
                materialize_seconds: 0.0,
            };
        };
        let mut h = hist.lock();
        self.refresh_branch_holds(&mut h);
        let now = self.now_ms();
        let needed = {
            let cat = self.catalog.lock();
            let ts = |s: u64| cat.get(s).map(|c| c.timestamp_ms);
            h.needed(current, head, now, &ts, self.opts.history_max_generations)
        };
        let generations: Vec<HistoryGeneration> = h
            .gens
            .iter()
            .map(|(no, g)| HistoryGeneration {
                name: g.name.clone(),
                base_seq: g.base.seq,
                end_seq: if *no == current { head } else { g.end },
                bytes: g.bytes,
                current: *no == current,
                held_by: if *no == current {
                    std::iter::once(Hold::Head)
                        .chain(h.lease_holds(*no))
                        .collect()
                } else {
                    needed.get(no).cloned().unwrap_or_default()
                },
            })
            .collect();
        HistoryStatus {
            head,
            reconstructable: h.reconstructable(current, head),
            bytes: generations
                .iter()
                .filter(|g| !g.current)
                .map(|g| g.bytes)
                .sum(),
            generations,
            retention: h.retention,
            snapshots: h.pins.len(),
            catalog: h.catalog,
            first_commit: self.catalog.lock().first().map_or(head, |c| c.seq),
            cache_entries: h.cache.len(),
            cache_bytes: h.cache.iter().map(|e| e.bytes).sum(),
            hits: h.hits,
            misses: h.misses,
            materializations: h.materializations,
            materialize_seconds: h.materialize_nanos as f64 / 1e9,
        }
    }

    /// Write the quads of the state at `at` as N-Quads.
    pub fn dump_nquads_at(&self, at: &crate::history::At, w: impl Write) -> Result<u64> {
        let (snap, _) = self.snapshot_at(at, &Default::default())?;
        dump_snapshot(&snap.without_cache_fill(), w)
    }

    /// The latest commit.
    pub fn head_commit(&self) -> CommitInfo {
        self.guarded_writer().head
    }

    /// A receiver that sees each newly published commit's number (the newest only, as
    /// a watch channel does). Change feeds wait on it for new commits.
    pub fn subscribe_commits(&self) -> tokio::sync::watch::Receiver<u64> {
        self.commits.subscribe()
    }

    /// The message and change digest recorded for commit `seq`, if it has either.
    pub fn annotation(&self, seq: u64) -> Option<crate::annotations::Annotation> {
        self.annotations.lock().get(seq).cloned()
    }

    /// This store computes a change digest for every WAL commit
    /// ([`StoreOptions::commit_digests`]).
    pub fn commit_digests(&self) -> bool {
        self.annotations.lock().digests()
    }

    /// Record the annotation of commit `c`, which is about to become durable (writer lock
    /// held): its message, and when digests are on and `changes` can tell them, its
    /// change digest. `changes` gives the deleted and inserted quads as N-Quads lines; it
    /// returns `None` for a commit whose change set is not at hand (bulk commits).
    fn annotate(
        &self,
        c: &CommitInfo,
        message: Option<Arc<str>>,
        changes: impl FnOnce() -> Option<(Vec<String>, Vec<String>)>,
    ) -> Result<crate::annotations::Annotation> {
        let mut a = self.annotations.lock();
        let digest = if a.digests() {
            changes().map(|(deleted, inserted)| {
                let parent = c.parent().and_then(|p| a.get(p)).and_then(|p| p.digest);
                crate::annotations::change_digest(
                    self.dataset_id,
                    c,
                    parent.as_ref(),
                    deleted,
                    inserted,
                )
            })
        } else {
            None
        };
        let ann = crate::annotations::Annotation { message, digest };
        a.append(c.seq, ann.clone(), self.dataset_id)?;
        Ok(ann)
    }

    /// In-memory stores keep annotations only for the commits their catalog keeps.
    fn forget_annotations(&self) {
        if self.root.is_none()
            && let Some(first) = self.catalog.lock().first()
        {
            self.annotations.lock().forget_before(first.seq);
        }
    }

    /// The digest of an empty `create` root commit, when digests are on and it has none.
    fn digest_root(&self, root: &CommitInfo) {
        if root.seq != 0 || root.kind != CommitKind::Create || root.inserted != 0 {
            return;
        }
        let a = self.annotations.lock();
        if !a.digests() || a.get(0).is_some_and(|a| a.digest.is_some()) {
            return;
        }
        drop(a);
        if let Err(e) = self.annotate(root, None, || Some((Vec::new(), Vec::new()))) {
            tracing::warn!(target: "sparkles::store", error = %e, "could not record the root commit's change digest");
        }
    }

    /// Metadata of one commit, if it exists and is retained.
    pub fn commit(&self, seq: u64) -> Option<CommitInfo> {
        self.catalog.lock().get(seq)
    }

    /// Whether a commit after `after`, up to and including `at`, may have changed the
    /// default graph. Commits that changed named graphs alone do not count. A commit
    /// whose record is no longer retained, or was written before the flag existed,
    /// counts as a change.
    pub fn default_graph_changed(&self, after: u64, at: u64) -> bool {
        self.catalog.lock().default_graph_changed(after, at)
    }

    /// A page of the commit catalog.
    pub fn commits(&self, range: CommitRange, limit: usize) -> CommitPage {
        self.catalog.lock().page(range, limit)
    }

    /// Replace the wall clock used for commit timestamps (tests).
    #[doc(hidden)]
    pub fn set_clock(&self, clock: Arc<dyn Fn() -> i64 + Send + Sync>) {
        crate::sparql::extensions::assert_family(self.owner_dataset_id());
        *self.clock.lock() = Some(clock);
    }

    // ------------------------------------------------------------ full-text ------

    /// Open the full-text index if `text.json` enables it (rebuilding it if needed).
    fn open_text(&self, wal: &[(u64, Vec<[Id; 4]>)]) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        #[cfg(feature = "text")]
        {
            let Some(cfg) = crate::text::imp_read_config(root)? else {
                return Ok(());
            };
            let snap = self.snapshot();
            let first_wal_seq = snap
                .generation
                .linked()
                .and_then(|link| link.file.segments.iter().find(|s| s.wal_end > 0))
                .and_then(|segment| segment.base_seq.checked_add(1))
                .or_else(|| wal.first().map(|(seq, _)| *seq));
            let load_wal = || {
                let Some(link) = snap.generation.linked() else {
                    return Ok(std::borrow::Cow::Borrowed(wal));
                };
                let mut touched = Vec::new();
                let dataset_root = link::dataset_root_of(root)?;
                for segment in &link.file.segments {
                    let path = segment.dir(&dataset_root).join("wal.log");
                    let from = wal::WalPoint {
                        seq: segment.base_seq,
                        offset: 0,
                        folding: false,
                    };
                    let mut cursor = wal::WalCursor::open(&path, from)?;
                    while cursor.position().offset < segment.wal_end {
                        let Some((seq, bytes)) = cursor.next()? else {
                            return Err(Error::Corrupt("inherited text replay ends early".into()));
                        };
                        touched.push((
                            seq,
                            bytes
                                .as_chunks::<WAL_REC>()
                                .0
                                .iter()
                                .map(|r| wal::record_quad(r))
                                .collect(),
                        ));
                    }
                    if cursor.position().offset != segment.wal_end {
                        return Err(Error::Corrupt(
                            "inherited text replay is not at a commit boundary".into(),
                        ));
                    }
                }
                touched.extend_from_slice(wal);
                Ok(std::borrow::Cow::Owned(touched))
            };
            match crate::text::TextIndex::open_ready(
                root,
                cfg.clone(),
                &snap,
                first_wal_seq,
                load_wal,
            ) {
                Ok(Some((ti, view))) => {
                    self.text.store(Some(Arc::new(ti)));
                    let mut s = (*snap).clone();
                    s.text = Some(view);
                    self.current.store(Arc::new(s));
                }
                Ok(None) => {
                    self.register_text_recovery(cfg);
                }
                Err(e) => {
                    tracing::error!(target: "sparkles::store", "full-text startup: {e}; recovering");
                    self.register_text_recovery(cfg);
                }
            }
        }
        #[cfg(not(feature = "text"))]
        let _ = wal;
        #[cfg(not(feature = "text"))]
        if root.join("text.json").exists() {
            tracing::warn!(
                target: "sparkles::store",
                "{}: full-text search is configured but this build has no `text` feature",
                root.display()
            );
        }
        Ok(())
    }

    /// Apply a WAL commit's changes to the full-text index and give `snap` its view.
    fn maintain_text(&self, snap: &mut Snapshot, log: &[(u8, [Id; 4])]) {
        #[cfg(feature = "text")]
        if let Some(job) = self.text_recovery.load_full().filter(|j| j.pending()) {
            job.record(snap, log);
            snap.text = Some(job.view(snap.commit));
        } else if let Some(ti) = self.text.load_full() {
            let prev = snap.text.take();
            snap.text = ti.apply_commit(snap, log, prev.as_ref());
        }
        #[cfg(not(feature = "text"))]
        let _ = (snap, log);
    }

    /// After a bulk commit from `old`: bring the full-text index to the new snapshot,
    /// by the documents that differ when they are few, else by a rebuild (on failure the
    /// previous view stays, and text queries report the index as stale).
    fn rebuild_text_locked(&self, snap: &mut Snapshot, old: &Snapshot) {
        #[cfg(feature = "text")]
        if let Some(job) = self.text_recovery.load_full().filter(|j| j.pending()) {
            job.changed_generation();
            snap.text = Some(job.view(snap.commit));
        } else if let Some(ti) = self.text.load_full() {
            let incremental = ti.apply_bulk(old, snap).unwrap_or_else(|e| {
                tracing::warn!(target: "sparkles::store", "full-text index after a bulk commit: {e}; rebuilding");
                None
            });
            // an online rebuild that runs meanwhile builds the index again once it is
            // done, since the generation changed: until then the index is behind
            let view = match incremental {
                Some(v) => Ok(Some(v)),
                None => ti.try_rebuild(snap),
            };
            snap.text = match view {
                Ok(Some(v)) => Some(v),
                Ok(None) => old.text.clone(),
                Err(e) => {
                    tracing::error!(target: "sparkles::store", "full-text rebuild after a bulk commit failed: {e}");
                    old.text.clone()
                }
            };
        }
        #[cfg(not(feature = "text"))]
        let _ = (snap, old);
    }

    /// Enable (or reconfigure) full-text search and build the index from the current
    /// state. The configuration is kept in `text.json`. This synchronous operation
    /// cancels and joins startup recovery before installing the requested configuration.
    #[cfg(feature = "text")]
    pub fn enable_text(&self, cfg: crate::text::TextConfig) -> Result<crate::text::TextStatus> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        let _lifecycle = self.text_lifecycle.lock();
        self.stop_text_recovery();
        // an online rebuild of the current index finishes first (it builds in `text.new`)
        let current = self.text.load_full();
        let _rebuild = current.as_ref().map(|t| t.lock_rebuild());
        let _w = self.guarded_writer();
        self.text.store(None);
        if let Some(root) = &self.root {
            write_atomic(
                &root.join("text.json"),
                &serde_json::to_vec_pretty(&cfg).unwrap(),
            )?;
        }
        let snap = self.snapshot();
        let (ti, view) = crate::text::TextIndex::open(self.root.as_deref(), cfg, &snap, &[])?;
        let ti = Arc::new(ti);
        self.text.store(Some(ti.clone()));
        let mut s = (*snap).clone();
        s.text = Some(view.clone());
        self.current.store(Arc::new(s));
        Ok(ti.status(Some(&view), snap.commit))
    }

    /// Turn full-text search off and delete its index. Cancels and joins startup
    /// recovery before removing its configuration and derived files.
    #[cfg(feature = "text")]
    pub fn disable_text(&self) -> Result<()> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        let _lifecycle = self.text_lifecycle.lock();
        self.stop_text_recovery();
        let current = self.text.load_full();
        let _rebuild = current.as_ref().map(|t| t.lock_rebuild());
        let _w = self.guarded_writer();
        self.text.store(None);
        let mut s = (*self.snapshot()).clone();
        s.text = None;
        self.current.store(Arc::new(s));
        if let Some(root) = &self.root {
            match std::fs::remove_file(root.join("text.json")) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
            let _ = std::fs::remove_dir_all(root.join("text"));
            let _ = std::fs::remove_file(root.join("text.dirty"));
        }
        Ok(())
    }

    /// Rebuild the full-text index from the current state. The index is built while
    /// writes go on and the current index serves searches; the commits made meanwhile
    /// are then applied to it, and it takes the current index's place. Writes wait only
    /// for that last step, unless a compaction or bulk commit changed the store's
    /// generation meanwhile: the index is then built again with writes waiting.
    ///
    /// During startup recovery, this synchronous method joins the queued/running
    /// recovery, or retries a failed attempt. The join releases the lifecycle lock
    /// so another caller can disable/reconfigure and cancel it. This method has no
    /// per-waiter cancellation token; cancelling an HTTP task retains the existing
    /// synchronous rebuild behavior. Store close and disable/reconfigure cancel the
    /// automatic job, joining any admitted directory publication fence.
    #[cfg(feature = "text")]
    pub fn rebuild_text(&self) -> Result<crate::text::TextStatus> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        let lifecycle = self.text_lifecycle.lock();
        if let Some(job) = self.retry_text_recovery() {
            // Joining a queued global-worker job must never prevent another caller
            // from disabling/reconfiguring this dataset and cancelling the join.
            drop(lifecycle);
            #[cfg(test)]
            text_recovery::hook(
                self.root.as_deref().expect("persistent recovery"),
                "manual-join",
            );
            job.wait()?;
            let _lifecycle = self.text_lifecycle.lock();
            if !self
                .text_recovery
                .load()
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &job))
            {
                return Err(Error::Cancelled);
            }
            return self.text_status().ok_or(Error::Cancelled);
        }
        let _lifecycle = lifecycle;
        let ti = self
            .text
            .load_full()
            .ok_or_else(|| Error::invalid("full-text search is not enabled"))?;
        // before the writer lock, which a rebuild holding this waits for
        let guard = ti.lock_rebuild();
        let start = {
            let _w = self.guarded_writer();
            ti.start_journal(&guard);
            self.snapshot()
        };
        let built = ti.build(&guard, &start)?;
        let _w = self.guarded_writer();
        if !self
            .text
            .load()
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(c, &ti))
        {
            return Err(Error::invalid(
                "full-text search was disabled or reconfigured during the rebuild",
            ));
        }
        let head = self.snapshot();
        let view = if head.generation.uid == start.generation.uid {
            ti.install(&guard, built, &head)?
        } else {
            drop(built);
            ti.rebuild_held(&guard, &head)?
        };
        let mut s = (*head).clone();
        s.text = Some(view.clone());
        self.current.store(Arc::new(s));
        Ok(ti.status(Some(&view), head.commit))
    }

    /// Full-text status (`None`: not enabled).
    #[cfg(feature = "text")]
    pub fn text_status(&self) -> Option<crate::text::TextStatus> {
        let snap = self.snapshot();
        if let Some(job) = self.text_recovery.load_full().filter(|job| job.pending()) {
            return Some(job.status(snap.commit));
        }
        if let Some(ti) = self.text.load_full() {
            return Some(ti.status(snap.text.as_deref(), snap.commit));
        }
        self.text_recovery
            .load_full()
            .map(|job| job.status(snap.commit))
    }

    /// Whether full-text search is enabled.
    pub fn text_enabled(&self) -> bool {
        #[cfg(feature = "text")]
        if self.text_recovery.load().is_some() {
            return true;
        }
        self.text.load().is_some()
    }

    /// Test hook: make the next full-text update fail.
    #[cfg(feature = "text")]
    #[doc(hidden)]
    pub fn fail_next_text_commit(&self) {
        if let Some(ti) = self.text.load_full() {
            ti.fail_next_commit();
        }
    }

    /// Test hook: pause (or resume) the full-text index's background tick, so staged
    /// documents stay uncommitted until a search, compaction or close.
    #[cfg(feature = "text")]
    #[doc(hidden)]
    pub fn set_text_ticks(&self, on: bool) {
        crate::sparql::extensions::assert_family(self.owner_dataset_id());
        if let Some(ti) = self.text.load_full() {
            ti.set_ticks(on);
        }
    }

    /// Timestamp for the commit after `head`: the clock, never before the head's.
    fn commit_time(&self, head: &CommitInfo) -> i64 {
        let now = match &*self.clock.lock() {
            Some(c) => c(),
            None => commit::now_ms(),
        };
        now.max(head.timestamp_ms)
    }

    pub fn is_persistent(&self) -> bool {
        self.root.is_some()
    }
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }
    pub fn options(&self) -> &StoreOptions {
        &self.opts
    }
    pub fn cache(&self) -> &Arc<BlockCache> {
        &self.cache
    }
    pub fn result_cache(&self) -> &Arc<crate::sparql::cache::ResultCache> {
        &self.results
    }

    fn guarded_writer(&self) -> MutexGuard<'_, WriterState> {
        crate::sparql::extensions::assert_family(self.owner_dataset_id());
        let mut w = self.writer.lock();
        self.drain_group(&mut w, None).ok();
        w
    }

    /// Current read snapshot.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        crate::sparql::extensions::assert_family(self.owner_dataset_id());
        self.current.load_full()
    }

    pub fn prefixes(&self) -> BTreeMap<String, String> {
        self.prefixes.lock().clone()
    }

    /// Add the prefixes of loaded data, keeping the ones already defined. Past
    /// [`StoreOptions::max_prefixes`], or with a name or IRI past its length limit, a
    /// prefix is left out (the data is not refused over its prefixes).
    pub fn add_prefixes(&self, p: BTreeMap<String, String>) -> Result<()> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        if p.is_empty() {
            return Ok(());
        }
        let mut cur = self.prefixes.lock();
        let before = cur.len();
        let mut skipped = 0usize;
        for (k, v) in p {
            if cur.contains_key(&k) {
                continue;
            }
            if self.prefixes_full(cur.len())
                || k.len() > MAX_PREFIX_NAME_BYTES
                || v.len() > MAX_PREFIX_IRI_BYTES
            {
                skipped += 1;
                continue;
            }
            cur.insert(k, v);
        }
        if skipped > 0 {
            tracing::warn!(
                target: "sparkles::store",
                "{skipped} prefixes of the loaded data were not added (at most {} per dataset, names of {MAX_PREFIX_NAME_BYTES} bytes, IRIs of {MAX_PREFIX_IRI_BYTES})",
                self.opts.max_prefixes
            );
        }
        if cur.len() != before
            && let Some(root) = &self.root
        {
            write_atomic(
                &root.join("prefixes.json"),
                &serde_json::to_vec_pretty(&*cur).unwrap(),
            )?;
        }
        Ok(())
    }

    /// Set (or replace) one prefix. Prefixes are metadata: no commit is made. A new
    /// prefix past [`StoreOptions::max_prefixes`] is refused.
    pub fn set_prefix(&self, prefix: &str, iri: &str) -> Result<()> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        if prefix.len() > MAX_PREFIX_NAME_BYTES || !valid_prefix_name(prefix) {
            return Err(Error::invalid(format!("invalid prefix name {prefix:?}")));
        }
        if iri.len() > MAX_PREFIX_IRI_BYTES {
            return Err(Error::invalid(format!(
                "prefix IRI longer than {MAX_PREFIX_IRI_BYTES} bytes"
            )));
        }
        oxrdf::NamedNode::new(iri)
            .map_err(|e| Error::invalid(format!("invalid IRI {iri:?}: {e}")))?;
        let mut cur = self.prefixes.lock();
        if cur.get(prefix).map(String::as_str) == Some(iri) {
            return Ok(());
        }
        if !cur.contains_key(prefix) && self.prefixes_full(cur.len()) {
            return Err(Error::invalid(format!(
                "the dataset has {} prefixes, the most allowed; remove one first",
                cur.len()
            )));
        }
        let mut next = cur.clone();
        next.insert(prefix.to_string(), iri.to_string());
        self.save_prefixes(&next)?;
        *cur = next;
        Ok(())
    }

    /// Remove one prefix; returns whether it was defined.
    pub fn remove_prefix(&self, prefix: &str) -> Result<bool> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        let mut cur = self.prefixes.lock();
        if !cur.contains_key(prefix) {
            return Ok(false);
        }
        let mut next = cur.clone();
        next.remove(prefix);
        self.save_prefixes(&next)?;
        *cur = next;
        Ok(true)
    }

    /// Whether `n` prefixes leave no room for another.
    fn prefixes_full(&self, n: usize) -> bool {
        self.opts.max_prefixes > 0 && n >= self.opts.max_prefixes
    }

    fn save_prefixes(&self, p: &BTreeMap<String, String>) -> Result<()> {
        if let Some(root) = &self.root {
            write_atomic(
                &root.join("prefixes.json"),
                &serde_json::to_vec_pretty(p).unwrap(),
            )?;
        }
        Ok(())
    }

    /// Begin the (single) write transaction; blocks while another writer is active.
    pub fn write(&self) -> WriteTxn<'_> {
        self.write_as(CommitKind::Transaction)
    }

    /// Begin the write transaction, recording its commit as `kind`.
    pub fn write_as(&self, kind: CommitKind) -> WriteTxn<'_> {
        self.write_with(kind, Default::default())
    }

    /// [`write_as`](Self::write_as) with options for the write guard.
    pub fn write_with(&self, kind: CommitKind, opts: crate::guard::WriteOptions) -> WriteTxn<'_> {
        let waiting = Waiting::new(&self.writers_waiting);
        crate::sparql::extensions::assert_family(self.owner_dataset_id());
        let mut guard = self.writer.lock();
        if !self.group_admission(kind, &opts, &guard) {
            self.drain_group(&mut guard, None).ok();
        }
        drop(waiting);
        self.begin(guard, kind, opts)
    }

    /// Write transactions waiting for the writer lock right now. A long holder of the
    /// lock, such as an automatic reasoning run, can give way to them.
    pub fn writers_waiting(&self) -> usize {
        self.writers_waiting.load(Ordering::Relaxed)
    }

    /// [`write_with`](Self::write_with), but a write whose `opts` are cancelled or past
    /// their deadline stops waiting for the writer lock (see
    /// [`WriteOptions::check`](crate::guard::WriteOptions::check)).
    pub fn try_write_with(
        &self,
        kind: CommitKind,
        opts: crate::guard::WriteOptions,
    ) -> Result<WriteTxn<'_>> {
        let mut guard = self.raw_writer(&opts)?;
        if !self.group_admission(kind, &opts, &guard) {
            self.drain_group(&mut guard, Some(&opts))?;
        }
        self.check_write_options(&opts)?;
        if guard.poisoned {
            return Err(Error::Poisoned);
        }
        Ok(self.begin(guard, kind, opts))
    }

    /// The writer lock, waited for in slices while `o` can be cancelled or time out.
    /// Then the write's precondition, if it has one, is checked on the head snapshot.
    fn raw_writer(&self, o: &crate::guard::WriteOptions) -> Result<MutexGuard<'_, WriterState>> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        let w = {
            // counted while it waits, so a long holder of the lock can give way
            let _waiting = Waiting::new(&self.writers_waiting);
            if o.no_wait {
                o.check()?;
                self.writer.try_lock().ok_or(Error::WriterBusy)?
            } else if o.cancel.is_none() && o.deadline.is_none() {
                self.writer.lock()
            } else {
                loop {
                    o.check()?;
                    if let Some(w) = self
                        .writer
                        .try_lock_for(std::time::Duration::from_millis(20))
                    {
                        break w;
                    }
                }
            }
        };
        Ok(w)
    }

    fn lock_writer(&self, o: &crate::guard::WriteOptions) -> Result<MutexGuard<'_, WriterState>> {
        let mut w = self.raw_writer(o)?;
        self.drain_group(&mut w, Some(o))?;
        self.check_write_options(o)?;
        Ok(w)
    }

    fn check_write_options(&self, o: &crate::guard::WriteOptions) -> Result<()> {
        // a dry run reports the precondition with the rest of its preview
        if let Some(p) = &o.precondition
            && o.dry_run.is_none()
        {
            p.check(&self.snapshot())?;
        }
        if let Some(m) = &o.message {
            crate::annotations::validate_message(m)?;
        }
        Ok(())
    }

    fn drain_group(
        &self,
        w: &mut WriterState,
        o: Option<&crate::guard::WriteOptions>,
    ) -> Result<()> {
        if let Some(group) = &self.group {
            if let Err(e) = group.drain(self, o) {
                if group.failed() {
                    w.poisoned = true;
                    let durable = self.current.load().commit;
                    if let Some(head) = self.catalog.lock().get(durable) {
                        w.head = head;
                    }
                    w.staged = None;
                }
                return Err(e);
            }
            w.staged = None;
        }
        Ok(())
    }

    fn group_admission(
        &self,
        kind: CommitKind,
        o: &crate::guard::WriteOptions,
        w: &WriterState,
    ) -> bool {
        self.group.is_some()
            && !w.poisoned
            && w.wal.is_some()
            && w.tap.is_none()
            && matches!(kind, CommitKind::Transaction | CommitKind::Update)
            && !o.bypass_validation
            && o.dry_run.is_none()
            && o.message.is_none()
            && o.precondition.is_none()
            && o.graphs.is_none()
            && !self.commit_digests()
            && !self.guard_required()
            && self.guard.read().is_none()
            && !self.text_enabled()
            && self.geo.load().is_none()
            && self.embed.works.lock().is_empty()
    }

    /// What stops a rebuild into `dir` early: the write's cancellation and deadline, and
    /// the free disk space the store keeps.
    fn build_interrupt(
        &self,
        o: Option<crate::guard::WriteOptions>,
        dir: &Path,
    ) -> Option<crate::builder::InterruptFn> {
        let o = o.filter(|o| o.cancel.is_some() || o.deadline.is_some());
        let reserve = self.root.as_ref().and(self.opts.min_free_disk_bytes);
        if o.is_none() && reserve.is_none() {
            return None;
        }
        let dir = dir.to_path_buf();
        Some(Arc::new(move || {
            if let Some(o) = &o {
                o.check()?;
            }
            match reserve {
                Some(r) => crate::disk::check_reserve(&dir, r, 0, false),
                None => Ok(()),
            }
        }))
    }

    /// Persistent stores keep [`StoreOptions::min_free_disk_bytes`] free: `StorageFull`
    /// when writing `need` more bytes would go below it.
    fn check_disk(&self, need: u64) -> Result<()> {
        match (&self.root, self.opts.min_free_disk_bytes) {
            (Some(root), Some(reserve)) => crate::disk::check_reserve(root, reserve, need, true),
            _ => Ok(()),
        }
    }

    /// Before `quads` parsed quads are encoded into a transaction of an in-memory store
    /// (which adds their terms to its vocabulary even if the commit is refused later):
    /// `StorageFull` when their delta alone would pass the limit.
    fn check_memory_before(&self, quads: usize) -> Result<()> {
        if self.root.is_some() || self.opts.max_memory_bytes.is_none() {
            return Ok(());
        }
        let snap = self.snapshot();
        let now = snap.generation.disk_bytes()
            + delta_bytes(&snap.delta)
            + snap.generation.dvocab.with(|v| v.bytes()) as u64;
        self.check_memory(now + quads as u64 * DELTA_QUAD_BYTES)
    }

    /// In-memory stores stay within [`StoreOptions::max_memory_bytes`]: `StorageFull`
    /// when a commit would make the data about `size` bytes.
    fn check_memory(&self, size: u64) -> Result<()> {
        match self.opts.max_memory_bytes {
            Some(max) if self.root.is_none() && size > max => {
                let h = crate::error::human_bytes;
                Err(Error::StorageFull(format!(
                    "the in-memory dataset would grow to about {}, over its limit of {}",
                    h(size),
                    h(max)
                )))
            }
            _ => Ok(()),
        }
    }

    fn begin<'a>(
        &'a self,
        guard: MutexGuard<'a, WriterState>,
        kind: CommitKind,
        opts: crate::guard::WriteOptions,
    ) -> WriteTxn<'a> {
        let base = guard.staged.clone().unwrap_or_else(|| self.snapshot());
        let mark = base.generation.dvocab.mark();
        let start_bnode = guard.next_bnode;
        WriteTxn {
            _callback_owner: crate::sparql::extensions::WriterOwner::enter(self.owner_dataset_id()),
            store: self,
            delta: base.delta.clone(),
            base,
            mark,
            start_bnode,
            guard: TxnGuard(Some(guard)),
            log: Vec::new(),
            bulk: Vec::new(),
            kind,
            net_ins: 0,
            net_del: 0,
            requested: opts
                .graphs
                .as_ref()
                .and_then(|a| a.triples.as_ref())
                .is_some_and(|t| t.limits_writes())
                .then(Vec::new),
            base_check: None,
            opts,
            writable: Default::default(),
            force: false,
            merge: None,
        }
    }

    /// Install (or remove) the write guard run before every commit.
    pub fn set_guard(&self, g: Option<Arc<dyn crate::guard::CommitGuard>>) {
        crate::sparql::extensions::assert_family(self.owner_dataset_id());
        *self.guard.write() = g;
    }

    pub fn guard(&self) -> Option<Arc<dyn crate::guard::CommitGuard>> {
        self.guard.read().clone()
    }

    /// Install (or remove) the observer told the outcome of every guard decision; it
    /// stays when the guard itself is replaced.
    pub fn set_guard_observer(&self, o: Option<Arc<dyn crate::guard::GuardObserver>>) {
        crate::sparql::extensions::assert_family(self.owner_dataset_id());
        *self.guard_observer.write() = o;
    }

    /// The dataset's `validation.json` requires a write guard.
    pub fn guard_required(&self) -> bool {
        self.guard_required.load(Ordering::Relaxed)
    }

    /// Say why the guard this dataset requires could not be installed (such as a build
    /// without the validator's feature). Commits refused for want of the guard then
    /// fail with [`Error::GuardMissing`] carrying this reason.
    pub fn set_guard_missing_reason(&self, reason: Option<String>) {
        crate::sparql::extensions::assert_family(self.owner_dataset_id());
        *self.guard_missing_reason.write() = reason;
    }

    /// Mark whether this dataset requires a guard (set with its configuration).
    pub fn set_guard_required(&self, required: bool) {
        crate::sparql::extensions::assert_family(self.owner_dataset_id());
        self.guard_required.store(required, Ordering::Relaxed);
    }

    /// Run the write guard on a candidate commit (writer lock held): `Ok(None)` when no
    /// guard applies, the summary when it passes, [`Error::Rejected`] when it rejects.
    fn run_guard(
        &self,
        base: &Snapshot,
        view: impl FnOnce() -> Arc<Snapshot>,
        kind: CommitKind,
        changes: crate::guard::Changes<'_>,
        opts: &crate::guard::WriteOptions,
        head: u64,
    ) -> Result<Option<Arc<crate::guard::ValidationSummary>>> {
        use crate::guard::{GuardLanguage, GuardMode, GuardStatus, Severity, ValidationSummary};
        let g = self.guard();
        let observer = self.guard_observer.read().clone();
        let language = g.as_ref().map_or(GuardLanguage::Shacl, |g| g.language());
        // a store opened to write without its required guard bypasses it on every write
        let unguarded = g.is_none() && self.guard_required() && self.opts.unvalidated_writes;
        if opts.bypass_validation || unguarded {
            let mut summary = ValidationSummary::empty(
                GuardStatus::Bypassed,
                GuardMode::Off,
                Severity::Violation,
            );
            summary.language = language;
            // a dry run bypasses nothing: the guard, the metrics and the log see no write
            if opts.dry_run.is_some() {
                return Ok(Some(Arc::new(summary)));
            }
            if let Some(g) = &g {
                g.bypassed();
            }
            if let Some(o) = &observer
                && (g.is_some() || self.guard_required())
            {
                o.observe(language, kind, Ok(&summary), std::time::Duration::ZERO);
            }
            tracing::warn!(target: "sparkles::store", "a write bypassed write-time validation");
            return Ok(Some(Arc::new(summary)));
        }
        let Some(g) = g else {
            if self.guard_required() && !self.opts.unvalidated_writes {
                return Err(Error::GuardMissing(
                    match self.guard_missing_reason.read().as_deref() {
                        Some(why) => format!(
                            "dataset requires write-time validation (validation.json), and its guard could not be installed: {why}"
                        ),
                        None => "dataset requires write-time validation (validation.json); install its guard (sparkles_shacl::guard::install or sparkles_shex::guard::install) or allow unvalidated writes".into(),
                    },
                ));
            }
            return Ok(None);
        };
        let view = view();
        let t0 = std::time::Instant::now();
        let checked = g.check(&crate::guard::Candidate {
            base,
            view,
            kind,
            changes,
            opts,
        });
        if let Some(o) = &observer
            && opts.dry_run.is_none()
        {
            o.observe(language, kind, checked.as_ref(), t0.elapsed());
        }
        let summary = checked?;
        if summary.status == GuardStatus::Rejected {
            return Err(Error::Rejected(Box::new(crate::guard::Rejection {
                summary,
                head,
                kind,
            })));
        }
        Ok(Some(Arc::new(summary)))
    }

    /// Tell the guard a commit it checked was published.
    fn guard_committed(&self, seq: u64) {
        if let Some(g) = self.guard() {
            g.committed(seq);
        }
    }

    /// Load RDF sources. Into an empty store (or for large inputs) this runs the bulk
    /// builder and rebuilds the base generation; small loads are transactional inserts.
    /// Returns the number of new quads.
    pub fn load(&self, sources: &[Source]) -> Result<u64> {
        let r = self.load_as(sources, CommitKind::Load)?;
        Ok(if r.committed { r.commit.inserted } else { 0 })
    }

    /// [`load`](Self::load), recording the commit as `kind`.
    pub fn load_as(&self, sources: &[Source], kind: CommitKind) -> Result<Receipt> {
        self.load_with(sources, kind, &Default::default())
    }

    /// [`load_as`](Self::load_as) with options for the write guard.
    pub fn load_with(
        &self,
        sources: &[Source],
        kind: CommitKind,
        o: &crate::guard::WriteOptions,
    ) -> Result<Receipt> {
        let snap = self.snapshot();
        // a write limited to some graphs checks each quad, which a rebuild does not
        if o.graphs.is_none()
            && (snap.is_empty() || estimated_quads(sources) > self.opts.bulk_threshold)
        {
            let mut w = self.lock_writer(o)?;
            if w.poisoned {
                return Err(Error::Poisoned);
            }
            let snap = self.snapshot();
            let bulk = BulkCommit {
                kind,
                net_del: 0,
                start_len: snap.len(),
                default_graph: sources_reach_default_graph(sources),
            };
            let check = Some((crate::guard::Changes::Unknown, o));
            Ok(self
                .rebuild_locked(&mut w, &snap, sources, &[], &[], Some(bulk), check)?
                .1)
        } else {
            let mut txn = self.try_write_with(kind, o.clone())?;
            let mut prefixes = BTreeMap::new();
            let mut parsed = 0;
            let prepared = (|| -> Result<()> {
                for s in sources {
                    o.check()?;
                    let mut labels = std::collections::HashMap::new();
                    let p = crate::io::parse_source_into(s, |q| {
                        parsed += 1;
                        if parsed % 65_536 == 1 {
                            o.check()?;
                        }
                        self.check_memory_before(parsed)?;
                        let ids = txn.encode_quad(&q, &mut labels)?;
                        txn.insert(ids)?;
                        Ok(())
                    })?;
                    prefixes.extend(p);
                }
                Ok(())
            })();
            if let Err(error) = prepared {
                // No uncommitted views escape this loader. Remove its terms while
                // holding the writer lock; earlier group-commit terms precede mark.
                if let Err(rollback_error) = txn.base.generation.dvocab.rollback(&txn.mark) {
                    // A partial file rollback may leave append state inconsistent.
                    // Refuse subsequent writes until the store is reopened.
                    txn.guard.poisoned = true;
                    tracing::error!(target: "sparkles::store", "failed load ({error}) could not remove its terms: {rollback_error}");
                    return Err(rollback_error);
                }
                txn.guard.next_bnode = txn.start_bnode;
                return Err(error);
            }
            let r = txn.commit()?;
            self.add_prefixes(prefixes)?;
            Ok(r)
        }
    }

    /// Replace a graph (or the whole dataset) with the contents of `sources` in one
    /// transaction. The sources are parsed completely before anything changes, so a
    /// parse error leaves the data untouched, and readers see either the old or the new
    /// content, never the cleared graph. Returns the number of quads parsed.
    pub fn replace(&self, target: ReplaceTarget, sources: &[Source]) -> Result<u64> {
        Ok(self.replace_as(target, sources, CommitKind::Transaction)?.0)
    }

    /// [`replace`](Self::replace), recording the commit as `kind`; also returns the
    /// receipt.
    pub fn replace_as(
        &self,
        target: ReplaceTarget,
        sources: &[Source],
        kind: CommitKind,
    ) -> Result<(u64, Receipt)> {
        self.replace_with(target, sources, kind, &Default::default())
    }

    /// [`replace_as`](Self::replace_as) with options for the write guard.
    pub fn replace_with(
        &self,
        target: ReplaceTarget,
        sources: &[Source],
        kind: CommitKind,
        o: &crate::guard::WriteOptions,
    ) -> Result<(u64, Receipt)> {
        if let Some(a) = &o.graphs {
            // the target is checked before it is looked up, so the answer does not
            // depend on whether it exists
            match &target {
                ReplaceTarget::Default if !a.writable(None) => {
                    return Err(crate::access::GraphAccess::refused(None));
                }
                ReplaceTarget::Named(n) if !a.writable_iri(n.as_str()) => {
                    return Err(crate::access::GraphAccess::refused_iri(n.as_str()));
                }
                ReplaceTarget::All
                    if !(a.read.is_all() && a.write.is_all() && a.triples.is_none()) =>
                {
                    return Err(Error::NotPermitted(
                        "replacing the whole dataset needs write access to every graph".into(),
                    ));
                }
                _ => {}
            }
        } else if estimated_quads(sources) > self.opts.bulk_threshold {
            return self.replace_bulk(target, sources, kind, o);
        }
        let mut parsed = Vec::with_capacity(sources.len());
        let mut prefixes = BTreeMap::new();
        for s in sources {
            o.check()?;
            let (quads, p) = crate::io::parse_to_vec(s)?;
            prefixes.extend(p);
            parsed.push(quads);
        }
        self.check_memory_before(parsed.iter().map(Vec::len).sum())?;
        let mut txn = self.try_write_with(kind, o.clone())?;
        let view = txn.view();
        let graphs: Vec<Id> = match &target {
            ReplaceTarget::Default => vec![Id::DEFAULT_GRAPH],
            ReplaceTarget::Named(n) => view
                .lookup_term(&Term::NamedNode(n.clone()))
                .into_iter()
                .collect(),
            ReplaceTarget::All => {
                let mut v = view.graph_ids()?;
                v.push(Id::DEFAULT_GRAPH);
                v
            }
        };
        let mut ids = Vec::new();
        for quads in &parsed {
            o.check()?;
            let mut labels = std::collections::HashMap::new();
            for q in quads {
                ids.push(txn.encode_quad(q, &mut labels)?);
            }
        }
        // Only the old quads the new content lacks are deleted, and inserting a quad
        // the graph still has changes nothing, so the transaction logs the difference
        // alone. The result is the same as clearing the graphs first.
        let keep: rustc_hash::FxHashSet<[Id; 4]> = ids.iter().copied().collect();
        // a view that hides triples replaces the ones it sees, and leaves the rest
        let read = txn.read_view()?;
        for g in graphs {
            for (i, k) in read.scan_keys(Perm::Gspo, &[g.0])?.into_iter().enumerate() {
                if i % 65_536 == 65_535 {
                    o.check()?;
                }
                let q = Perm::Gspo.to_quad(&k);
                if !keep.contains(&q) {
                    txn.delete(q)?;
                }
            }
        }
        drop(keep);
        let n = ids.len() as u64;
        txn.insert_bulk(ids)?;
        let r = txn.commit()?;
        self.add_prefixes(prefixes)?;
        Ok((n, r))
    }

    /// A large replace: one rebuild that leaves out the target graphs and adds the
    /// sources, parsed as a stream by the bulk builder. Atomic like every rebuild: a parse
    /// error leaves the store as it was.
    fn replace_bulk(
        &self,
        target: ReplaceTarget,
        sources: &[Source],
        kind: CommitKind,
        o: &crate::guard::WriteOptions,
    ) -> Result<(u64, Receipt)> {
        let mut w = self.lock_writer(o)?;
        if w.poisoned {
            return Err(Error::Poisoned);
        }
        let snap = self.snapshot();
        let graphs: Vec<Id> = match &target {
            ReplaceTarget::Default => vec![Id::DEFAULT_GRAPH],
            ReplaceTarget::Named(n) => snap.lookup_iri(n.as_str()).into_iter().collect(),
            ReplaceTarget::All => {
                let mut v = snap.graph_ids()?;
                v.push(Id::DEFAULT_GRAPH);
                v
            }
        };
        let mut dropped = 0;
        for g in &graphs {
            dropped += snap.count(Perm::Gspo, &[g.0])?;
        }
        let start_len = snap.len();
        let bulk = BulkCommit {
            kind,
            net_del: dropped,
            start_len,
            default_graph: graphs.contains(&Id::DEFAULT_GRAPH)
                || sources_reach_default_graph(sources),
        };
        let check = Some((crate::guard::Changes::Unknown, o));
        let (_, r) =
            self.rebuild_locked(&mut w, &snap, sources, &[], &graphs, Some(bulk), check)?;
        // the quads the replacement holds: what is left, less what was kept
        Ok(((r.commit.quads + dropped).saturating_sub(start_len), r))
    }

    /// Compact: merge base ⊕ delta into a new generation. The data does not change, so
    /// neither does the head commit. Writes go on during the build (see
    /// [`compact_with`](Self::compact_with)).
    pub fn compact(&self) -> Result<()> {
        self.compact_with(&CompactOptions::default())?;
        Ok(())
    }

    /// Rebuild with the writer lock held: a new generation from `snap` plus `extra`
    /// sources and `extra_quads` (encoded store ids from a bulk write transaction).
    /// With `bulk`, the rebuild is a new commit; without it (compaction), the head stays.
    /// Returns the number of new quads and the receipt.
    #[allow(clippy::too_many_arguments)]
    fn rebuild_locked(
        &self,
        w: &mut WriterState,
        snap: &Snapshot,
        extra: &[Source],
        extra_quads: &[[Id; 4]],
        drop_graphs: &[Id],
        bulk: Option<BulkCommit>,
        check: Option<(crate::guard::Changes<'_>, &crate::guard::WriteOptions)>,
    ) -> Result<(u64, Receipt)> {
        if let Some(b) = &bulk {
            self.check_protected(b.kind)?;
        }
        let started = std::time::Instant::now();
        let before = snap.len();
        // the head state before a bulk commit, whose changes the change log records
        let prior = self.snapshot();
        // a running compaction gives up: this rebuild makes a new generation itself
        w.tap = None;
        if bulk.is_some() {
            self.compaction.rebuilding.store(true, Ordering::Relaxed);
        }
        let _rebuilding = bulk
            .is_some()
            .then(|| compaction::Rebuilding(&self.compaction.rebuilding));
        let message = match (&bulk, check.as_ref().and_then(|(_, o)| o.message.as_ref())) {
            (Some(_), Some(m)) => crate::annotations::validate_message(m)?,
            _ => None,
        };
        // a dry run builds and checks the generation, then removes it (see `preview`)
        let dry = match (&bulk, &check) {
            (Some(_), Some((_, o))) => o.dry_run.clone(),
            _ => None,
        };
        if dry.is_none() {
            // the old generation's WAL is the only other copy of the recent commits' ids:
            // the catalog must be durable before it is discarded
            self.catalog.lock().sync()?;
            // and so must the change log, which recovers its tail from that WAL
            self.sync_change_log()?;
            // and so must the full-text index, which could otherwise only catch up from it
            #[cfg(feature = "text")]
            if let Some(ti) = self.text.load_full() {
                ti.checkpoint()?;
            }
        }
        let (dir, name, tmp) = match &self.root {
            Some(root) => {
                // above a number a background compaction is building in
                let n: u32 = snap
                    .generation
                    .name
                    .strip_prefix("gen-")
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(0)
                    .max(self.compaction.reserved.load(Ordering::Relaxed))
                    + 1;
                let name = format!("gen-{n:04}");
                let dir = root.join(&name);
                if dir.exists() {
                    std::fs::remove_dir_all(&dir)?;
                }
                (dir, name, None)
            }
            None => {
                let t = tempfile::Builder::new().prefix("sparkles-mem-").tempdir()?;
                (t.path().to_path_buf(), "mem".to_string(), Some(t))
            }
        };
        let mut bopts = self.opts.build.clone();
        bopts.first_bnode = w.next_bnode;
        let wopts = check.as_ref().map(|(_, o)| (*o).clone());
        let interrupt = self.build_interrupt(wopts.clone(), &dir);
        let built = (|| {
            let mut builder = Builder::new(&dir, bopts)?;
            if let Some(i) = interrupt.clone() {
                builder = builder.with_interrupt(i);
            }
            write_snapshot(
                &builder,
                snap,
                None,
                |q| Ok(!drop_graphs.contains(&q[3])),
                extra_quads,
            )?;
            for s in extra {
                builder.add_source(s)?;
            }
            builder.add_prefixes(self.prefixes());
            let meta = builder.finish()?;
            // a write stopped while it was built publishes nothing
            if let Some(i) = &interrupt {
                i()?;
            }
            let mut storage = crate::preview::StorageCheck::default();
            if bulk.is_some() {
                let old = snap.generation.dir.as_deref();
                if dry.is_some() {
                    storage = self.rebuild_storage(old, &dir);
                }
                let fits = self
                    .check_memory(dir_size(&dir))
                    .and_then(|_| self.quota.check_rebuild(old, &dir));
                match fits {
                    Err(e) if dry.is_some() => storage.refused = Some(e),
                    r => r?,
                }
            }
            Ok((meta, storage))
        })();
        let (meta, storage) = match built {
            Ok(m) => m,
            Err(e) => {
                // the unfinished generation's space is freed now, not at the next rebuild
                if self.root.is_some() {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                return Err(e);
            }
        };
        let mut gen_ = Generation::open(&dir, &name, self.root.is_some())?;
        gen_._tmp = tmp;
        let gen_ = Arc::new(gen_);
        if dry.is_none() {
            w.next_bnode = w.next_bnode.max(meta.next_bnode);
        }
        let head_seq = w.head.seq;
        let candidate = || {
            Arc::new(Snapshot {
                dataset_id: self.owner_dataset_id(),
                generation: gen_.clone(),
                delta: Delta::default(),
                version: 0,
                cache: self.cache.clone(),
                results: Arc::new(crate::sparql::cache::ResultCache::new(0, 0.0)),
                dvocab_len: gen_.dvocab.len(),
                commit: head_seq,
                text: None,
                geo: None,
                union_default_graph: self.opts.union_default_graph,
                geo_op_vertices: self.opts.geo_op_vertices,
                delta_stats: Default::default(),
                counts: Default::default(),
                mask: None,
                historical: false,
                change_log: self.changelog.clone(),
            })
        };
        // a bulk commit is validated on the built generation, before anything is published
        let validation = match (&bulk, check) {
            (Some(b), Some((changes, o))) => {
                match self.run_guard(snap, candidate, b.kind, changes, o, head_seq) {
                    Ok(v) => v,
                    Err(Error::Rejected(r)) if dry.is_some() => Some(Arc::new(r.summary)),
                    Err(e) => {
                        drop(gen_);
                        if self.root.is_some() {
                            let _ = std::fs::remove_dir_all(&dir);
                        }
                        return Err(e);
                    }
                }
            }
            _ => None,
        };
        let head = match &bulk {
            Some(b) => CommitInfo {
                seq: w.head.seq + 1,
                timestamp_ms: self.commit_time(&w.head),
                kind: b.kind,
                inserted: (meta.quads + b.net_del).saturating_sub(b.start_len),
                deleted: b.net_del,
                quads: meta.quads,
                generation: commit::generation_number(&name),
                bulk: true,
                exact: b.net_del == 0,
                reconstructed: false,
                default_graph: b.default_graph,
                unvalidated: validation
                    .as_ref()
                    .is_some_and(|v| v.status == crate::guard::GuardStatus::Bypassed),
            },
            None => w.head,
        };
        if let (Some(dr), Some((changes, o))) = (&dry, check) {
            let preview = self.preview_bulk(crate::store::preview::BulkPreview {
                head: w.head,
                view: snap,
                candidate: &candidate(),
                commit: head,
                changes,
                drop_graphs,
                opts: o,
                dr,
                validation,
                storage,
                message,
            });
            drop(gen_);
            if self.root.is_some() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            // a walk of the directory during the build counted the candidate
            self.quota.invalidate();
            return Err(Error::DryRun(Box::new(preview?)));
        }
        let mut annotation = crate::annotations::Annotation::default();
        if let Some(root) = &self.root {
            // Publication order: the new generation's files (with the commit its base
            // holds) and directory entries are durable, its WAL file exists durably, then
            // CURRENT switches durably, and only after that is the old generation removed.
            let origin = if bulk.is_some() { "bulk" } else { "compaction" };
            write_synced(
                &dir.join("commit.json"),
                &commit::gen_commit_bytes(self.dataset_id, origin, &head),
            )?;
            let wal = wal::open_for_append(&dir.join("wal.log"))?;
            sync_dir(&dir)?;
            sync_dir(root)?;
            // a bulk commit's message is durable before the switch (no digest: its
            // change set is not at hand)
            if bulk.is_some() {
                annotation = self.annotate(&head, message.clone(), || None)?;
            }
            if let Err(e) = write_atomic(&root.join("CURRENT"), name.as_bytes()) {
                if !annotation.is_empty() && self.annotations.lock().undo(head.seq).is_err() {
                    w.poisoned = true;
                }
                return Err(e);
            }
            w.trim_wal();
            w.wal_alloc = wal.metadata()?.len();
            w.wal = Some(BufWriter::new(wal));
            w.wal_len = 0;
            self.wal_end.store(0, Ordering::Relaxed);
            self.quota.set_preallocated(0);
            *gen_.wal_index.lock() = Some(wal::WalIndex::new(head.seq, false));
        } else if bulk.is_some() {
            annotation = self.annotate(&head, message.clone(), || None)?;
        }
        // the switch is the commit point: a failure after it leaves the published state
        // behind the durable one, so later writes are refused
        if bulk.is_some() {
            w.head = head;
            self.catalog.lock().append(head);
            self.forget_annotations();
        }
        self.compaction
            .bulk_committed(head.seq, head.timestamp_ms, started.elapsed());
        if let Err(e) = self.add_prefixes(meta.prefixes.clone()) {
            w.poisoned = true;
            return Err(e);
        }
        let old = snap.generation.dir.clone();
        let old_name = snap.generation.name.clone();
        let dvocab_len = gen_.dvocab.len();
        let mut new_snap = Snapshot {
            dataset_id: self.owner_dataset_id(),
            generation: gen_,
            delta: Delta::default(),
            version: snap.version + 1,
            cache: self.cache.clone(),
            results: self.results.clone(),
            dvocab_len,
            commit: head.seq,
            text: if bulk.is_some() {
                None
            } else {
                snap.text.clone()
            },
            geo: None,
            union_default_graph: self.opts.union_default_graph,
            geo_op_vertices: self.opts.geo_op_vertices,
            delta_stats: Default::default(),
            counts: Default::default(),
            mask: None,
            historical: false,
            change_log: self.changelog.clone(),
        };
        if bulk.is_some() {
            self.rebuild_text_locked(&mut new_snap, &prior);
            // the bulk commit's changes, while the state before it is at hand
            let author = check.as_ref().and_then(|(_, o)| o.author.clone());
            self.log_bulk_commit(&head, message.clone(), author, &prior, &new_snap);
        }
        // a new generation needs its own spatial base (bulk commits and compactions)
        self.rebuild_geo_locked(&mut new_snap, snap);
        if bulk.is_some() {
            self.remember_past(head.seq);
        }
        self.current.store(Arc::new(new_snap));
        self.commits.send_replace(head.seq);
        // and its own vector indexes, built in the background
        self.vectors_switched(snap);
        if bulk.is_some() {
            // a bulk commit has no log to schedule embeddings from
            self.embed_bulk(head.seq);
        }
        // The old generation is kept if history needs it, else removed; open readers
        // keep their mmaps alive.
        if let (Some(root), Some(old)) = (&self.root, old)
            && old.starts_with(root)
            && old != dir
        {
            match &self.history {
                Some(h) => {
                    let mut h = h.lock();
                    let (old_no, new_no) = (
                        commit::generation_number(&old_name),
                        commit::generation_number(&name),
                    );
                    if let Some(g) = h.gens.get_mut(&old_no) {
                        g.end = snap.commit;
                        g.bytes = dir_size(&g.dir);
                    }
                    h.gens.insert(
                        new_no,
                        crate::history::GenEntry {
                            name: name.clone(),
                            dir: dir.clone(),
                            base: head,
                            end: head.seq,
                            fold_legacy: false,
                            bytes: 0,
                        },
                    );
                    self.collect_locked(&mut h, new_no, head.seq);
                }
                None => {
                    let _ = std::fs::remove_dir_all(old);
                }
            }
        }
        if validation.is_some() && bulk.is_some() {
            self.guard_committed(head.seq);
        }
        if bulk.is_none() {
            annotation = self.annotation(head.seq).unwrap_or_default();
        }
        // a linked branch that rebuilt no longer reads its upstream's files
        self.release_link_if_rebuilt();
        // a new generation (and maybe one fewer old one): measure the directory again
        self.quota.invalidate();
        let receipt = Receipt {
            dataset_id: self.owner_dataset_id(),
            committed: bulk.is_some(),
            commit: head,
            validation,
            annotation,
        };
        Ok((meta.quads.saturating_sub(before), receipt))
    }

    /// Build the generation directory `gdir` from `snap`: every quad `keep` accepts (of
    /// the graphs `graphs` only, when given), the `prefixes`, and new blank nodes
    /// numbered from `next_bnode` on. Building stops with `507` once the file system
    /// would keep less than `reserve` bytes free. `indexing` is called between writing
    /// the quads and building the indexes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_from_snapshot(
        &self,
        gdir: &Path,
        snap: &Snapshot,
        graphs: Option<&[u64]>,
        next_bnode: u64,
        reserve: Option<u64>,
        prefixes: BTreeMap<String, String>,
        keep: impl FnMut(&[Id; 4]) -> Result<bool>,
        indexing: impl FnOnce(),
    ) -> Result<IndexMeta> {
        let mut bopts = self.opts.build.clone();
        bopts.first_bnode = next_bnode;
        let mut builder = Builder::new(gdir, bopts)?;
        if let Some(reserve) = reserve {
            let gdir = gdir.to_path_buf();
            builder = builder.with_interrupt(Arc::new(move || {
                crate::disk::check_reserve(&gdir, reserve, 0, false)
            }));
        }
        write_snapshot(&builder, snap, graphs, keep, &[])?;
        indexing();
        builder.add_prefixes(prefixes);
        builder.finish()
    }

    /// The configuration files of the indexes that a copy of this store rebuilds when
    /// it is opened: `text.json` (full-text search) and `geo.json` (the spatial index),
    /// when they are on. An in-memory store renders them from its running indexes.
    pub(crate) fn index_config_files(&self) -> Result<Vec<(&'static str, Vec<u8>)>> {
        let read = |file: &str| -> Result<Option<Vec<u8>>> {
            let Some(root) = &self.root else {
                return Ok(None);
            };
            match std::fs::read(root.join(file)) {
                Ok(b) => Ok(Some(b)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            }
        };
        let mut out = Vec::new();
        #[cfg(feature = "text")]
        let text_cfg = self
            .text
            .load()
            .as_ref()
            .map(|ti| serde_json::to_vec_pretty(ti.config()).unwrap());
        #[cfg(not(feature = "text"))]
        let text_cfg = read("text.json")?;
        if let Some(cfg) = text_cfg {
            out.push(("text.json", cfg));
        }
        let geo_cfg = match &self.root {
            Some(_) => read(crate::geo::CONFIG_FILE)?,
            None => self
                .geo_status()
                .map(|s| serde_json::to_vec_pretty(&s.config).unwrap()),
        };
        if let Some(cfg) = geo_cfg {
            out.push((crate::geo::CONFIG_FILE, cfg));
        }
        // the vector indexes are built again where the copy opens
        let vector_cfg = self.vector_configs();
        if !vector_cfg.is_empty() {
            let file = crate::vector::VectorConfigFile {
                indexes: vector_cfg,
                ..Default::default()
            };
            out.push((
                crate::vector::config::CONFIG_FILE,
                serde_json::to_vec_pretty(&file).unwrap(),
            ));
        }
        Ok(out)
    }

    /// `forkedFrom` of a database made by [`clone_to`](Self::clone_to) (or restored
    /// from a backup under a new identity).
    pub fn forked_from(&self) -> Option<ForkedFrom> {
        if let Some(f) = self.forked_mem {
            return Some(f);
        }
        let root = self.root.as_ref()?;
        commit::read_forked_from(root).ok().flatten()
    }

    /// `restoredFrom` of a database restored from a backup.
    pub fn restored_from(&self) -> Option<commit::RestoredFrom> {
        let root = self.root.as_ref()?;
        commit::read_restored_from(root).ok().flatten()
    }

    /// Write all quads as N-Quads to `w`.
    pub fn dump_nquads(&self, w: impl Write) -> Result<u64> {
        dump_snapshot(&self.snapshot().without_cache_fill(), w)
    }

    /// Compressed N-Quads backup into `dir` (Fuseki `/$/backup`), with
    /// [`Codec::dump_default`] (zstd, or gzip in builds without zstd). Returns the file path.
    pub fn backup(&self, dir: &Path, name: &str) -> Result<PathBuf> {
        self.backup_with(dir, name, Codec::dump_default(), None, 1)
    }

    /// N-Quads backup into `dir` as `{name}_{timestamp}.nq{codec extension}`, written to a
    /// temporary name first so a failed backup leaves no partial file. Returns the path.
    pub fn backup_with(
        &self,
        dir: &Path,
        name: &str,
        codec: Codec,
        level: Option<Level>,
        threads: usize,
    ) -> Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let ts = crate::builder::now_rfc3339().replace(':', "-");
        let path = dir.join(format!("{name}_{ts}.nq{}", codec.extension()));
        let tmp = tempfile::Builder::new()
            .prefix(".backup-")
            .tempfile_in(dir)?;
        let mut w = codec.writer(BufWriter::new(tmp.as_file()), level, threads)?;
        self.dump_nquads(&mut w)?;
        w.finish()?;
        tmp.as_file().sync_all()?;
        tmp.persist(&path).map_err(|e| Error::Io(e.error))?;
        Ok(path)
    }

    /// Write the sparse vocabulary index (`vocab.idx`) of the current generation when
    /// it has none, because an older version built it. The store opened next uses it.
    /// Returns its number of entries, or `None` when the generation already has one or
    /// the store is in memory.
    pub fn add_vocab_index(&self) -> Result<Option<usize>> {
        let _w = self.guarded_writer();
        let snap = self.snapshot();
        let Some(dir) = snap.generation.dir.as_ref() else {
            return Ok(None);
        };
        if dir.join("vocab.idx").exists() {
            return Ok(None);
        }
        crate::vocab::add_sparse_index(dir).map(Some)
    }

    pub fn disk_bytes(&self) -> u64 {
        match &self.root {
            Some(r) => dir_size(r),
            None => 0,
        }
    }

    /// Size of the current generation's write-ahead log, up to the end of its last
    /// commit, without the space preallocated after it (0 for an in-memory store). It is
    /// read without the writer lock.
    pub fn wal_bytes(&self) -> u64 {
        if self.root.is_none() {
            return 0;
        }
        self.wal_end.load(Ordering::Relaxed)
    }
}

/// Push the quads of `snap` (of the graphs `graphs` only, when given) for which `keep`
/// returns true, then `extra` (store ids of `snap`), into `builder`. Vocabulary ids are
/// passed as their keys; inline and blank-node ids pass through unchanged, which keeps
/// blank-node identity. Triple-term keys embed their blank-node payloads, so blank
/// nodes inside them keep it too.
fn write_snapshot(
    builder: &Builder,
    snap: &Snapshot,
    graphs: Option<&[u64]>,
    mut keep: impl FnMut(&[Id; 4]) -> Result<bool>,
    extra: &[[Id; 4]],
) -> Result<()> {
    if snap.is_empty() && extra.is_empty() {
        return Ok(());
    }
    let scope = builder.new_scope();
    let mut enc = builder.encoder(scope);
    let mut keys: [Vec<u8>; 4] = Default::default();
    let mut push = |q: &[Id; 4]| -> Result<()> {
        for (i, id) in q.iter().enumerate() {
            keys[i].clear();
            if matches!(id.tag(), Tag::Vocab | Tag::Delta) {
                keys[i].extend_from_slice(
                    &snap
                        .key(*id)
                        .ok_or_else(|| Error::Corrupt(format!("dangling id {id:?}")))?,
                );
            }
        }
        let slot = |i: usize| {
            if keys[i].is_empty() {
                Slot::Id(q[i])
            } else {
                Slot::Key(&keys[i])
            }
        };
        enc.push_slots([slot(0), slot(1), slot(2), slot(3)])
    };
    let mut each = |q: &[Id; 4]| if keep(q)? { push(q) } else { Ok(()) };
    match graphs {
        None => snap.for_each_quad(&mut each)?,
        Some(gs) => {
            for &g in gs {
                snap.for_each_quad_in(&[g], &mut each)?;
            }
        }
    }
    for q in extra {
        push(q)?;
    }
    enc.flush()
}

/// Progress callback: (fraction done in `[0, 1]`, message).
pub use crate::task::ProgressFn;

pub(crate) fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Replace `path` durably: write and sync a temporary file, rename it over `path`, then
/// sync the directory so the rename itself survives a power loss.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    write_synced(&tmp, bytes)?;
    std::fs::rename(tmp, path)?;
    sync_dir(path.parent().unwrap_or(Path::new(".")))
}

/// Write a file and flush its contents to stable storage.
pub(crate) fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// Flush a directory's entries (created, renamed or removed files) to stable storage.
/// Directories cannot be opened for syncing on Windows, where this is a no-op.
pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Apply an insert/delete to a delta, preserving the invariants
/// `ins ∩ base = ∅` and `del ⊆ base`.
fn apply(delta: &mut Delta, q: &[Id; 4], insert: bool, in_base: bool) {
    for p in Perm::ALL {
        let k = p.to_key(q);
        let i = p.index();
        // The invariants above determine the only set this operation can change.
        // Even an absent removal can copy shared persistent-tree nodes, so avoid
        // touching the opposite set when the immutable base proves absence.
        match (insert, in_base) {
            (true, false) => {
                delta.ins[i].insert(k);
            }
            (true, true) => {
                delta.del[i].remove(&k);
            }
            (false, false) => {
                delta.ins[i].remove(&k);
            }
            (false, true) => {
                delta.del[i].insert(k);
            }
        }
    }
}

struct TxnGuard<'a>(Option<MutexGuard<'a, WriterState>>);

impl<'a> std::ops::Deref for TxnGuard<'a> {
    type Target = MutexGuard<'a, WriterState>;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("transaction writer held")
    }
}
impl std::ops::DerefMut for TxnGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().expect("transaction writer held")
    }
}

/// The single write transaction. Changes are invisible until [`commit`](Self::commit).
pub struct WriteTxn<'s> {
    _callback_owner: crate::sparql::extensions::WriterOwner,
    store: &'s Store,
    base: Arc<Snapshot>,
    delta: Delta,
    guard: TxnGuard<'s>,
    log: Vec<(u8, [Id; 4])>,
    /// large insert batches applied by rebuilding the generation on commit
    bulk: Vec<[Id; 4]>,
    kind: CommitKind,
    /// net quads added / removed relative to `base` (the commit's counts)
    net_ins: u64,
    net_del: u64,
    /// options for the write guard
    opts: crate::guard::WriteOptions,
    /// graphs already checked against [`WriteOptions::graphs`](crate::guard::WriteOptions::graphs)
    writable: rustc_hash::FxHashMap<u64, bool>,
    /// the delta vocabulary and the blank-node counter at the start, where a dry run
    /// rolls them back to
    mark: crate::vocab::VocabMark,
    start_bnode: u64,
    /// the quads asked to be inserted or deleted, when the graph view's protections
    /// limit writes: each is checked before the commit, whether or not it changed
    /// anything
    requested: Option<Vec<[Id; 4]>>,
    /// the protections applied at the state the transaction started from
    base_check: Option<crate::access::triples::WriteCheck>,
    /// commit even without a net change (a merge commit records its second parent)
    force: bool,
    /// the merge record written with the commit
    merge: Option<branching::MergeRec>,
}

impl Drop for WriteTxn<'_> {
    fn drop(&mut self) {
        // a dry run leaves no terms and no blank-node ids behind, however it ended; the
        // writer lock is still held here, and only this transaction's views reached them
        if self.opts.dry_run.is_some() {
            if let Err(e) = self.base.generation.dvocab.rollback(&self.mark) {
                tracing::warn!(target: "sparkles::store", "a dry run could not remove the terms it added: {e}");
            }
            self.guard.next_bnode = self.start_bnode;
        }
    }
}

impl WriteTxn<'_> {
    /// [`Error::NotPermitted`] unless the write's graph view lets it change graph `g`.
    /// Called before a quad is looked up, so the answer never depends on the data.
    fn check_graph(&mut self, g: Id) -> Result<()> {
        let Some(access) = self.opts.graphs.clone() else {
            return Ok(());
        };
        let ok = match self.writable.get(&g.0) {
            Some(ok) => *ok,
            None => {
                let ok = access.check_write(&self.view(), g).is_ok();
                self.writable.insert(g.0, ok);
                ok
            }
        };
        if ok {
            Ok(())
        } else {
            access.check_write(&self.view(), g)
        }
    }

    /// The graph view of this transaction's writes, if it does not cover every graph.
    pub fn graphs(&self) -> Option<&Arc<crate::access::GraphAccess>> {
        self.opts.graphs.as_ref()
    }

    /// The state the transaction's reads see: [`view`](Self::view), without the triples
    /// the graph view's protections hide. Before the first change it is the committed
    /// snapshot's masked view, built once per commit.
    pub fn read_view(&self) -> Result<Arc<Snapshot>> {
        match self.opts.graphs.as_ref().filter(|a| a.hides_triples()) {
            Some(a) if !self.is_dirty() => a.masked(&self.base),
            Some(a) => a.masked(&Arc::new(self.view())),
            None => Ok(Arc::new(self.view())),
        }
    }

    /// Check a quad asked to be deleted whose terms the store may lack (`Id::UNDEF`
    /// for a missing one), against the protections at the state the transaction
    /// started from. A quad of stored terms is checked again at the commit.
    pub fn check_requested(&mut self, q: [Id; 4], pred: &str, graph: Option<&Term>) -> Result<()> {
        if self.requested.is_none() {
            return Ok(());
        }
        self.base_check()?;
        match self.base_check.as_mut() {
            Some(c) => c.check(q, pred, graph),
            None => Ok(()),
        }
    }

    fn base_check(&mut self) -> Result<()> {
        if self.base_check.is_none()
            && let Some(rules) = self.opts.graphs.as_ref().and_then(|a| a.triples.as_ref())
        {
            self.base_check = crate::access::triples::WriteCheck::new(rules, self.base.clone())?;
        }
        Ok(())
    }

    /// Every quad asked to be inserted or deleted must be writable both at the state the
    /// transaction started from and at the state it leaves (where a protection on
    /// classes or with a pattern may match it differently). The quads asked for are
    /// checked, not the changes that took effect, so the answer does not depend on
    /// whether a hidden quad exists.
    fn check_requested_all(&mut self) -> Result<()> {
        let Some(mut req) = self.requested.take() else {
            return Ok(());
        };
        req.sort_unstable_by_key(|q| [q[0].0, q[1].0, q[2].0, q[3].0]);
        req.dedup();
        self.base_check()?;
        if let Some(c) = self.base_check.as_mut() {
            for (i, q) in req.iter().enumerate() {
                if i % 65_536 == 65_535 {
                    self.opts.check()?;
                }
                c.check_ids(*q)?;
            }
        }
        let rules = self.opts.graphs.as_ref().and_then(|a| a.triples.clone());
        if let Some(rules) = rules
            && rules.rules.iter().any(|r| {
                !r.write.is_all()
                    && (r.protection.classes.is_some() || r.protection.pattern.is_some())
            })
            && let Some(mut c) =
                crate::access::triples::WriteCheck::new(&rules, Arc::new(self.view()))?
        {
            for (i, q) in req.iter().enumerate() {
                if i % 65_536 == 65_535 {
                    self.opts.check()?;
                }
                c.check_ids(*q)?;
            }
        }
        self.requested = Some(Vec::new());
        Ok(())
    }

    /// Snapshot view including this transaction's uncommitted changes. It keeps the
    /// committed version number but not its result cache: the data differs from that
    /// version, so cached results must neither be read nor written through this view.
    pub fn view(&self) -> Snapshot {
        Snapshot {
            dataset_id: self.base.dataset_id,
            generation: self.base.generation.clone(),
            delta: self.delta.clone(),
            version: self.base.version,
            cache: self.base.cache.clone(),
            results: Arc::new(crate::sparql::cache::ResultCache::new(0, 0.0)),
            dvocab_len: self.base.generation.dvocab.len(),
            commit: self.base.commit,
            // the full-text index does not cover this transaction's changes either, and
            // a search has no plan without it: refused once there are changes
            text: match &self.base.text {
                Some(t) if self.is_dirty() => Some(Arc::new(t.with_uncommitted_changes())),
                t => t.clone(),
            },
            // the index does not cover this transaction's changes: plans without it
            geo: self.base.geo.as_ref().map(|v| Arc::new(v.for_txn())),
            union_default_graph: self.base.union_default_graph,
            geo_op_vertices: self.base.geo_op_vertices,
            delta_stats: Default::default(),
            counts: Default::default(),
            mask: None,
            historical: false,
            change_log: self.base.change_log.clone(),
        }
    }

    pub fn base(&self) -> &Arc<Snapshot> {
        &self.base
    }

    /// Get or create the id for a term (new terms go to the delta vocabulary). A blank
    /// node, also inside a triple term, must be a stored one named by its label (see
    /// [`parse_bnode_label`]). [`WriteTxn::intern_scoped`] and [`WriteTxn::new_bnode`]
    /// make new ones.
    pub fn intern(&mut self, t: &Term) -> Result<Id> {
        crate::nesting::check_triple_term(t)?;
        if let Some(id) = id::inline_id(t) {
            return Ok(id);
        }
        let mut foreign = None;
        let mut key = Vec::new();
        id::write_term_key_with(t, &mut key, &mut |b| match parse_bnode_label(b.as_str()) {
            Some(id) => id.payload(),
            None => {
                foreign.get_or_insert_with(|| b.as_str().to_string());
                0
            }
        });
        if let Some(label) = foreign {
            return Err(Error::invalid(format!(
                "the blank node _:{label} is not a stored blank node and cannot be written as one"
            )));
        }
        if let Term::BlankNode(b) = t {
            return Ok(parse_bnode_label(b.as_str()).expect("checked above"));
        }
        self.intern_key(&key)
    }

    /// Intern a term whose blank nodes (including those inside RDF 1.2 triple terms and
    /// composite literals) are scoped by `labels`: unseen labels get fresh blank node ids.
    pub fn intern_scoped(
        &mut self,
        t: &Term,
        labels: &mut std::collections::HashMap<String, Id>,
    ) -> Result<Id> {
        let relabeled = self.relabel_cdt(t, labels);
        let t = relabeled.as_ref().unwrap_or(t);
        match t {
            Term::BlankNode(b) => {
                if let Some(&id) = labels.get(b.as_str()) {
                    return Ok(id);
                }
                let id = self.new_bnode();
                labels.insert(b.as_str().to_string(), id);
                Ok(id)
            }
            Term::Triple(_) => {
                crate::nesting::check_triple_term(t)?;
                let mut next = self.guard.next_bnode;
                let mut key = Vec::new();
                id::write_term_key_with(t, &mut key, &mut |b| {
                    if let Some(id) = labels.get(b.as_str()) {
                        return id.payload();
                    }
                    let id = Id::bnode(next);
                    next += 1;
                    labels.insert(b.as_str().to_string(), id);
                    id.payload()
                });
                self.guard.next_bnode = next;
                self.intern_key(&key)
            }
            t => self.intern(t),
        }
    }

    /// `t` with the blank node labels inside its composite literals (`cdt:List`,
    /// `cdt:Map`) replaced by the labels of the stored nodes `labels` gives them, new
    /// ones for unseen labels, or `None` when it holds no such literal. The labels of a
    /// literal and of the terms around it name the same nodes, as Jena's loader has it.
    pub fn relabel_cdt(
        &mut self,
        t: &Term,
        labels: &mut std::collections::HashMap<String, Id>,
    ) -> Option<Term> {
        crate::sparql::cdt::relabel_term(t, &mut |b| {
            let id = match labels.get(b) {
                Some(&id) => id,
                None => {
                    let id = self.new_bnode();
                    labels.insert(b.to_string(), id);
                    id
                }
            };
            id::bnode_label(id.payload())
        })
    }

    pub fn intern_key(&mut self, key: &[u8]) -> Result<Id> {
        if self.guard.poisoned {
            return Err(Error::Poisoned);
        }
        if let Ok(i) = self.base.generation.vocab.find(key) {
            return Ok(Id::vocab(i));
        }
        Ok(Id::delta(self.base.generation.dvocab.insert(key)?))
    }

    /// Resolve a term against this writer's vocabulary without retaining a data view.
    /// Like a fresh transaction view, this sees vocabulary added by earlier operations.
    pub(crate) fn lookup_term(&self, term: &Term) -> Option<Id> {
        match term {
            Term::BlankNode(b) => parse_bnode_label(b.as_str()),
            _ => {
                if let Some(id) = id::inline_id(term) {
                    return Some(id);
                }
                self.lookup_key(&id::term_key(term))
            }
        }
    }

    /// The id of a term key the store or this transaction has, without adding it.
    pub fn lookup_key(&self, key: &[u8]) -> Option<Id> {
        if let Ok(i) = self.base.generation.vocab.find(key) {
            return Some(Id::vocab(i));
        }
        self.base.generation.dvocab.find(key).map(Id::delta)
    }

    pub fn new_bnode(&mut self) -> Id {
        let id = self.guard.next_bnode;
        self.guard.next_bnode += 1;
        Id::bnode(id)
    }

    /// Whether `id` is a stored blank node this store has handed out: one that
    /// [`new_bnode`](Self::new_bnode) or a load numbered before now. Any other blank node
    /// id could still be given to a new node.
    pub fn bnode_allocated(&self, id: Id) -> bool {
        let p = id.payload();
        if id.tag() != crate::id::Tag::BNode || p & Id::LOCAL_BNODE_BIT != 0 {
            return false;
        }
        let next = self.guard.next_bnode;
        let (ord, own) = (
            crate::branch::bnode_ordinal(p),
            crate::branch::bnode_ordinal(next),
        );
        // a node of another branch's range came with a merge or a starting commit
        if ord != own {
            return ord < self.store.branching.next_ordinal.load(Ordering::Relaxed);
        }
        p < next
    }

    /// Encode a parsed quad; blank node labels are scoped by `labels` (fresh ids).
    pub fn encode_quad(
        &mut self,
        q: &Quad,
        labels: &mut std::collections::HashMap<String, Id>,
    ) -> Result<[Id; 4]> {
        let s = match &q.subject {
            NamedOrBlankNode::NamedNode(n) => self.intern_key(&id::iri_key(n.as_str()))?,
            NamedOrBlankNode::BlankNode(b) => {
                self.intern_scoped(&Term::BlankNode(b.clone()), labels)?
            }
        };
        let p = self.intern_key(&id::iri_key(q.predicate.as_str()))?;
        let o = match &q.object {
            t @ (Term::BlankNode(_) | Term::Triple(_)) => self.intern_scoped(t, labels)?,
            Term::Literal(l) if crate::sparql::cdt::may_name_bnodes(l) => {
                self.intern_scoped(&q.object, labels)?
            }
            t => self.intern(t)?,
        };
        let g = match &q.graph_name {
            GraphName::DefaultGraph => Id::DEFAULT_GRAPH,
            GraphName::NamedNode(n) => self.intern_key(&id::iri_key(n.as_str()))?,
            GraphName::BlankNode(b) => self.intern_scoped(&Term::BlankNode(b.clone()), labels)?,
        };
        Ok([s, p, o, g])
    }

    fn in_base(&self, q: &[Id; 4]) -> Result<bool> {
        #[cfg(test)]
        tests::WRITE_BASE_PROBES.with(|n| n.set(n.get() + 1));
        self.base
            .perm(Perm::Spo)
            .contains(&self.base.cache, &Perm::Spo.to_key(q))
    }

    pub fn contains(&self, q: &[Id; 4]) -> Result<bool> {
        let k = Perm::Spo.to_key(q);
        let i = Perm::Spo.index();
        if self.delta.ins[i].contains(&k) {
            return Ok(true);
        }
        if self.delta.del[i].contains(&k) {
            return Ok(false);
        }
        self.in_base(q)
    }

    /// Physical-base membership for an effective change, or `None` for a no-op.
    /// Every delta is relative to the physical index: inserts are absent from it,
    /// and deletions are present in it, including replayed and linked overlays.
    fn change_base(&self, q: &[Id; 4], insert: bool) -> Result<Option<bool>> {
        let k = Perm::Spo.to_key(q);
        let i = Perm::Spo.index();
        if self.delta.ins[i].contains(&k) {
            return Ok((!insert).then_some(false));
        }
        if self.delta.del[i].contains(&k) {
            return Ok(insert.then_some(true));
        }
        let in_base = self.in_base(q)?;
        Ok((in_base != insert).then_some(in_base))
    }

    /// Whether the quad was present in the committed snapshot this transaction started
    /// from (`in_base`: present in the base index).
    fn present_at_start(&self, q: &[Id; 4], in_base: bool) -> bool {
        let k = Perm::Spo.to_key(q);
        let i = Perm::Spo.index();
        self.base.delta.ins[i].contains(&k) || (in_base && !self.base.delta.del[i].contains(&k))
    }

    /// Insert a quad; returns true if it was not present.
    pub fn insert(&mut self, q: [Id; 4]) -> Result<bool> {
        if q.iter().any(|id| {
            matches!(id.tag(), Tag::Local | Tag::Undef)
                || id.tag() == Tag::BNode && id.payload() & Id::LOCAL_BNODE_BIT != 0
        }) {
            return Err(Error::invalid("cannot store query-local or unbound terms"));
        }
        self.check_graph(q[3])?;
        if let Some(r) = self.requested.as_mut() {
            r.push(q);
        }
        let Some(ib) = self.change_base(&q, true)? else {
            return Ok(false);
        };
        // re-adding a quad this transaction deleted cancels that deletion
        if self.present_at_start(&q, ib) {
            self.net_del -= 1;
        } else {
            self.net_ins += 1;
        }
        apply(&mut self.delta, &q, true, ib);
        self.log.push((WAL_INSERT, q));
        Ok(true)
    }

    /// Delete a quad; returns true if it was present.
    pub fn delete(&mut self, q: [Id; 4]) -> Result<bool> {
        self.check_graph(q[3])?;
        if let Some(r) = self.requested.as_mut() {
            r.push(q);
        }
        let Some(ib) = self.change_base(&q, false)? else {
            return Ok(false);
        };
        // deleting a quad this transaction added cancels that insertion
        if self.present_at_start(&q, ib) {
            self.net_del += 1;
        } else {
            self.net_ins -= 1;
        }
        apply(&mut self.delta, &q, false, ib);
        self.log.push((WAL_DELETE, q));
        Ok(true)
    }

    pub fn is_dirty(&self) -> bool {
        !self.log.is_empty() || !self.bulk.is_empty()
    }

    /// Insert many quads. Batches at or above the store's `bulk_threshold` are not
    /// applied to the delta; on commit they are merged into a freshly built generation
    /// (QLever-style rebuild), which is much faster than per-quad delta inserts. Staged
    /// bulk quads are not visible through [`view`](Self::view) / [`contains`](Self::contains)
    /// before commit.
    pub fn insert_bulk(&mut self, quads: Vec<[Id; 4]>) -> Result<()> {
        if self.opts.graphs.is_some() {
            for q in &quads {
                self.check_graph(q[3])?;
            }
        }
        // a write whose quads protections check goes through the delta, so that the
        // state it leaves can be checked before the commit
        if (quads.len() as u64) < self.store.opts.bulk_threshold || self.requested.is_some() {
            for q in quads {
                self.insert(q)?;
            }
            return Ok(());
        }
        if quads
            .iter()
            .flatten()
            .any(|id| matches!(id.tag(), Tag::Local | Tag::Undef))
        {
            return Err(Error::invalid("cannot store query-local or unbound terms"));
        }
        self.bulk.extend(quads);
        Ok(())
    }

    /// Durably commit and publish a new snapshot. A transaction without net effect (and
    /// without a bulk batch) creates no commit: its receipt carries the unchanged head.
    pub fn commit(self) -> Result<Receipt> {
        let span = tracing::info_span!(
            target: "sparkles::commit",
            "commit",
            kind = self.kind.name(),
            seq = tracing::field::Empty,
            inserted = tracing::field::Empty,
            deleted = tracing::field::Empty,
        );
        let _entered = span.enter();
        let r = self.commit_inner();
        if let Ok(r) = &r
            && r.committed
        {
            // as i64: exporters render u64 fields as strings
            span.record("seq", r.commit.seq as i64);
            span.record("inserted", r.commit.inserted as i64);
            span.record("deleted", r.commit.deleted as i64);
        }
        r
    }

    fn commit_inner(mut self) -> Result<Receipt> {
        self._callback_owner.check()?;
        if self.guard.poisoned || self.store.group.as_ref().is_some_and(|g| g.failed()) {
            return Err(Error::Poisoned);
        }
        let grouped = self.bulk.is_empty()
            && !self.force
            && self.merge.is_none()
            && self.wal_bytes() <= group::MAX_BYTES
            && self
                .store
                .group_admission(self.kind, &self.opts, &self.guard);
        // A dependent no-op, preview, validation, bulk rebuild or other fallback
        // cannot acknowledge or capture a speculative predecessor.
        if !grouped || (self.net_ins == 0 && self.net_del == 0) {
            self.store.drain_group(&mut self.guard, Some(&self.opts))?;
        }
        // a commit with a merge record (a merge, or a commit a merge replays) passes
        if (self.is_dirty() || self.force) && self.merge.is_none() {
            self.store.check_protected(self.kind)?;
        }
        self.check_requested_all()?;
        // a write cancelled (its client gone) or past its deadline publishes nothing
        self.opts.check()?;
        if self.bulk.is_empty() {
            if let Some(dr) = self.opts.dry_run.clone() {
                return Err(Error::DryRun(Box::new(self.preview_log(&dr)?)));
            }
            if self.net_ins == 0 && self.net_del == 0 && !self.force {
                // no net change: no commit, nothing to validate
                return self.publish_log(None);
            }
            let head = self.guard.head.seq;
            let validation = self.store.run_guard(
                &self.base,
                || Arc::new(self.view()),
                self.kind,
                crate::guard::Changes::Log(&self.log),
                &self.opts,
                head,
            )?;
            if grouped && validation.is_none() {
                return self.publish_grouped();
            }
            if grouped {
                self.store.drain_group(&mut self.guard, Some(&self.opts))?;
            }
            return self.publish_log(validation);
        }
        // Build the new generation from this transaction's view (base + uncommitted delta)
        // plus the bulk quads; switching CURRENT is the atomic commit point, so the
        // transaction's small changes need no WAL records.
        let bulk = std::mem::take(&mut self.bulk);
        let view = self.view();
        self.base.generation.dvocab.sync()?;
        let commit = BulkCommit {
            kind: self.kind,
            net_del: self.net_del,
            start_len: self.base.len(),
            default_graph: self.log.iter().any(|(_, q)| q[3] == Id::DEFAULT_GRAPH)
                || bulk.iter().any(|q| q[3] == Id::DEFAULT_GRAPH),
        };
        let log = std::mem::take(&mut self.log);
        let check = Some((
            crate::guard::Changes::Rebuilt {
                log: &log,
                bulk: &bulk,
            },
            &self.opts,
        ));
        let (_, receipt) = self.store.rebuild_locked(
            &mut self.guard,
            &view,
            &[],
            &bulk,
            &[],
            Some(commit),
            check,
        )?;
        Ok(receipt)
    }

    fn publish_log(
        &mut self,
        validation: Option<Arc<crate::guard::ValidationSummary>>,
    ) -> Result<Receipt> {
        let gen_ = &self.base.generation;
        let head = self.guard.head;
        if self.net_ins == 0 && self.net_del == 0 && !self.force {
            // nothing changed (or every change was undone): no commit, nothing published
            return Ok(Receipt {
                dataset_id: self.store.owner_dataset_id(),
                committed: false,
                commit: head,
                validation: None,
                annotation: self.store.annotation(head.seq).unwrap_or_default(),
            });
        }
        let message = match &self.opts.message {
            Some(m) => crate::annotations::validate_message(m)?,
            None => None,
        };
        // nothing is written when the disk (or an in-memory store's limit, or a
        // persistent one's quota) has no room
        self.check_storage()?;
        // A commit that added terms syncs them together with its WAL records (see
        // `sync_commit`); without a WAL they are synced here. They are written to the
        // file first, so that a reader of the WAL (`sparkles check`, a backup) finds
        // every term its commit records name.
        if self.guard.wal.is_none() {
            gen_.dvocab.sync()?;
        } else if gen_.dvocab.needs_sync() {
            gen_.dvocab.flush()?;
        }
        let c = self.next_commit(validation.as_deref());
        // the message and digest are durable before the commit is (see `annotations`)
        let annotation = self
            .store
            .annotate(&c, message, || Some(self.change_lines()))?;
        let next_bnode = self.guard.next_bnode;
        let prealloc = self.wal_prealloc();
        // a merge's record is durable before its commit (open drops one above the head)
        if let Some(m) = self.merge.as_mut() {
            m.seq = c.seq;
            let rec = *m;
            if let Err(e) = self.store.branching.merges.lock().append(rec) {
                let _ = self.store.annotations.lock().undo(c.seq);
                return Err(e);
            }
            self.store.failpoint("merge-recorded");
        }
        let w = &mut **self.guard;
        if let Some(wal) = w.wal.as_mut() {
            let mut data = Vec::with_capacity((self.log.len() + 1) * WAL_REC);
            let mut rec = [0u8; WAL_REC];
            for (op, q) in &self.log {
                rec[0] = *op;
                for j in 0..4 {
                    rec[1 + j * 8..9 + j * 8].copy_from_slice(&q[j].0.to_le_bytes());
                }
                data.extend_from_slice(&rec);
            }
            rec[0] = WAL_COMMIT;
            rec[1..9].copy_from_slice(&next_bnode.to_le_bytes());
            let flags = if c.unvalidated {
                commit::WAL_FLAG_UNVALIDATED
            } else {
                0
            };
            commit::seal_wal_commit(&mut rec, c.seq, c.timestamp_ms, c.kind, flags, &data);
            data.extend_from_slice(&rec);
            // once the first byte is written, a failure leaves the WAL in an unknown
            // state: refuse further writes, so a seq can never be written twice
            let alloc_before = w.wal_alloc;
            let written = wal
                .flush()
                .and_then(|_| {
                    wal::write_commit(wal.get_ref(), w.wal_len, &mut w.wal_alloc, &data, prealloc)
                })
                .map_err(Error::from)
                .and_then(|_| sync_commit_reused(wal.get_ref(), &gen_.dvocab, &mut w.vocab_sync));
            if let Err(e) = written {
                w.poisoned = true;
                let _ = self.store.annotations.lock().undo(c.seq);
                let _ = self.store.branching.merges.lock().undo(c.seq);
                return Err(e);
            }
            // the bytes the file grew by: the records past its end and any zeros after,
            // which the quota does not count
            self.store.quota.add(w.wal_alloc - alloc_before);
            w.wal_len += data.len() as u64;
            self.store.quota.set_preallocated(w.wal_alloc - w.wal_len);
            self.store.wal_end.store(w.wal_len, Ordering::Relaxed);
            if let Some(ix) = gen_.wal_index.lock().as_mut() {
                ix.note(wal::WalPoint {
                    seq: c.seq,
                    offset: w.wal_len,
                    folding: false,
                });
            }
        }
        if let Some(tap) = self.guard.tap.as_mut() {
            if tap.active.load(Ordering::Acquire) {
                tap.commits.push(compaction::TapCommit {
                    info: c,
                    next_bnode,
                    changes: self.log.clone(),
                });
            } else {
                self.guard.tap = None;
            }
        }
        if let Some(log) = self.store.changelog.as_ref().filter(|l| l.is_enabled()) {
            // the background writer turns the ids into keys
            log.push(changelog::Pending {
                commit: changelog::ChangeCommit::of(
                    &c,
                    self.opts.author.clone(),
                    annotation.message.clone(),
                ),
                body: changelog::PendingBody::Log {
                    generation: gen_.clone(),
                    changes: self.log.clone(),
                },
            });
        }
        self.guard.head = c;
        self.store.catalog.lock().append(c);
        self.store.forget_annotations();
        self.store.compaction.committed(c.timestamp_ms);
        if let Some(m) = self.merge
            && let Some(set) = self.store.branching.set()
        {
            set.note_merges(self.store.dataset_id, &[m], false);
        }
        let version = self.base.version + 1;
        let mut snap = Snapshot {
            dataset_id: self.base.dataset_id,
            generation: gen_.clone(),
            delta: std::mem::take(&mut self.delta),
            version,
            cache: self.base.cache.clone(),
            results: self.base.results.clone(),
            dvocab_len: gen_.dvocab.len(),
            commit: c.seq,
            text: self.base.text.clone(),
            geo: self.base.geo.clone(),
            union_default_graph: self.base.union_default_graph,
            geo_op_vertices: self.base.geo_op_vertices,
            delta_stats: Default::default(),
            counts: Default::default(),
            mask: None,
            historical: false,
            change_log: self.base.change_log.clone(),
        };
        self.store.maintain_text(&mut snap, &self.log);
        self.store.maintain_geo(&mut snap, &self.log);
        self.store.remember_past(c.seq);
        let snap = Arc::new(snap);
        self.store.current.store(snap.clone());
        self.store.commits.send_replace(c.seq);
        // after the publication: a worker that takes these pairs reads this state
        self.store.embed_noted(&snap, &self.log, c.seq, self.kind);
        if validation.is_some() {
            self.store.guard_committed(c.seq);
        }
        Ok(Receipt {
            dataset_id: self.store.owner_dataset_id(),
            committed: true,
            commit: c,
            validation,
            annotation,
        })
    }

    /// The bytes the commit appends to the WAL.
    fn wal_bytes(&self) -> u64 {
        (self.log.len() as u64 + 1) * WAL_REC as u64
    }

    /// The most this commit may preallocate in the WAL past its records (see
    /// [`StoreOptions::wal_prealloc_bytes`]): none when that would take the disk below
    /// the free space the store keeps. The preallocation only saves time, so it never
    /// makes a commit fail. The quota does not count it.
    fn wal_prealloc(&self) -> u64 {
        let p = self.store.opts.wal_prealloc_bytes;
        let wal_bytes = self.wal_bytes();
        if p == 0 || self.guard.wal_len + wal_bytes <= self.guard.wal_alloc {
            return p;
        }
        if self.store.check_disk(wal_bytes + p + 4096).is_err() {
            return 0;
        }
        p
    }

    /// The in-memory size of the store after this transaction (in-memory stores).
    fn memory_size(&self) -> u64 {
        let gen_ = &self.base.generation;
        gen_.disk_bytes() + delta_bytes(&self.delta) + gen_.dvocab.with(|v| v.bytes()) as u64
    }

    /// The storage checks of a commit through the WAL: the free disk space the store
    /// keeps, and for a commit that adds quads the quota or the in-memory size limit.
    fn check_storage(&self) -> Result<()> {
        let wal_bytes = self.wal_bytes();
        self.store.check_disk(wal_bytes)?;
        if self.net_ins > 0 {
            self.store.quota.check_commit(wal_bytes)?;
        }
        if self.net_ins > 0
            && self.store.root.is_none()
            && self.store.opts.max_memory_bytes.is_some()
        {
            self.store.check_memory(self.memory_size())?;
        }
        Ok(())
    }

    /// The commit this transaction makes through the WAL, after the head.
    fn next_commit(&self, validation: Option<&crate::guard::ValidationSummary>) -> CommitInfo {
        let head = self.guard.head;
        CommitInfo {
            seq: head.seq + 1,
            timestamp_ms: self.store.commit_time(&head),
            kind: self.kind,
            inserted: self.net_ins,
            deleted: self.net_del,
            quads: (head.quads + self.net_ins).saturating_sub(self.net_del),
            generation: commit::generation_number(&self.base.generation.name),
            bulk: false,
            exact: true,
            reconstructed: false,
            default_graph: self.log.iter().any(|(_, q)| q[3] == Id::DEFAULT_GRAPH),
            unvalidated: validation
                .is_some_and(|v| v.status == crate::guard::GuardStatus::Bypassed),
        }
    }

    /// Preview this transaction instead of committing it (see [`crate::preview`]):
    /// what the commit would be, with the changes `dr` asks for. Nothing is written,
    /// and the terms it added are removed again.
    pub fn preview(mut self, dr: crate::preview::DryRun) -> Result<crate::preview::Preview> {
        self.opts.dry_run = Some(dr);
        crate::preview::catch(self.commit())
    }

    /// The net changes of this transaction as canonical N-Quads lines: (deleted,
    /// inserted). The log holds effective changes only, so a quad was present at the
    /// start iff its first change is a delete, and is present at the end iff its last
    /// change is an insert.
    fn change_lines(&self) -> (Vec<String>, Vec<String>) {
        let mut ends: rustc_hash::FxHashMap<[Id; 4], (u8, u8)> = Default::default();
        for (op, q) in &self.log {
            ends.entry(*q).or_insert((*op, *op)).1 = *op;
        }
        let view = self.view();
        let (mut deleted, mut inserted) = (Vec::new(), Vec::new());
        for (q, (first, last)) in ends {
            let (was, is) = (first == WAL_DELETE, last == WAL_INSERT);
            if was == is {
                continue;
            }
            let Some(t) = view.quad_to_terms(&q) else {
                continue;
            };
            let line = crate::annotations::nquads_line(&t);
            if was { &mut deleted } else { &mut inserted }.push(line);
        }
        (deleted, inserted)
    }
}

/// The graphs [`Store::replace`] clears before loading.
#[derive(Clone, Debug)]
pub enum ReplaceTarget {
    Default,
    Named(NamedNode),
    /// every graph, default included
    All,
}

/// Convenience helper for tests and the CLI.
pub fn named(iri: &str) -> NamedNode {
    NamedNode::new_unchecked(iri)
}

/// Whether `<root>/validation.json` asks for write-time validation (any mode but
/// `off`). Only the mode is read: the store does not depend on the validator. A file that
/// does not parse also requires a guard (fail closed).
fn guard_required_by(root: &Path) -> bool {
    match std::fs::read(root.join(crate::guard::config::CONFIG_FILE)) {
        Ok(b) => serde_json::from_slice::<serde_json::Value>(&b)
            .map(|j| j.get("mode").and_then(|m| m.as_str()) != Some("off"))
            .unwrap_or(true),
        Err(_) => false,
    }
}

/// A rough quad count of RDF sources from their size (~80 bytes per quad in text
/// formats; compressed sizes scaled by [`Codec::expansion`](crate::codec::Codec::expansion)).
fn estimated_quads(sources: &[Source]) -> u64 {
    let mut bytes = 0u64;
    for s in sources {
        bytes += match &s.data {
            crate::io::SourceData::Bytes(b) => b.len() as u64,
            crate::io::SourceData::File(p) => std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
        } * s.codec().map_or(1, |c| c.expansion());
    }
    bytes / 80
}

/// A Turtle prefix name (`PN_PREFIX`, ASCII subset), or the empty prefix.
fn valid_prefix_name(p: &str) -> bool {
    let b = p.as_bytes();
    p.is_empty()
        || (b[0].is_ascii_alphabetic()
            && b.iter()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
            && !p.ends_with('.'))
}

/// Write the quads of `snap` as N-Quads.
fn dump_snapshot(snap: &Snapshot, w: impl Write) -> Result<u64> {
    let mut ser = oxttl::NQuadsSerializer::new().for_writer(w);
    let mut n = 0;
    snap.for_each_quad(|q| {
        if let Some(quad) = snap.quad_to_terms(q) {
            ser.serialize_quad(&quad)?;
            n += 1;
        }
        Ok(())
    })?;
    ser.finish().flush()?;
    Ok(n)
}

/// Estimated memory of one quad of a delta (seven ordered sets of 32-byte keys).
const DELTA_QUAD_BYTES: u64 = 7 * 64;

/// Estimated memory of a (replayed) delta.
fn delta_bytes(d: &Delta) -> u64 {
    (d.inserts() + d.deletes()) as u64 * DELTA_QUAD_BYTES
}

/// Retire the generations history no longer needs (`current` is the current generation
/// and `head` the latest commit); call with the writer lock held. They are renamed to
/// `*.deleting`, and the caller deletes the returned directories.
fn collect_generations(
    root: &Path,
    now_ms: i64,
    cat: &Catalog,
    h: &mut crate::history::HistoryState,
    current: u32,
    head: u64,
    max_gens: usize,
) -> Vec<PathBuf> {
    let needed = {
        let ts = |s: u64| cat.get(s).map(|c| c.timestamp_ms);
        h.needed(current, head, now_ms, &ts, max_gens)
    };
    let doomed: Vec<(u32, PathBuf)> = h
        .gens
        .iter()
        .filter(|(no, _)| **no != current && !needed.contains_key(no))
        .map(|(no, g)| (*no, g.dir.clone()))
        .collect();
    let mut retired = Vec::new();
    for (no, dir) in doomed {
        h.open.retain(|(n, _)| *n != no);
        h.cache.retain(|c| c.generation != no);
        match crate::history::retire_generation(root, &dir) {
            Ok(d) => {
                h.gens.remove(&no);
                retired.extend(d);
            }
            Err(e) => {
                tracing::warn!(target: "sparkles::store", "could not remove {}: {e}", dir.display())
            }
        }
    }
    retired
}

/// A persistent store's history collection, held weakly (by backup lease guards): it
/// does nothing once the store is closed.
pub(crate) struct Collector {
    root: PathBuf,
    max_gens: usize,
    current: std::sync::Weak<ArcSwap<Snapshot>>,
    writer: std::sync::Weak<Mutex<WriterState>>,
    catalog: std::sync::Weak<Mutex<Catalog>>,
    clock: std::sync::Weak<Mutex<Option<Clock>>>,
    history: std::sync::Weak<Mutex<crate::history::HistoryState>>,
}

impl Collector {
    /// Remove lease `id`, then collect if the writer is free right now.
    pub(crate) fn release(&self, lease: u64) {
        let Some(history) = self.history.upgrade() else {
            return;
        };
        history.lock().leases.remove(&lease);
        self.try_collect();
    }

    /// Collect unneeded generations if the store is open and its writer free (the next
    /// collection point does it otherwise).
    pub(crate) fn try_collect(&self) {
        let (Some(writer), Some(current), Some(catalog), Some(clock), Some(history)) = (
            self.writer.upgrade(),
            self.current.upgrade(),
            self.catalog.upgrade(),
            self.clock.upgrade(),
            self.history.upgrade(),
        ) else {
            return;
        };
        // the store's drop marks the writer closed under this lock, so the directory is
        // still this store's (and locked) while the collection runs
        let Some(mut w) = writer.try_lock() else {
            return;
        };
        // A completed idle group needs no later write to resume collection.
        if w.staged.is_some() && current.load().commit == w.head.seq {
            w.staged = None;
        }
        if w.closed || w.staged.is_some() {
            return;
        }
        let now = match &*clock.lock() {
            Some(c) => c(),
            None => commit::now_ms(),
        };
        let cur = commit::generation_number(&current.load().generation.name);
        // lock order: writer, history, catalog
        let mut h = history.lock();
        let retired = collect_generations(
            &self.root,
            now,
            &catalog.lock(),
            &mut h,
            cur,
            w.head.seq,
            self.max_gens,
        );
        for dir in retired {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        #[cfg(feature = "text")]
        self.stop_text_recovery();
        // Checkpoint/join derived writers while the dataset OS lock is still held.
        self.text.store(None);
        // a lease guard outliving the store must not collect in a directory that another
        // process (or a restore's swap) may own next
        {
            let mut w = self.guarded_writer();
            w.closed = true;
            // Backup leases may retain WriterState after the store closes.
            // Shut down the owned worker now, rather than with that last lease.
            w.vocab_sync = Default::default();
            w.trim_wal();
        }
        if let Some(log) = &self.changelog
            && let Err(e) = log.flush(true)
        {
            tracing::warn!(target: "sparkles::store", error = %e, "could not write the change log on close");
        }
        self.embed.close();
    }
}

/// The history state of a store being opened: pins and retention from
/// `history.json`, and the table of this dataset's generation directories. Finishes
/// interrupted collections and removes interrupted rebuilds (generations newer than
/// the current one).
fn open_history(
    root: &Path,
    dataset_id: uuid::Uuid,
    current: u32,
    head: u64,
    catalog: &Catalog,
) -> Result<crate::history::HistoryState> {
    use crate::history;
    history::remove_deleting(root)?;
    compaction::remove_interrupted(root, dataset_id, current);
    let cfg = history::read_file(root, dataset_id)?;
    let mut h = history::HistoryState::new(cfg.pins, cfg.retention);
    h.schedules = cfg.schedules;
    h.catalog = cfg.catalog;
    for (no, name, base, fold_legacy) in history::scan_generations(root, dataset_id)? {
        let dir = root.join(&name);
        if no > current {
            if let Err(e) = history::delete_generation(root, &dir) {
                tracing::warn!(target: "sparkles::store", "could not remove the interrupted rebuild {name}: {e}");
            }
            continue;
        }
        let end = if no == current {
            head
        } else {
            catalog
                .last_in_generation(no)
                .map_or_else(|| wal_end(&dir, &base, fold_legacy), |e| e.max(base.seq))
        };
        let bytes = if no == current { 0 } else { dir_size(&dir) };
        h.gens.insert(
            no,
            history::GenEntry {
                name,
                dir,
                base,
                end,
                fold_legacy,
                bytes,
            },
        );
    }
    Ok(h)
}

/// Where commit `seq` ends in generation `g`'s log (`entry` describes it). The sparse
/// index gives a nearby position, built by reading the log once if the generation has
/// none yet, and the log is read forward from there to the commit.
pub(crate) fn wal_point(
    g: &Generation,
    entry: &crate::history::GenEntry,
    seq: u64,
) -> Result<wal::WalPoint> {
    let path = entry.dir.join("wal.log");
    let floor = {
        let mut ix = g.wal_index.lock();
        if ix.is_none() {
            *ix = Some(wal::WalIndex::scan(
                &path,
                entry.base.seq,
                entry.fold_legacy,
            )?);
        }
        ix.as_ref().map(|ix| ix.floor(seq)).expect("built above")
    };
    if floor.seq == seq && !floor.folding {
        return Ok(floor);
    }
    wal::WalCursor::open(&path, floor)?.seek_to(seq)
}

/// Make a commit durable: its WAL records, already written to `wal`, and the delta terms
/// it added, if any. The two files are synced at the same time, on two threads, so a
/// commit that adds terms waits for one `fdatasync` rather than two in a row (on ext4
/// both wait for the same journal commit). A crash can then leave the commit record
/// durable without the terms it names: replay treats a last commit naming delta ids
/// that `delta.vocab` lacks as a torn tail, like one whose checksum fails. The commit is
/// acknowledged only after both syncs succeed, so no acknowledged commit is dropped.
fn sync_commit_reused(
    wal: &File,
    dvocab: &DeltaVocab,
    worker: &mut crate::vocab::LazySync,
) -> Result<()> {
    if !dvocab.needs_sync() {
        return Ok(wal.sync_data()?);
    }
    let pending = dvocab.sync_on(worker);
    let synced = wal.sync_data();
    let vocab = match pending {
        Some(pending) => pending.wait(),
        None => dvocab.sync(),
    };
    // Always await both, including when WAL sync fails, before releasing ownership.
    synced?;
    vocab
}

fn sync_commit(wal: &File, dvocab: &DeltaVocab) -> Result<()> {
    if !dvocab.needs_sync() {
        return Ok(wal.sync_data()?);
    }
    // the caller wrote both files' new bytes before either sync starts, so that both
    // are part of the journal commit the first sync starts
    std::thread::scope(|s| {
        let vocab = std::thread::Builder::new()
            .name("vocab-sync".into())
            .spawn_scoped(s, || dvocab.sync());
        let synced = wal.sync_data();
        let vocab = match vocab {
            Ok(h) => h.join().unwrap_or_else(|_| {
                Err(std::io::Error::other("the delta vocabulary sync panicked").into())
            }),
            // no thread to spare: one after the other
            Err(_) => dvocab.sync(),
        };
        synced?;
        vocab
    })
}

/// The highest delta-vocabulary id among `quads`, plus one (0 when they name none).
fn delta_ids_end<'a>(quads: impl IntoIterator<Item = &'a [Id; 4]>) -> u64 {
    quads
        .into_iter()
        .flatten()
        .filter(|id| id.tag() == crate::id::Tag::Delta)
        .map(|id| id.payload() + 1)
        .max()
        .unwrap_or(0)
}

/// The last commit in a generation's WAL, from its commit records alone.
pub(crate) fn wal_end(dir: &Path, base: &CommitInfo, fold_legacy: bool) -> u64 {
    let Ok(buf) = std::fs::read(dir.join("wal.log")) else {
        return base.seq;
    };
    let recs = buf.as_chunks::<WAL_REC>().0;
    let (mut end, mut txn_start, mut seen_v2) = (base.seq, 0usize, false);
    for (i, rec) in recs.iter().enumerate() {
        match rec[0] {
            WAL_INSERT | WAL_DELETE => {}
            WAL_COMMIT => {
                match commit::open_wal_commit(rec, &buf[txn_start * WAL_REC..i * WAL_REC]) {
                    Some(Ok((seq, _, _, _))) => {
                        seen_v2 = true;
                        end = seq;
                    }
                    Some(Err(())) => break,
                    None if fold_legacy && !seen_v2 => {}
                    None => end += 1,
                }
                txn_start = i + 1;
            }
            _ => break,
        }
    }
    end
}

/// The state a WAL replay starts from.
pub(crate) struct ReplayFrom<'a> {
    pub generation: &'a Arc<Generation>,
    pub cache: &'a Arc<BlockCache>,
    pub results: &'a Arc<crate::sparql::cache::ResultCache>,
    /// the commit the generation's base index holds
    pub base: CommitInfo,
    pub gen_no: u32,
    /// legacy commit records before the first one with metadata fold into the base
    pub fold_legacy: bool,
    pub next_bnode: u64,
    /// collect the quads each commit changed
    pub keep_touched: bool,
    /// for error messages
    pub path: &'a Path,
    /// the delta the generation's base holds (empty except for a linked generation)
    pub start: Delta,
}

/// What a WAL replay yields.
pub(crate) struct Replay {
    pub delta: Delta,
    /// complete transactions replayed
    pub version: u64,
    /// the commits replayed (without those folded into the base)
    pub commits: Vec<CommitInfo>,
    /// quads of the base plus transactions folded into it
    pub base_quads: u64,
    pub next_bnode: u64,
    /// bytes of complete transactions (anything after is a torn tail)
    pub good: usize,
    pub touched: Vec<(u64, Vec<[Id; 4]>)>,
    /// where the replayed commits end in the log
    pub index: wal::WalIndex,
}

/// Replay the records of a generation's WAL (`buf`) onto its base index, when the store
/// opens. A checksum mismatch in the final transaction is a torn tail and ends the
/// replay; one before it is [`Error::Corrupt`]. `check` runs every 64 Ki records with
/// the delta so far. The same inputs always give the same commit numbers, and
/// [`wal::WalCursor`] numbers the commits of past-state reads and diffs the same way.
pub(crate) fn replay_wal(
    from: &ReplayFrom<'_>,
    buf: &[u8],
    check: &mut dyn FnMut(u64, &Delta) -> Result<()>,
) -> Result<Replay> {
    let cache = from.cache;
    let recs = buf.as_chunks::<WAL_REC>().0;
    // the last complete transaction may be torn; damage before it is corruption
    let last_commit = recs.iter().rposition(|r| r[0] == WAL_COMMIT);
    // where the last transaction starts: a commit that overwrites preallocated space may
    // reach the disk in any order of its pages, so a crash can leave zeros in the middle
    // of it, before its commit record. It was never acknowledged, and is torn.
    let last_txn = last_commit.map(|l| {
        recs[..l]
            .iter()
            .rposition(|r| r[0] == WAL_COMMIT)
            .map_or(0, |p| p + 1)
    });
    let dvocab_len = from.generation.dvocab.len();
    let mut out = Replay {
        delta: from.start.clone(),
        version: 0,
        commits: Vec::new(),
        base_quads: (from.generation.meta.quads + from.start.inserts() as u64)
            .saturating_sub(from.start.deletes() as u64),
        next_bnode: from.next_bnode,
        good: 0,
        touched: Vec::new(),
        index: wal::WalIndex::new(from.base.seq, from.fold_legacy),
    };
    let mut pending: Vec<(u8, [Id; 4])> = Vec::new();
    let mut txn_start = 0usize;
    // whether each quad a transaction changes was present when it began (for the
    // commit's net counts), from its first change in the transaction
    let mut start: rustc_hash::FxHashMap<Key, bool> = Default::default();
    let probe = Snapshot {
        dataset_id: uuid::Uuid::nil(),
        generation: from.generation.clone(),
        delta: Delta::default(),
        version: 0,
        cache: cache.clone(),
        results: from.results.clone(),
        dvocab_len: u64::MAX,
        commit: 0,
        text: None,
        geo: None,
        union_default_graph: false,
        geo_op_vertices: StoreOptions::default().geo_op_vertices,
        delta_stats: Default::default(),
        counts: Default::default(),
        mask: None,
        historical: false,
        change_log: None,
    };
    let mut quads = out.base_quads;
    let mut seen_v2 = false;
    for (i, rec) in recs.iter().enumerate() {
        if i % 65_536 == 65_535 {
            check(i as u64 + 1, &out.delta)?;
        }
        let q: [Id; 4] = std::array::from_fn(|j| {
            Id(u64::from_le_bytes(
                rec[1 + j * 8..9 + j * 8].try_into().unwrap(),
            ))
        });
        match rec[0] {
            WAL_INSERT | WAL_DELETE => pending.push((rec[0], q)),
            WAL_COMMIT => {
                let meta = commit::open_wal_commit(rec, &buf[txn_start * WAL_REC..i * WAL_REC]);
                if matches!(meta, Some(Err(()))) {
                    if Some(i) == last_commit {
                        break; // torn tail
                    }
                    return Err(Error::Corrupt(format!(
                        "{}: checksum mismatch in the transaction ending at byte {}",
                        from.path.display(),
                        (i + 1) * WAL_REC
                    )));
                }
                // A commit's new terms and its WAL records are synced at the same time
                // (see `sync_commit`): after a crash, the last commit may name terms
                // that did not reach `delta.vocab`. It was never acknowledged.
                let needs = delta_ids_end(pending.iter().map(|(_, q)| q));
                if needs > dvocab_len {
                    if Some(i) == last_commit {
                        break; // torn tail
                    }
                    return Err(Error::Corrupt(format!(
                        "{}: the transaction ending at byte {} names delta term {}, beyond the {dvocab_len} terms of delta.vocab",
                        from.path.display(),
                        (i + 1) * WAL_REC,
                        needs - 1
                    )));
                }
                let (mut ins, mut del) = (0i64, 0i64);
                let touched: Vec<[Id; 4]> = if from.keep_touched {
                    pending.iter().map(|(_, q)| *q).collect()
                } else {
                    Vec::new()
                };
                let before = out.commits.len();
                let default_graph = pending.iter().any(|(_, q)| q[3] == Id::DEFAULT_GRAPH);
                for (op, q) in pending.drain(..) {
                    let k = Perm::Spo.to_key(&q);
                    let in_base = probe.perm(Perm::Spo).contains(cache, &k)?;
                    let spo = Perm::Spo.index();
                    let present = *start.entry(k).or_insert_with(|| {
                        out.delta.ins[spo].contains(&k)
                            || (in_base && !out.delta.del[spo].contains(&k))
                    });
                    match (op == WAL_INSERT, present) {
                        (true, false) => ins += 1,
                        (true, true) => del -= 1,
                        (false, true) => del += 1,
                        (false, false) => ins -= 1,
                    }
                    apply(&mut out.delta, &q, op == WAL_INSERT, in_base);
                }
                start.clear();
                let (ins, del) = (ins.max(0) as u64, del.max(0) as u64);
                quads = (quads + ins).saturating_sub(del);
                out.next_bnode = out.next_bnode.max(q[0].0);
                out.version += 1;
                let prev = out.commits.last().copied().unwrap_or(from.base);
                match meta {
                    Some(Ok((seq, ts, kind, flags))) => {
                        seen_v2 = true;
                        if seq != prev.seq + 1 {
                            return Err(Error::Corrupt(format!(
                                "{}: commit {seq} follows commit {}",
                                from.path.display(),
                                prev.seq
                            )));
                        }
                        out.commits.push(CommitInfo {
                            seq,
                            timestamp_ms: ts,
                            kind,
                            inserted: ins,
                            deleted: del,
                            quads,
                            generation: from.gen_no,
                            bulk: false,
                            exact: true,
                            reconstructed: false,
                            default_graph,
                            unvalidated: flags & commit::WAL_FLAG_UNVALIDATED != 0,
                        });
                    }
                    // a legacy commit record: folded into the baseline when the
                    // database is being upgraded, otherwise numbered in order
                    _ if from.fold_legacy && !seen_v2 => out.base_quads = quads,
                    _ => out.commits.push(CommitInfo {
                        seq: prev.seq + 1,
                        timestamp_ms: prev.timestamp_ms,
                        kind: CommitKind::Unknown,
                        inserted: ins,
                        deleted: del,
                        quads,
                        generation: from.gen_no,
                        bulk: false,
                        exact: true,
                        reconstructed: true,
                        default_graph,
                        unvalidated: false,
                    }),
                }
                if from.keep_touched && out.commits.len() > before {
                    out.touched.push((out.commits.last().unwrap().seq, touched));
                }
                out.good = (i + 1) * WAL_REC;
                out.index.note(wal::WalPoint {
                    seq: out.commits.last().map_or(from.base.seq, |c| c.seq),
                    offset: out.good as u64,
                    folding: from.fold_legacy && !seen_v2,
                });
                txn_start = i + 1;
            }
            // zeros in the last transaction: torn
            _ if last_txn.is_some_and(|t| i >= t) && wal::is_zero_record(rec) => break,
            // damage before the last commit record is not a torn tail: truncating here
            // would drop the committed transactions after it
            op if last_commit.is_some_and(|l| i < l) => {
                return Err(Error::Corrupt(format!(
                    "{}: unknown record type {op} at byte {}, before the last commit",
                    from.path.display(),
                    i * WAL_REC
                )));
            }
            _ => break,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::RdfFormat;

    #[test]
    fn poisoned_writer_refuses_transaction_admission_and_vocabulary_appends() {
        let store = Store::in_memory(StoreOptions::default());
        store.writer.lock().poisoned = true;
        assert!(matches!(
            store.try_write_with(CommitKind::Load, Default::default()),
            Err(Error::Poisoned)
        ));
        let before = store.snapshot().generation.dvocab.len();
        let mut txn = store.write();
        assert!(matches!(txn.intern_key(b"unused"), Err(Error::Poisoned)));
        assert_eq!(store.snapshot().generation.dvocab.len(), before);
    }

    std::thread_local! {
        pub(super) static WRITE_BASE_PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[test]
    fn effective_changes_reuse_delta_membership_and_probe_base_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let store = Store::open(&path, StoreOptions::default()).unwrap();
        let base_quad = quad("base", "p", "object");
        store
            .load(&[Source::from_bytes(
                format!("{base_quad} .\n").into_bytes(),
                RdfFormat::NQuads,
                None,
            )])
            .unwrap();
        let mut w = store.write();
        let base = w.encode_quad(&base_quad, &mut Default::default()).unwrap();
        let added = w
            .encode_quad(&quad("added", "p", "object"), &mut Default::default())
            .unwrap();
        assert!(w.delete(base).unwrap());
        assert!(w.insert(added).unwrap());
        w.commit().unwrap();
        drop(store);
        // Admission now starts from nonempty del/ins reconstructed by WAL replay.
        let store = Store::open(&path, StoreOptions::default()).unwrap();
        let held = store.snapshot();
        let mut w = store.write();
        let sequence = [
            (base, false, false, 0),
            (base, true, true, 0),
            (base, true, false, 1),
            (base, false, true, 1),
            (added, true, false, 0),
            (added, false, true, 0),
            (added, false, false, 1),
            (added, true, true, 1),
        ];
        let mut expected = std::collections::BTreeSet::from([added]);
        for (q, insert, changed, probes) in sequence {
            WRITE_BASE_PROBES.with(|n| n.set(0));
            assert_eq!(
                if insert { w.insert(q) } else { w.delete(q) }.unwrap(),
                changed
            );
            assert_eq!(WRITE_BASE_PROBES.with(|n| n.get()), probes);
            if insert {
                expected.insert(q);
            } else {
                expected.remove(&q);
            }
            let view = w.view();
            for p in Perm::ALL {
                let actual: std::collections::BTreeSet<_> = view
                    .scan_keys(p, &[])
                    .unwrap()
                    .iter()
                    .map(|k| p.to_quad(k))
                    .collect();
                assert_eq!(actual, expected, "{p:?}");
            }
        }
        assert!(!w.commit().unwrap().committed, "all changes cancel");
        assert_eq!(store.snapshot().commit, held.commit);
        for p in Perm::ALL {
            assert_eq!(
                held.scan_keys(p, &[]).unwrap(),
                store.snapshot().scan_keys(p, &[]).unwrap()
            );
        }
    }

    const TTL: &str = r#"
@prefix ex: <http://ex.org/> .
ex:a ex:p 1, 2, 3 . ex:b ex:p 2 . ex:c ex:q "hello"@en .
"#;

    fn src() -> Source {
        Source::from_bytes(TTL.as_bytes().to_vec(), RdfFormat::Turtle, None)
    }

    /// `<http://ex.org/s> <http://ex.org/p> <http://ex.org/o>` in the default graph.
    fn quad(s: &str, p: &str, o: &str) -> Quad {
        let n = |x: &str| NamedNode::new_unchecked(format!("http://ex.org/{x}"));
        Quad::new(n(s), n(p), n(o), GraphName::DefaultGraph)
    }

    #[test]
    fn commits_keep_the_disk_reserve_and_the_memory_limit() {
        let many = |n: usize| {
            let mut nt = String::new();
            for i in 0..n {
                nt.push_str(&format!(
                    "<http://ex.org/s{i}> <http://ex.org/p> \"{i}\" .\n"
                ));
            }
            Source::from_bytes(nt.into_bytes(), RdfFormat::NTriples, None)
        };
        let full = |e: Error| assert!(matches!(e, Error::StorageFull(_)), "{e}");
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        Store::open(&root, StoreOptions::default())
            .unwrap()
            .load(&[src()])
            .unwrap();
        // a reserve no disk has: small commits, rebuilds and compaction are refused
        let opts = StoreOptions {
            min_free_disk_bytes: Some(u64::MAX / 2),
            ..Default::default()
        };
        let store = Store::open(&root, opts).unwrap();
        let head = store.head_commit().seq;
        let entries = || std::fs::read_dir(&root).unwrap().count();
        let before = entries();
        full(store.load(&[many(10)]).unwrap_err());
        full(store.load(&[many(300_000)]).unwrap_err());
        full(store.compact().unwrap_err());
        assert_eq!(store.head_commit().seq, head);
        assert_eq!(store.snapshot().len(), 5);
        assert_eq!(entries(), before, "no unfinished generation is left behind");
        drop(store);
        // a reserve the disk keeps
        let opts = StoreOptions {
            min_free_disk_bytes: Some(1),
            ..Default::default()
        };
        Store::open(&root, opts).unwrap().load(&[many(10)]).unwrap();

        // in memory: growth past the limit is refused, whichever path the write takes
        let opts = StoreOptions {
            max_memory_bytes: Some(64 << 10),
            ..Default::default()
        };
        let mem = Store::in_memory(opts.clone());
        full(mem.load(&[many(20_000)]).unwrap_err());
        assert!(mem.snapshot().is_empty());
        mem.load(&[many(10)]).unwrap();
        full(mem.load(&[many(5_000)]).unwrap_err());
        assert_eq!(mem.snapshot().len(), 10);
        // shrinking is always allowed
        let mut t = mem.write();
        let k = mem.snapshot().scan_keys(Perm::Spo, &[]).unwrap()[0];
        assert!(t.delete(Perm::Spo.to_quad(&k)).unwrap());
        t.commit().unwrap();
        assert_eq!(mem.snapshot().len(), 9);
    }

    #[test]
    fn a_write_that_will_not_wait_fails_while_the_writer_lock_is_held() {
        use crate::guard::WriteOptions;
        let store = Store::in_memory(Default::default());
        let now = WriteOptions {
            no_wait: true,
            ..Default::default()
        };
        let held = store.write();
        let e = store
            .try_write_with(CommitKind::Update, now.clone())
            .map(|_| ())
            .unwrap_err();
        assert!(matches!(e, Error::WriterBusy), "{e}");
        drop(held);
        let mut t = store.try_write_with(CommitKind::Update, now).unwrap();
        let q = t
            .encode_quad(&quad("x", "p", "y"), &mut Default::default())
            .unwrap();
        assert!(t.insert(q).unwrap());
        assert!(t.commit().unwrap().committed);
    }

    /// Commits sync the delta vocabulary only when they added terms to it; terms of
    /// every kind of commit are there after a reopen.
    #[test]
    fn delta_terms_survive_reopen_whether_or_not_a_commit_added_any() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let store = Store::open(&root, Default::default()).unwrap();
        store.load(&[src()]).unwrap();
        let commit = |store: &Store, s: &str, o: &str, insert: bool| {
            let mut t = store.write();
            let q = t
                .encode_quad(&quad(s, "p", o), &mut Default::default())
                .unwrap();
            if insert {
                assert!(t.insert(q).unwrap());
            } else {
                assert!(t.delete(q).unwrap());
            }
            t.commit().unwrap();
        };
        // new terms, then the same terms again (nothing new to sync), then new ones
        commit(&store, "n1", "o1", true);
        commit(&store, "n1", "o1", false);
        commit(&store, "n1", "o1", true);
        commit(&store, "n2", "o2", true);
        let head = store.head_commit().seq;
        drop(store);
        let store = Store::open(&root, Default::default()).unwrap();
        assert_eq!(store.head_commit().seq, head);
        let snap = store.snapshot();
        let mut found = Vec::new();
        snap.for_each_quad(|q| {
            if let Some(t) = snap.quad_to_terms(q) {
                found.push(t.to_string());
            }
            Ok(())
        })
        .unwrap();
        for s in ["n1", "n2"] {
            assert!(
                found
                    .iter()
                    .any(|t| t.contains(&format!("http://ex.org/{s}>"))),
                "{s} after reopen: {found:?}"
            );
        }
    }

    #[test]
    fn vocabulary_worker_is_lazy_reused_and_generation_safe() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Default::default()).unwrap();
        store.load(&[src()]).unwrap();
        assert!(store.guarded_writer().vocab_sync.thread_id().is_none());
        let commit = |s: &str, o: &str| {
            let mut t = store.write();
            let q = t
                .encode_quad(&quad(s, "p", o), &mut Default::default())
                .unwrap();
            t.insert(q).unwrap();
            t.commit().unwrap()
        };
        // Terms in the bulk-built base do not create the worker.
        let mut t = store.write();
        let q = t
            .encode_quad(&quad("a", "p", "b"), &mut Default::default())
            .unwrap();
        t.insert(q).unwrap();
        t.commit().unwrap();
        assert!(store.guarded_writer().vocab_sync.thread_id().is_none());
        commit("fresh1", "fresh-object1");
        let id = store.guarded_writer().vocab_sync.thread_id().unwrap();
        commit("fresh2", "fresh-object2");
        assert_eq!(store.guarded_writer().vocab_sync.thread_id(), Some(id));
        store.compact().unwrap();
        commit("fresh3", "fresh-object3");
        assert_eq!(store.guarded_writer().vocab_sync.thread_id(), Some(id));
        let head = store.snapshot().commit;
        let len = store.snapshot().len();
        drop(store);
        let reopened = Store::open(dir.path(), Default::default()).unwrap();
        assert_eq!(reopened.snapshot().commit, head);
        assert_eq!(reopened.snapshot().len(), len);
        assert!(reopened.guarded_writer().vocab_sync.thread_id().is_none());
    }

    #[test]
    fn vocabulary_worker_spawn_failure_keeps_commits_durable() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Default::default()).unwrap();
        store.guarded_writer().vocab_sync.fail_spawn = true;
        let mut txn = store.write();
        let q = txn
            .encode_quad(&quad("s", "p", "o"), &mut Default::default())
            .unwrap();
        txn.insert(q).unwrap();
        let receipt = txn.commit().unwrap();
        assert!(store.guarded_writer().vocab_sync.thread_id().is_none());
        drop(store);
        let store = Store::open(dir.path(), Default::default()).unwrap();
        assert_eq!(store.snapshot().commit, receipt.commit.seq);
        assert_eq!(store.snapshot().len(), 1);
    }

    #[test]
    fn a_failed_sync_of_new_terms_fails_the_commit_and_stops_writes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let store = Store::open(&root, Default::default()).unwrap();
        store.load(&[src()]).unwrap();
        let insert = |store: &Store, s: &str| {
            let mut t = store.write();
            let q = t
                .encode_quad(&quad(s, "p", "o"), &mut Default::default())
                .unwrap();
            assert!(t.insert(q).unwrap());
            t.commit()
        };
        let head = insert(&store, "n1").unwrap().commit.seq;
        let len = store.snapshot().len();
        store.snapshot().generation.dvocab.fail_next_sync();
        // the WAL records are written and synced, the terms' sync fails
        assert!(insert(&store, "n2").is_err());
        // nothing was published, and no further write is accepted: the outcome of the
        // failed commit is known only after the next open, as for a failed WAL sync
        assert_eq!(store.head_commit().seq, head);
        assert_eq!(store.snapshot().len(), len);
        assert!(matches!(insert(&store, "n3"), Err(Error::Poisoned)));
        drop(store);
        // the terms reached the file (only their sync failed): the commit is there
        let store = Store::open(&root, Default::default()).unwrap();
        assert_eq!(store.head_commit().seq, head + 1);
        assert_eq!(insert(&store, "n3").unwrap().commit.seq, head + 2);
    }

    #[test]
    fn cancelled_and_timed_out_writes_publish_nothing() {
        use crate::guard::WriteOptions;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        // every load a rebuild, so the bulk path is covered too
        let bulk = StoreOptions {
            bulk_threshold: 0,
            ..Default::default()
        };
        let store = Store::open(&root, bulk).unwrap();
        store.load(&[src()]).unwrap();
        let head = store.head_commit().seq;
        let gens = || std::fs::read_dir(&root).unwrap().count();
        let before = gens();
        let cancelled = WriteOptions {
            cancel: Some(Arc::new(AtomicBool::new(true))),
            ..Default::default()
        };
        let late = WriteOptions {
            deadline: Some(std::time::Instant::now()),
            ..Default::default()
        };
        let more = || {
            Source::from_bytes(
                b"<http://ex.org/x> <http://ex.org/p> \"9\" .".to_vec(),
                RdfFormat::NTriples,
                None,
            )
        };
        for o in [&cancelled, &late] {
            let e = store.load_with(&[more()], CommitKind::Load, o).unwrap_err();
            assert!(matches!(e, Error::Cancelled | Error::Timeout), "{e}");
            let e = store
                .replace_with(ReplaceTarget::All, &[more()], CommitKind::GspPut, o)
                .unwrap_err();
            assert!(matches!(e, Error::Cancelled | Error::Timeout), "{e}");
        }
        assert_eq!(store.head_commit().seq, head);
        assert_eq!(
            gens(),
            before,
            "an interrupted rebuild leaves no generation"
        );
        drop(store);

        // the transactional path, and the wait for the writer lock
        let store = Arc::new(Store::open(&root, StoreOptions::default()).unwrap());
        let e = store
            .load_with(&[more()], CommitKind::Load, &cancelled)
            .unwrap_err();
        assert!(matches!(e, Error::Cancelled), "{e}");
        let (held, release) = (
            Arc::new(std::sync::Barrier::new(2)),
            Arc::new(std::sync::Barrier::new(2)),
        );
        let holder = std::thread::spawn({
            let (store, held, release) = (store.clone(), held.clone(), release.clone());
            move || {
                let _txn = store.write();
                held.wait();
                release.wait();
            }
        });
        held.wait();
        let o = WriteOptions {
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_millis(50)),
            ..Default::default()
        };
        assert!(matches!(
            store.try_write_with(CommitKind::Update, o).err(),
            Some(Error::Timeout)
        ));
        release.wait();
        holder.join().unwrap();
        assert_eq!(store.head_commit().seq, head);
        assert_eq!(store.snapshot().len(), 5);
    }

    #[test]
    fn persistent_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        {
            let store = Store::open(&root, StoreOptions::default()).unwrap();
            assert!(store.snapshot().is_empty());
            store.load(&[src()]).unwrap();
            assert_eq!(store.snapshot().len(), 5);
            // update: insert + delete
            let s0 = store.snapshot();
            let mut t = store.write();
            let a = t
                .intern(&Term::NamedNode(named("http://ex.org/a")))
                .unwrap();
            let p = t
                .intern(&Term::NamedNode(named("http://ex.org/p")))
                .unwrap();
            let z = t
                .intern(&Term::NamedNode(named("http://ex.org/new")))
                .unwrap();
            assert_eq!(z.tag(), Tag::Delta);
            assert!(t.insert([a, p, z, Id::DEFAULT_GRAPH]).unwrap());
            assert!(
                t.delete([a, p, Id::from_i64(1).unwrap(), Id::DEFAULT_GRAPH])
                    .unwrap()
            );
            assert!(
                !t.delete([a, p, Id::from_i64(99).unwrap(), Id::DEFAULT_GRAPH])
                    .unwrap()
            );
            t.commit().unwrap();
            // old snapshot unaffected (MVCC)
            assert_eq!(s0.len(), 5);
            let s = store.snapshot();
            assert_eq!(s.len(), 5);
            let rows = s.scan_keys(Perm::Spo, &[a.0, p.0]).unwrap();
            assert_eq!(rows.len(), 3);
            assert!(rows.iter().any(|k| k[2] == z.0));
            assert_eq!(s.count(Perm::Pso, &[p.0]).unwrap(), 4);
        }
        // reopen: WAL replay
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        let s = store.snapshot();
        assert_eq!(s.len(), 5);
        let p = s.lookup_iri("http://ex.org/p").unwrap();
        assert_eq!(s.count(Perm::Pso, &[p.0]).unwrap(), 4);
        assert!(s.lookup_iri("http://ex.org/new").is_some());
        // compact merges delta
        store.compact().unwrap();
        let s = store.snapshot();
        assert!(s.delta.is_empty());
        assert_eq!(s.len(), 5);
        assert_eq!(s.generation.name, "gen-0003");
        let new = s.lookup_iri("http://ex.org/new").unwrap();
        assert_eq!(new.tag(), Tag::Vocab);
        drop(s);
        drop(store);
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(store.snapshot().len(), 5);
        assert!(store.prefixes().contains_key("ex"));
    }

    #[test]
    fn bulk_insert_rebuilds() {
        let dir = tempfile::tempdir().unwrap();
        let opts = StoreOptions {
            bulk_threshold: 10,
            ..Default::default()
        };
        let store = Store::open(&dir.path().join("db"), opts.clone()).unwrap();
        store.load(&[src()]).unwrap();
        let mut t = store.write();
        let p = t
            .intern(&Term::NamedNode(named("http://ex.org/bulk")))
            .unwrap();
        let quads: Vec<[Id; 4]> = (0..100)
            .map(|i| {
                [
                    Id::bnode(1000 + i),
                    p,
                    Id::from_i64(i as i64).unwrap(),
                    Id::DEFAULT_GRAPH,
                ]
            })
            .collect();
        t.insert_bulk(quads).unwrap();
        let a = t
            .intern(&Term::NamedNode(named("http://ex.org/a")))
            .unwrap();
        let q = t
            .intern(&Term::NamedNode(named("http://ex.org/p")))
            .unwrap();
        t.delete([a, q, Id::from_i64(1).unwrap(), Id::DEFAULT_GRAPH])
            .unwrap();
        t.commit().unwrap();
        let s = store.snapshot();
        assert_eq!(s.len(), 5 + 100 - 1);
        assert!(s.delta.is_empty());
        let p = s.lookup_iri("http://ex.org/bulk").unwrap();
        assert_eq!(p.tag(), Tag::Vocab);
        assert_eq!(s.count(Perm::Pso, &[p.0]).unwrap(), 100);
        drop(s);
        drop(store);
        let store = Store::open(&dir.path().join("db"), opts).unwrap();
        assert_eq!(store.snapshot().len(), 104);
    }

    #[test]
    fn scans_merge_the_delta_into_base_blocks() {
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        // several blocks per permutation, some quads in a named graph
        let mut nt = String::new();
        for i in 0..90_000u64 {
            let g = if i % 13 == 0 {
                " <http://ex.org/g>"
            } else {
                ""
            };
            nt.push_str(&format!(
                "<http://ex.org/s{}> <http://ex.org/p{}> <http://ex.org/o{}>{g} .\n",
                i % 5000,
                i % 7,
                (i * 31) % 9973
            ));
        }
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(nt.into_bytes(), RdfFormat::NQuads, None)])
            .unwrap();
        assert!(store.snapshot().perm(Perm::Spo).blocks.len() > 2);
        let mut model: std::collections::BTreeSet<[Id; 4]> = Default::default();
        store
            .snapshot()
            .for_each_quad(|q| {
                model.insert(*q);
                Ok(())
            })
            .unwrap();
        let quads: Vec<[Id; 4]> = model.iter().copied().collect();
        let pick = |r: u64| quads[r as usize % quads.len()];
        // deletes, inserts of new combinations of terms (clustered and spread out),
        // re-inserts of deleted quads, over a few commits
        let mut deleted = Vec::new();
        for _ in 0..4 {
            let mut t = store.write();
            for _ in 0..300 {
                match next() % 4 {
                    0 | 1 => {
                        let q = pick(next());
                        t.delete(q).unwrap();
                        model.remove(&q);
                        deleted.push(q);
                    }
                    2 => {
                        let (a, b, c) = (pick(next()), pick(next()), pick(next()));
                        let q = [a[0], b[1], c[2], pick(next())[3]];
                        t.insert(q).unwrap();
                        model.insert(q);
                    }
                    _ if !deleted.is_empty() => {
                        let q = deleted[next() as usize % deleted.len()];
                        t.insert(q).unwrap();
                        model.insert(q);
                    }
                    _ => {}
                }
            }
            t.commit().unwrap();
        }
        let snap = store.snapshot();
        for perm in Perm::ALL {
            let keys: Vec<Key> = model.iter().map(|q| perm.to_key(q)).collect();
            let mut keys = keys;
            keys.sort_unstable();
            let some = |r: u64| keys[r as usize % keys.len()];
            let mut ranges = vec![([0; 4], [u64::MAX; 4])];
            for _ in 0..6 {
                let k = some(next());
                ranges.push((pad(&k[..1], 0), pad(&k[..1], u64::MAX)));
                let (a, b) = (some(next()), some(next()));
                ranges.push((a.min(b), a.max(b)));
            }
            for (lo, hi) in ranges {
                for mask in [crate::index::ALL_COLS, 0b0001, 0b0110, 0b1000] {
                    let want: Vec<Vec<u64>> = keys
                        .iter()
                        .filter(|k| **k >= lo && **k <= hi)
                        .map(|k| {
                            (0..4)
                                .filter(|c| mask & (1 << c) != 0)
                                .map(|c| k[c])
                                .collect()
                        })
                        .collect();
                    let mut got: Vec<Vec<u64>> = Vec::new();
                    snap.scan_between_cols(perm, lo, hi, mask, |c| {
                        match c {
                            Chunk::Block(b, s, e) => got.extend((s..e).map(|i| {
                                (0..4)
                                    .filter(|c| mask & (1 << c) != 0)
                                    .map(|c| b.cols[c][i])
                                    .collect()
                            })),
                            Chunk::Row(k) => got.push(
                                (0..4)
                                    .filter(|c| mask & (1 << c) != 0)
                                    .map(|c| k[c])
                                    .collect(),
                            ),
                        }
                        Ok(true)
                    })
                    .unwrap();
                    assert_eq!(got, want, "{perm:?} {lo:?}..={hi:?} mask {mask:b}");
                }
            }
        }
    }

    #[test]
    fn predicate_stats_include_delta() {
        let store = Store::in_memory(StoreOptions::default());
        store.load(&[src()]).unwrap();
        let mut t = store.write();
        let p = t
            .intern(&Term::NamedNode(named("http://ex.org/new")))
            .unwrap();
        let old_p = t
            .intern(&Term::NamedNode(named("http://ex.org/p")))
            .unwrap();
        for i in 0..100 {
            let s = Id::bnode(10_000 + i % 10);
            t.insert([s, p, Id::from_i64(i as i64).unwrap(), Id::DEFAULT_GRAPH])
                .unwrap();
        }
        t.insert([
            Id::bnode(20_000),
            old_p,
            Id::from_i64(7).unwrap(),
            Id::DEFAULT_GRAPH,
        ])
        .unwrap();
        t.commit().unwrap();
        let s = store.snapshot();
        let ps = s.predicate_stat(p.0).unwrap();
        assert_eq!(
            (ps.count, ps.distinct_subjects, ps.distinct_objects),
            (100, 10, 100)
        );
        // base statistics combined with the delta
        let ps = s.predicate_stat(old_p.0).unwrap();
        assert_eq!(ps.count, 5);
        assert_eq!(ps.distinct_subjects, 3);
    }

    #[test]
    fn database_directory_is_locked() {
        let dir = tempfile::tempdir().unwrap();
        let a = Store::open(dir.path(), StoreOptions::default()).unwrap();
        let err = Store::open(dir.path(), StoreOptions::default())
            .err()
            .unwrap();
        assert!(err.to_string().contains("in use"), "{err}");
        drop(a);
        Store::open(dir.path(), StoreOptions::default()).unwrap();
    }

    #[test]
    fn mem_store_and_graphs() {
        let store = Store::in_memory(StoreOptions::default());
        let trig = r#"@prefix ex: <http://ex.org/> .
            ex:s ex:p ex:o .
            ex:g1 { ex:s ex:p ex:o1 . }
            ex:g2 { ex:s ex:p ex:o2 . ex:s ex:p ex:o3 . }"#;
        store
            .load(&[Source::from_bytes(
                trig.as_bytes().to_vec(),
                RdfFormat::TriG,
                None,
            )])
            .unwrap();
        let s = store.snapshot();
        assert_eq!(s.len(), 4);
        let graphs = s.graph_ids().unwrap();
        assert_eq!(graphs.len(), 2);
        // small incremental load goes through the delta
        store
            .load(&[Source::from_bytes(
                b"<http://ex.org/x> <http://ex.org/p> \"v\" .".to_vec(),
                RdfFormat::NTriples,
                Some(named("http://ex.org/g3")),
            )])
            .unwrap();
        let s = store.snapshot();
        assert_eq!(s.len(), 5);
        assert_eq!(s.delta.inserts(), 1);
        assert_eq!(s.graph_ids().unwrap().len(), 3);
        let mut n = 0;
        s.for_each_quad(|_| {
            n += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(n, 5);
    }
}
