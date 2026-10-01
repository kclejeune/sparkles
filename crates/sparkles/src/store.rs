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
mod geo;
pub use backup::{BackupCapture, CapturedFile, FileKind, FileSource, LeaseGuard};

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
use std::sync::atomic::{AtomicBool, Ordering};

/// Immutable base index generation.
pub struct Generation {
    /// unique within the process: two openings of one directory differ
    pub uid: u64,
    pub name: String,
    pub dir: Option<PathBuf>,
    pub vocab: Vocab,
    pub perms: Vec<PermIndex>,
    pub stats: Stats,
    pub meta: IndexMeta,
    pub dvocab: DeltaVocab,
    /// keeps a temporary directory alive for in-memory stores with a bulk-built base
    _tmp: Option<tempfile::TempDir>,
    /// packed vectors of the base index, built on first search
    pub vectors: crate::vector::GenerationVectors,
    /// the spatial index's geometry column and base tree for this generation
    pub geo: crate::geo::GenerationGeo,
}

impl Generation {
    fn empty(dvocab: DeltaVocab) -> Generation {
        Generation {
            uid: crate::index::next_uid(),
            name: "mem".into(),
            dir: None,
            vocab: Vocab::empty(),
            perms: Perm::ALL.iter().map(|&p| PermIndex::empty(p)).collect(),
            stats: Stats::default(),
            meta: IndexMeta::default(),
            dvocab,
            _tmp: None,
            vectors: Default::default(),
            geo: Default::default(),
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
            vocab: Vocab::open(dir)?,
            perms: Perm::ALL
                .iter()
                .map(|&p| PermIndex::open(dir, p))
                .collect::<Result<_>>()?,
            stats,
            meta,
            dvocab,
            _tmp: None,
            vectors: Default::default(),
            geo: Default::default(),
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
    fn range<'a>(set: &'a OrdSet<Key>, prefix: &[u64]) -> impl Iterator<Item = &'a Key> + 'a {
        Self::key_range(set, pad(prefix, 0), pad(prefix, u64::MAX))
    }
    fn key_range(set: &OrdSet<Key>, lo: Key, hi: Key) -> impl Iterator<Item = &Key> + '_ {
        set.range((Bound::Included(lo), Bound::Included(hi)))
    }
}

/// A consistent, immutable view of the store.
#[derive(Clone)]
pub struct Snapshot {
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
            Tag::Vocab | Tag::Delta => self.key(id).map(|k| id::key_to_term(&k)),
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
    /// 0). When the delta has changes in the range, every column is decoded, because
    /// the merge compares full keys.
    pub fn scan_between_cols(
        &self,
        perm: Perm,
        lo: Key,
        hi: Key,
        mask: crate::index::ColMask,
        mut f: impl FnMut(Chunk<'_>) -> Result<bool>,
    ) -> Result<()> {
        let pi = perm.index();
        let mut ins = Delta::key_range(&self.delta.ins[pi], lo, hi).peekable();
        let mut del = Delta::key_range(&self.delta.del[pi], lo, hi).peekable();
        let mask = if ins.peek().is_some() || del.peek().is_some() {
            crate::index::ALL_COLS
        } else {
            mask
        };
        let base = self.perm(perm);
        let mut stop = false;
        if base.rows > 0 {
            let r = base.for_each_key_range_cols(&self.cache, &lo, &hi, mask, |blk, s, e| {
                if stop {
                    return Ok(!stop);
                }
                let last = blk.key(e - 1);
                let ins_hit = ins.peek().is_some_and(|k| **k <= last);
                let del_hit = del.peek().is_some_and(|k| **k <= last);
                if !ins_hit && !del_hit {
                    if !f(Chunk::Block(blk, s, e))? {
                        stop = true;
                    }
                    return Ok(!stop);
                }
                // merge row by row
                let mut run_start = s;
                for i in s..e {
                    let k = blk.key(i);
                    let mut flush_before = false;
                    let mut skip = false;
                    if ins.peek().is_some_and(|x| **x < k) {
                        flush_before = true;
                    }
                    if del.peek().is_some_and(|x| **x == k) {
                        skip = true;
                    }
                    if flush_before || skip {
                        if run_start < i && !f(Chunk::Block(blk, run_start, i))? {
                            stop = true;
                            return Ok(!stop);
                        }
                        while let Some(x) = ins.peek().filter(|x| ***x < k) {
                            if !f(Chunk::Row(**x))? {
                                stop = true;
                                return Ok(!stop);
                            }
                            ins.next();
                        }
                        if skip {
                            del.next();
                            run_start = i + 1;
                        } else {
                            run_start = i;
                        }
                    }
                    // deletes that don't exist in base (shouldn't happen) are skipped
                    while del.peek().is_some_and(|x| **x < k) {
                        del.next();
                    }
                }
                if run_start < e && !f(Chunk::Block(blk, run_start, e))? {
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
    pub fn for_each_quad(&self, mut f: impl FnMut(&[Id; 4]) -> Result<()>) -> Result<()> {
        self.scan(Perm::Gspo, &[], |c| {
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

pub fn bnode_for(id: Id) -> BlankNode {
    BlankNode::new_unchecked(format!("b{:x}", id.payload()))
}

/// Blank node labels produced by [`bnode_for`] map back to the same id.
pub fn parse_bnode_label(label: &str) -> Option<Id> {
    let hex = label.strip_prefix('b')?;
    u64::from_str_radix(hex, 16).ok().map(Id::bnode)
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
    /// Prefixes per dataset (0: unlimited): [`Store::set_prefix`] refuses a new one
    /// past it, and the prefixes of loaded data stop being added.
    pub max_prefixes: usize,
    /// Memory for the spatial index (geometry column and trees); a build that would
    /// exceed it is refused and queries run without the index.
    pub geo_budget_bytes: u64,
    /// Largest sum of input vertices of one geometry operation (overlay, buffer, hull,
    /// relate); larger ones are a type error.
    pub geo_op_vertices: u64,
}

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
            max_prefixes: DEFAULT_MAX_PREFIXES,
            geo_budget_bytes: 4 << 30,
            geo_op_vertices: 2_000_000,
        }
    }
}

struct WriterState {
    wal: Option<BufWriter<File>>,
    next_bnode: u64,
    /// the latest commit
    head: CommitInfo,
    /// a WAL or generation write failed after a commit started: refuse further writes
    poisoned: bool,
    /// the store is being dropped: a backup lease released later must not collect
    closed: bool,
}

/// Commit metadata for a transaction that is committed by rebuilding the generation.
struct BulkCommit {
    kind: CommitKind,
    /// quads the transaction deleted (net) before its bulk batch
    net_del: u64,
    /// quads in the committed snapshot the transaction started from
    start_len: u64,
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct Store {
    root: Option<PathBuf>,
    opts: StoreOptions,
    // `current`, `writer`, `catalog`, `clock` and `history` are shared (weakly) with
    // backup lease guards, whose drop collects history without a `&Store`
    current: Arc<ArcSwap<Snapshot>>,
    writer: Arc<Mutex<WriterState>>,
    cache: Arc<BlockCache>,
    results: Arc<crate::sparql::cache::ResultCache>,
    prefixes: Mutex<BTreeMap<String, String>>,
    /// exclusive OS lock on `<root>/sparkles.lock` (TDB2 `tdb.lock`), held while open
    _lock: Option<File>,
    dataset_id: uuid::Uuid,
    catalog: Arc<Mutex<Catalog>>,
    /// test hook replacing the wall clock (milliseconds since the epoch)
    clock: Arc<Mutex<Option<Clock>>>,
    /// full-text index, when enabled for this dataset
    text: arc_swap::ArcSwapOption<crate::text::TextIndex>,
    /// spatial index, when enabled for this dataset
    geo: arc_swap::ArcSwapOption<crate::geo::GeoIndex>,
    /// pins, retention, generations and materialized past states (persistent stores)
    history: Option<Arc<Mutex<crate::history::HistoryState>>>,
    /// write guard checked before every commit (write-time validation)
    guard: parking_lot::RwLock<Option<Arc<dyn crate::guard::CommitGuard>>>,
    /// told the outcome of every guard decision (metrics)
    guard_observer: parking_lot::RwLock<Option<Arc<dyn crate::guard::GuardObserver>>>,
    /// `validation.json` asks for a guard: commits fail without one (fail closed)
    guard_required: AtomicBool,
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
            Err(Error::Invalid(format!(
                "database {} is in use by another process (pid {}); stop it or talk to it over HTTP",
                root.display(),
                pid.trim()
            )))
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
        let cache = Arc::new(BlockCache::new(opts.cache_bytes));
        let results = Arc::new(crate::sparql::cache::ResultCache::new(
            opts.result_cache_bytes,
            opts.result_cache_min_ms,
        ));
        let gen_ = Arc::new(Generation::empty(DeltaVocab::in_memory()));
        let dataset_id = uuid::Uuid::new_v4();
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
        };
        let store = Store {
            root: None,
            current: Arc::new(ArcSwap::from_pointee(Snapshot {
                generation: gen_,
                delta: Delta::default(),
                version: 0,
                cache: cache.clone(),
                results: results.clone(),
                dvocab_len: 0,
                commit: 0,
                text: None,
                geo: None,
                union_default_graph: opts.union_default_graph,
                geo_op_vertices: opts.geo_op_vertices,
                delta_stats: Default::default(),
                historical: false,
            })),
            writer: Arc::new(Mutex::new(WriterState {
                wal: None,
                next_bnode: 0,
                head: root,
                poisoned: false,
                closed: false,
            })),
            cache,
            results,
            prefixes: Mutex::new(BTreeMap::new()),
            _lock: None,
            dataset_id,
            catalog: Arc::new(Mutex::new(Catalog::memory(root, opts.memory_commit_ring))),
            clock: Arc::new(Mutex::new(None)),
            text: Default::default(),
            geo: Default::default(),
            history: None,
            guard: parking_lot::RwLock::new(None),
            guard_observer: parking_lot::RwLock::new(None),
            guard_required: AtomicBool::new(false),
            #[cfg(any(test, feature = "failpoints"))]
            failpoints: Default::default(),
            opts,
        };
        store.open_geo();
        store
    }

    /// Open (or create) a persistent store rooted at `root`.
    pub fn open(root: &Path, opts: StoreOptions) -> Result<Store> {
        std::fs::create_dir_all(root)?;
        let lock = lock_dir(root)?;
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
        let gen_ = Generation::open(&root.join(&name), &name, true)?;
        let gen_no = commit::generation_number(&name);
        let cache = Arc::new(BlockCache::new(opts.cache_bytes));
        let results = Arc::new(crate::sparql::cache::ResultCache::new(
            opts.result_cache_bytes,
            opts.result_cache_min_ms,
        ));
        let mut next_bnode = gen_.meta.next_bnode;
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
                };
                (c, true)
            }
        };
        // replay the WAL
        let wal_path = root.join(&name).join("wal.log");
        let mut delta = Delta::default();
        let mut version = 0;
        let gen_ = Arc::new(gen_);
        let mut replayed: Vec<CommitInfo> = Vec::new();
        // the quads each replayed commit changed, for catching up the full-text index
        let text_on = cfg!(feature = "text") && root.join("text.json").exists();
        let mut wal_text: Vec<(u64, Vec<[Id; 4]>)> = Vec::new();
        // quads of the base plus WAL transactions folded into a baseline commit
        let mut base_quads = gen_.meta.quads;
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
            };
            let r = replay_wal(&from, &buf, Stop::End, &mut |_, _| Ok(()))?;
            if r.good != buf.len() {
                OpenOptions::new()
                    .write(true)
                    .open(&wal_path)?
                    .set_len(r.good as u64)?;
            }
            delta = r.delta;
            version = r.version;
            replayed = r.commits;
            base_quads = r.base_quads;
            next_bnode = r.next_bnode;
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
        let head = replayed.last().copied().unwrap_or(base);
        let wal = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&wal_path)?;
        let dvocab_len = gen_.dvocab.len();
        let history = open_history(root, dataset_id, gen_no, head.seq, &catalog)?;
        let store = Store {
            root: Some(root.to_path_buf()),
            current: Arc::new(ArcSwap::from_pointee(Snapshot {
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
                historical: false,
            })),
            writer: Arc::new(Mutex::new(WriterState {
                wal: Some(BufWriter::new(wal)),
                next_bnode,
                head,
                poisoned: false,
                closed: false,
            })),
            cache,
            results,
            prefixes: Mutex::new(prefixes),
            _lock: Some(lock),
            dataset_id,
            catalog: Arc::new(Mutex::new(catalog)),
            clock: Arc::new(Mutex::new(None)),
            text: Default::default(),
            geo: Default::default(),
            history: Some(Arc::new(Mutex::new(history))),
            guard: parking_lot::RwLock::new(None),
            guard_observer: parking_lot::RwLock::new(None),
            guard_required: AtomicBool::new(guard_required_by(root)),
            #[cfg(any(test, feature = "failpoints"))]
            failpoints: Default::default(),
            opts,
        };
        store.collect_history(gen_no, head.seq);
        store.open_text(&wal_text)?;
        store.open_geo();
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
        let Some(root) = &self.root else { return };
        collect_generations(
            root,
            self.now_ms(),
            &self.catalog.lock(),
            h,
            current,
            head,
            self.opts.history_max_generations,
        );
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
                .ok_or_else(|| Error::NotFound(format!("no snapshot '{name}'")))?,
        };
        if seq > head.seq {
            return Err(Error::NotFound(format!(
                "no commit {seq} (head is {})",
                head.seq
            )));
        }
        let meta = self.catalog.lock().get(seq);
        let Some(commit) = meta else {
            let snapshot = match at {
                At::Snapshot(n) => Some(n.clone()),
                _ => None,
            };
            return Err(match &self.history {
                Some(h) => self.history_gone(&h.lock(), seq, head.seq, snapshot, None),
                None => Error::HistoryGone(Box::new(crate::history::HistoryGone {
                    message: format!("commit {seq} is no longer reconstructable"),
                    seq,
                    head: head.seq,
                    snapshot,
                    reconstructable: Vec::new(),
                    metadata: None,
                })),
            });
        };
        Ok(crate::history::Resolved {
            at: at.clone(),
            commit,
            head: head.seq,
            historical: seq != self.snapshot().commit,
        })
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
        let (Some(_), Some(hist)) = (&self.root, &self.history) else {
            return Err(Error::HistoryUnsupported(
                "point-in-time reads need a persistent dataset".into(),
            ));
        };
        let seq = r.commit.seq;
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
        if let Some(i) = h.cache.iter().position(|(k, _, _)| *k == (owner, seq)) {
            let e = h.cache.remove(i);
            let snap = e.1.clone();
            h.cache.insert(0, e);
            h.hits += 1;
            return Ok((snap, r));
        }
        h.misses += 1;
        let entry = h.gens[&owner].clone();
        let generation = if owner == current {
            live.generation.clone()
        } else if let Some(i) = h.open.iter().position(|(n, _)| *n == owner) {
            let e = h.open.remove(i);
            let g = e.1.clone();
            h.open.insert(0, e);
            g
        } else {
            let g = Arc::new(Generation::open_sealed(&entry.dir, &entry.name)?);
            h.open.insert(0, (owner, g.clone()));
            h.open.truncate(2);
            g
        };
        let budget = self.opts.history_cache_bytes;
        let delta = if seq == entry.base.seq {
            Delta::default()
        } else {
            let wal = entry.dir.join("wal.log");
            let buf = std::fs::read(&wal)?;
            let from = ReplayFrom {
                generation: &generation,
                cache: &self.cache,
                results: &self.results,
                base: entry.base,
                gen_no: owner,
                fold_legacy: entry.fold_legacy,
                next_bnode: 0,
                keep_touched: false,
                path: &wal,
            };
            let mut check = |_: u64, d: &Delta| -> Result<()> {
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
            let rep = replay_wal(&from, &buf, Stop::AfterSeq(seq), &mut check)?;
            if !rep.reached {
                return Err(Error::Corrupt(format!(
                    "{}: the log ends before commit {seq}",
                    wal.display()
                )));
            }
            rep.delta
        };
        h.materializations += 1;
        let bytes = delta_bytes(&delta);
        let dvocab_len = generation.dvocab.len();
        let snap = Arc::new(Snapshot {
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
            historical: true,
        });
        h.cache.insert(0, ((owner, seq), snap.clone(), bytes));
        let mut total: u64 = h.cache.iter().map(|e| e.2).sum();
        while total > budget && h.cache.len() > 1 {
            total -= h.cache.pop().map_or(0, |e| e.2);
        }
        Ok((snap, r))
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
            generation: owner.and_then(|o| h.gens.get(&o)).map(|g| g.name.clone()),
            reconstructable: owner.is_some(),
        }
    }

    /// The named snapshots, by commit then name.
    pub fn snapshots(&self) -> Vec<crate::history::NamedSnapshot> {
        let Some(hist) = &self.history else {
            return Vec::new();
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
        if !crate::history::valid_name(name) {
            return Err(Error::invalid(format!(
                "invalid snapshot name {name:?}: letters, digits, '.', '_' and '-', 1 to 64, starting with a letter or digit"
            )));
        }
        if note.as_ref().is_some_and(|n| n.len() > 1024) {
            return Err(Error::invalid("the note is longer than 1024 bytes"));
        }
        let (Some(root), Some(hist)) = (&self.root, &self.history) else {
            return Err(Error::HistoryUnsupported(
                "named snapshots need a persistent dataset".into(),
            ));
        };
        // no rebuild may run while the pin is being established
        let w = self.writer.lock();
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
        };
        h.pins.insert(name.to_string(), pin.clone());
        if let Err(e) = crate::history::write_file(root, self.dataset_id, &h.pins, h.retention) {
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
            return Ok(false);
        };
        let w = self.writer.lock();
        let current = commit::generation_number(&self.snapshot().generation.name);
        let mut h = hist.lock();
        let Some(pin) = h.pins.remove(name) else {
            return Ok(false);
        };
        if let Err(e) = crate::history::write_file(root, self.dataset_id, &h.pins, h.retention) {
            h.pins.insert(name.to_string(), pin);
            return Err(e);
        }
        self.collect_locked(&mut h, current, w.head.seq);
        Ok(true)
    }

    pub fn retention(&self) -> crate::history::Retention {
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
        let (Some(root), Some(hist)) = (&self.root, &self.history) else {
            return Err(Error::HistoryUnsupported(
                "retention needs a persistent dataset".into(),
            ));
        };
        {
            let w = self.writer.lock();
            let current = commit::generation_number(&self.snapshot().generation.name);
            let mut h = hist.lock();
            let old = h.retention;
            h.retention = r;
            if let Err(e) = crate::history::write_file(root, self.dataset_id, &h.pins, r) {
                h.retention = old;
                return Err(e);
            }
            self.collect_locked(&mut h, current, w.head.seq);
        }
        Ok(self.history())
    }

    /// Retained generations, readable commits, retention and cache counters.
    pub fn history(&self) -> crate::history::HistoryStatus {
        use crate::history::{HistoryGeneration, HistoryStatus, Hold};
        let head = self.head_commit().seq;
        let current = commit::generation_number(&self.snapshot().generation.name);
        let Some(hist) = &self.history else {
            return HistoryStatus {
                head,
                reconstructable: vec![(head, head)],
                generations: Vec::new(),
                bytes: 0,
                retention: Default::default(),
                snapshots: 0,
                cache_entries: 0,
                cache_bytes: 0,
                hits: 0,
                misses: 0,
                materializations: 0,
            };
        };
        let h = hist.lock();
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
            cache_entries: h.cache.len(),
            cache_bytes: h.cache.iter().map(|e| e.2).sum(),
            hits: h.hits,
            misses: h.misses,
            materializations: h.materializations,
        }
    }

    /// Write the quads of the state at `at` as N-Quads.
    pub fn dump_nquads_at(&self, at: &crate::history::At, w: impl Write) -> Result<u64> {
        let (snap, _) = self.snapshot_at(at, &Default::default())?;
        dump_snapshot(&snap.without_cache_fill(), w)
    }

    /// The latest commit.
    pub fn head_commit(&self) -> CommitInfo {
        self.writer.lock().head
    }

    /// Metadata of one commit, if it exists and is retained.
    pub fn commit(&self, seq: u64) -> Option<CommitInfo> {
        self.catalog.lock().get(seq)
    }

    /// A page of the commit catalog.
    pub fn commits(&self, range: CommitRange, limit: usize) -> CommitPage {
        self.catalog.lock().page(range, limit)
    }

    /// Replace the wall clock used for commit timestamps (tests).
    #[doc(hidden)]
    pub fn set_clock(&self, clock: Arc<dyn Fn() -> i64 + Send + Sync>) {
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
            match crate::text::TextIndex::open(Some(root), cfg, &snap, wal) {
                Ok((ti, view)) => {
                    self.text.store(Some(Arc::new(ti)));
                    let mut s = (*snap).clone();
                    s.text = Some(view);
                    self.current.store(Arc::new(s));
                }
                Err(e) => tracing::error!("full-text index of {}: {e}", root.display()),
            }
        }
        #[cfg(not(feature = "text"))]
        let _ = wal;
        #[cfg(not(feature = "text"))]
        if root.join("text.json").exists() {
            tracing::warn!(
                "{}: full-text search is configured but this build has no `text` feature",
                root.display()
            );
        }
        Ok(())
    }

    /// Apply a WAL commit's changes to the full-text index and give `snap` its view.
    fn maintain_text(&self, snap: &mut Snapshot, log: &[(u8, [Id; 4])]) {
        #[cfg(feature = "text")]
        if let Some(ti) = self.text.load_full() {
            let prev = snap.text.take();
            snap.text = ti.apply_commit(snap, log, prev.as_ref());
        }
        #[cfg(not(feature = "text"))]
        let _ = (snap, log);
    }

    /// After a bulk commit: rebuild the full-text index from the new snapshot (on failure
    /// the previous view stays, and text queries report the index as stale).
    fn rebuild_text_locked(&self, snap: &mut Snapshot, prev: Option<Arc<crate::text::TextView>>) {
        #[cfg(feature = "text")]
        if let Some(ti) = self.text.load_full() {
            snap.text = match ti.rebuild(snap) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::error!("full-text rebuild after a bulk commit failed: {e}");
                    prev
                }
            };
        }
        #[cfg(not(feature = "text"))]
        let _ = (snap, prev);
    }

    /// Enable (or reconfigure) full-text search and build the index from the current
    /// state. The configuration is kept in `text.json`.
    #[cfg(feature = "text")]
    pub fn enable_text(&self, cfg: crate::text::TextConfig) -> Result<crate::text::TextStatus> {
        let _w = self.writer.lock();
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

    /// Turn full-text search off and delete its index.
    #[cfg(feature = "text")]
    pub fn disable_text(&self) -> Result<()> {
        let _w = self.writer.lock();
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

    /// Rebuild the full-text index from the current state (writes wait meanwhile).
    #[cfg(feature = "text")]
    pub fn rebuild_text(&self) -> Result<crate::text::TextStatus> {
        let _w = self.writer.lock();
        let ti = self
            .text
            .load_full()
            .ok_or_else(|| Error::invalid("full-text search is not enabled"))?;
        let snap = self.snapshot();
        let view = ti.rebuild(&snap)?;
        let mut s = (*snap).clone();
        s.text = Some(view.clone());
        self.current.store(Arc::new(s));
        Ok(ti.status(Some(&view), snap.commit))
    }

    /// Full-text status (`None`: not enabled).
    #[cfg(feature = "text")]
    pub fn text_status(&self) -> Option<crate::text::TextStatus> {
        let ti = self.text.load_full()?;
        let snap = self.snapshot();
        Some(ti.status(snap.text.as_deref(), snap.commit))
    }

    /// Whether full-text search is enabled.
    pub fn text_enabled(&self) -> bool {
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

    /// Current read snapshot.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.current.load_full()
    }

    pub fn prefixes(&self) -> BTreeMap<String, String> {
        self.prefixes.lock().clone()
    }

    /// Add the prefixes of loaded data, keeping the ones already defined. Past
    /// [`StoreOptions::max_prefixes`], or with a name or IRI past its length limit, a
    /// prefix is left out (the data is not refused over its prefixes).
    pub fn add_prefixes(&self, p: BTreeMap<String, String>) -> Result<()> {
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
        let guard = self.writer.lock();
        self.begin(guard, kind, opts)
    }

    /// [`write_with`](Self::write_with), but a write whose `opts` are cancelled or past
    /// their deadline stops waiting for the writer lock (see
    /// [`WriteOptions::check`](crate::guard::WriteOptions::check)).
    pub fn try_write_with(
        &self,
        kind: CommitKind,
        opts: crate::guard::WriteOptions,
    ) -> Result<WriteTxn<'_>> {
        let guard = self.lock_writer(&opts)?;
        Ok(self.begin(guard, kind, opts))
    }

    /// The writer lock, waited for in slices while `o` can be cancelled or time out.
    fn lock_writer(&self, o: &crate::guard::WriteOptions) -> Result<MutexGuard<'_, WriterState>> {
        if o.cancel.is_none() && o.deadline.is_none() {
            return Ok(self.writer.lock());
        }
        loop {
            o.check()?;
            if let Some(w) = self
                .writer
                .try_lock_for(std::time::Duration::from_millis(20))
            {
                return Ok(w);
            }
        }
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
        let base = self.snapshot();
        WriteTxn {
            store: self,
            delta: base.delta.clone(),
            base,
            guard,
            log: Vec::new(),
            bulk: Vec::new(),
            kind,
            net_ins: 0,
            net_del: 0,
            opts,
        }
    }

    /// Install (or remove) the write guard run before every commit.
    pub fn set_guard(&self, g: Option<Arc<dyn crate::guard::CommitGuard>>) {
        *self.guard.write() = g;
    }

    pub fn guard(&self) -> Option<Arc<dyn crate::guard::CommitGuard>> {
        self.guard.read().clone()
    }

    /// Install (or remove) the observer told the outcome of every guard decision; it
    /// stays when the guard itself is replaced.
    pub fn set_guard_observer(&self, o: Option<Arc<dyn crate::guard::GuardObserver>>) {
        *self.guard_observer.write() = o;
    }

    /// The dataset's `validation.json` requires a write guard.
    pub fn guard_required(&self) -> bool {
        self.guard_required.load(Ordering::Relaxed)
    }

    /// Mark whether this dataset requires a guard (set with its configuration).
    pub fn set_guard_required(&self, required: bool) {
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
        use crate::guard::{GuardMode, GuardStatus, Severity, ValidationSummary};
        let g = self.guard();
        let observer = self.guard_observer.read().clone();
        if opts.bypass_validation {
            let summary = ValidationSummary::empty(
                GuardStatus::Bypassed,
                GuardMode::Off,
                Severity::Violation,
            );
            if let Some(g) = &g {
                g.bypassed();
            }
            if let Some(o) = &observer
                && (g.is_some() || self.guard_required())
            {
                o.observe(kind, Ok(&summary), std::time::Duration::ZERO);
            }
            tracing::warn!("a write bypassed write-time validation");
            return Ok(Some(Arc::new(summary)));
        }
        let Some(g) = g else {
            if self.guard_required() && !self.opts.unvalidated_writes {
                return Err(Error::GuardMissing(
                    "dataset requires write-time SHACL validation; install the guard (sparkles_shacl::guard::ShaclGuard::install) or allow unvalidated writes".into(),
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
        if let Some(o) = &observer {
            o.observe(kind, checked.as_ref(), t0.elapsed());
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
        if snap.is_empty() || estimated_quads(sources) > self.opts.bulk_threshold {
            let mut w = self.lock_writer(o)?;
            if w.poisoned {
                return Err(Error::Poisoned);
            }
            let snap = self.snapshot();
            let bulk = BulkCommit {
                kind,
                net_del: 0,
                start_len: snap.len(),
            };
            let check = Some((crate::guard::Changes::Unknown, o));
            Ok(self
                .rebuild_locked(&mut w, &snap, sources, &[], &[], Some(bulk), check)?
                .1)
        } else {
            let mut txn = self.try_write_with(kind, o.clone())?;
            let mut prefixes = BTreeMap::new();
            for s in sources {
                o.check()?;
                let (quads, p) = crate::io::parse_to_vec(s)?;
                self.check_memory_before(quads.len())?;
                prefixes.extend(p);
                let mut labels = std::collections::HashMap::new();
                for (i, q) in quads.iter().enumerate() {
                    if i % 65_536 == 65_535 {
                        o.check()?;
                    }
                    let ids = txn.encode_quad(q, &mut labels)?;
                    txn.insert(ids)?;
                }
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
        if estimated_quads(sources) > self.opts.bulk_threshold {
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
        for g in graphs {
            for (i, k) in view.scan_keys(Perm::Gspo, &[g.0])?.into_iter().enumerate() {
                if i % 65_536 == 65_535 {
                    o.check()?;
                }
                txn.delete(Perm::Gspo.to_quad(&k))?;
            }
        }
        let mut ids = Vec::new();
        for quads in &parsed {
            o.check()?;
            let mut labels = std::collections::HashMap::new();
            for q in quads {
                ids.push(txn.encode_quad(q, &mut labels)?);
            }
        }
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
        };
        let check = Some((crate::guard::Changes::Unknown, o));
        let (_, r) =
            self.rebuild_locked(&mut w, &snap, sources, &[], &graphs, Some(bulk), check)?;
        // the quads the replacement holds: what is left, less what was kept
        Ok(((r.commit.quads + dropped).saturating_sub(start_len), r))
    }

    /// Compact: merge base ⊕ delta into a new generation. The data does not change, so
    /// neither does the head commit.
    pub fn compact(&self) -> Result<()> {
        let mut w = self.writer.lock();
        if w.poisoned {
            return Err(Error::Poisoned);
        }
        let snap = self.snapshot();
        self.rebuild_locked(&mut w, &snap, &[], &[], &[], None, None)?;
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
        let before = snap.len();
        // the old generation's WAL is the only other copy of the recent commits' ids:
        // the catalog must be durable before it is discarded
        self.catalog.lock().sync()?;
        // and so must the full-text index, which could otherwise only catch up from it
        #[cfg(feature = "text")]
        if let Some(ti) = self.text.load_full() {
            ti.checkpoint()?;
        }
        let (dir, name, tmp) = match &self.root {
            Some(root) => {
                let n: u32 = snap
                    .generation
                    .name
                    .strip_prefix("gen-")
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(0)
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
            if bulk.is_some() {
                self.check_memory(dir_size(&dir))?;
            }
            Ok(meta)
        })();
        let meta = match built {
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
        w.next_bnode = w.next_bnode.max(meta.next_bnode);
        // a bulk commit is validated on the built generation, before anything is published
        let validation = match (&bulk, check) {
            (Some(b), Some((changes, o))) => {
                let candidate = || {
                    Arc::new(Snapshot {
                        generation: gen_.clone(),
                        delta: Delta::default(),
                        version: 0,
                        cache: self.cache.clone(),
                        results: Arc::new(crate::sparql::cache::ResultCache::new(0, 0.0)),
                        dvocab_len: gen_.dvocab.len(),
                        commit: w.head.seq,
                        text: None,
                        geo: None,
                        union_default_graph: self.opts.union_default_graph,
                        geo_op_vertices: self.opts.geo_op_vertices,
                        delta_stats: Default::default(),
                        historical: false,
                    })
                };
                match self.run_guard(snap, candidate, b.kind, changes, o, w.head.seq) {
                    Ok(v) => v,
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
            },
            None => w.head,
        };
        if let Some(root) = &self.root {
            // Publication order: the new generation's files (with the commit its base
            // holds) and directory entries are durable, its WAL file exists durably, then
            // CURRENT switches durably, and only after that is the old generation removed.
            let origin = if bulk.is_some() { "bulk" } else { "compaction" };
            write_synced(
                &dir.join("commit.json"),
                &commit::gen_commit_bytes(self.dataset_id, origin, &head),
            )?;
            let wal = OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("wal.log"))?;
            sync_dir(&dir)?;
            sync_dir(root)?;
            write_atomic(&root.join("CURRENT"), name.as_bytes())?;
            w.wal = Some(BufWriter::new(wal));
        }
        // the switch is the commit point: a failure after it leaves the published state
        // behind the durable one, so later writes are refused
        if bulk.is_some() {
            w.head = head;
            self.catalog.lock().append(head);
        }
        if let Err(e) = self.add_prefixes(meta.prefixes.clone()) {
            w.poisoned = true;
            return Err(e);
        }
        let old = snap.generation.dir.clone();
        let old_name = snap.generation.name.clone();
        let dvocab_len = gen_.dvocab.len();
        let mut new_snap = Snapshot {
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
            historical: false,
        };
        if bulk.is_some() {
            self.rebuild_text_locked(&mut new_snap, snap.text.clone());
        }
        // a new generation needs its own spatial base (bulk commits and compactions)
        self.rebuild_geo_locked(&mut new_snap, snap);
        self.current.store(Arc::new(new_snap));
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
        let receipt = Receipt {
            dataset_id: self.dataset_id,
            committed: bulk.is_some(),
            commit: head,
            validation,
        };
        Ok((meta.quads.saturating_sub(before), receipt))
    }

    /// Build a new, independent database in `dir` (absent or empty) from one consistent
    /// snapshot of this store: every quad (except those in `opts.exclude_graphs`) in a
    /// freshly built generation `gen-0001` with an empty delta, the prefixes, and a new
    /// dataset id whose root commit records this store's id and the snapshot's commit as
    /// `forkedFrom`. Blank nodes keep their ids (`_:b<hex>` labels), and the blank-node
    /// counter is carried over, so new blank nodes never collide with copied ones.
    ///
    /// The writer lock is held only to capture the snapshot; this store is never written.
    /// On any error `dir` is left as it was found (removed, or emptied).
    pub fn clone_to(&self, dir: &Path, opts: &CloneOptions) -> Result<CloneReport> {
        let t0 = std::time::Instant::now();
        let existed = dir.exists();
        if existed && std::fs::read_dir(dir)?.next().is_some() {
            return Err(Error::Invalid(format!(
                "{} exists and is not empty",
                dir.display()
            )));
        }
        // the snapshot and the blank-node counter together: no commit falls in between
        let (snap, next_bnode) = {
            let w = self.writer.lock();
            (self.snapshot(), w.next_bnode)
        };
        std::fs::create_dir_all(dir)?;
        let mut guard = CleanDir {
            dir,
            remove: !existed,
            armed: true,
        };
        let excluded: std::collections::HashSet<u64> = opts
            .exclude_graphs
            .iter()
            .filter_map(|g| snap.lookup_iri(g.as_str()))
            .map(|g| g.0)
            .collect();
        let report = |f: f32, msg: &str| {
            if let Some(p) = &opts.progress {
                p(f, msg);
            }
        };
        let name = "gen-0001";
        let gdir = dir.join(name);
        let mut bopts = self.opts.build.clone();
        bopts.first_bnode = next_bnode;
        let mut builder = Builder::new(&gdir, bopts)?;
        // the clone's file system keeps the same free space as this store's
        if let Some(reserve) = self.opts.min_free_disk_bytes {
            let gdir = gdir.clone();
            builder = builder.with_interrupt(Arc::new(move || {
                crate::disk::check_reserve(&gdir, reserve, 0, false)
            }));
        }
        let total = snap.len().max(1);
        let (mut seen, mut graphs, mut last_graph) = (0u64, 0u64, None);
        report(0.0, "copying quads");
        write_snapshot(
            &builder,
            &snap,
            |q| {
                seen += 1;
                if seen % 65_536 == 0 {
                    if opts
                        .cancel
                        .as_ref()
                        .is_some_and(|c| c.load(Ordering::Relaxed))
                    {
                        return Err(Error::Cancelled);
                    }
                    report(0.7 * seen as f32 / total as f32, "copying quads");
                }
                if excluded.contains(&q[3].0) {
                    return Ok(false);
                }
                if last_graph != Some(q[3]) {
                    graphs += 1;
                    last_graph = Some(q[3]);
                }
                Ok(true)
            },
            &[],
        )?;
        report(0.7, "building indexes");
        let prefixes = self.prefixes();
        builder.add_prefixes(prefixes.clone());
        let meta = builder.finish()?;
        // a new lineage: its own id and root commit, forked from the snapshot
        let id = uuid::Uuid::new_v4();
        let now = commit::now_ms();
        let root = CommitInfo {
            seq: 0,
            timestamp_ms: now,
            kind: CommitKind::Create,
            inserted: meta.quads,
            deleted: 0,
            quads: meta.quads,
            generation: 1,
            bulk: true,
            exact: true,
            reconstructed: false,
        };
        let forked_from = ForkedFrom {
            id: self.dataset_id,
            seq: snap.commit,
        };
        write_synced(
            &gdir.join("commit.json"),
            &commit::gen_commit_bytes(id, "clone", &root),
        )?;
        File::create(gdir.join("wal.log"))?.sync_all()?;
        sync_dir(&gdir)?;
        write_atomic(
            &dir.join("dataset.json"),
            &commit::clone_dataset_file_bytes(id, now, forked_from),
        )?;
        Catalog::create(&dir.join("commits.bin"), id, root)?;
        if !prefixes.is_empty() {
            write_atomic(
                &dir.join("prefixes.json"),
                &serde_json::to_vec_pretty(&prefixes).unwrap(),
            )?;
        }
        // full-text search stays on: the clone rebuilds its index when opened
        #[cfg(feature = "text")]
        let text_cfg = self
            .text
            .load()
            .as_ref()
            .map(|ti| serde_json::to_vec_pretty(ti.config()).unwrap());
        #[cfg(not(feature = "text"))]
        let text_cfg = match &self.root {
            Some(root) => match std::fs::read(root.join("text.json")) {
                Ok(b) => Some(b),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            },
            None => None,
        };
        if let Some(cfg) = text_cfg {
            write_atomic(&dir.join("text.json"), &cfg)?;
        }
        // so does the spatial index
        let geo_cfg = match &self.root {
            Some(root) => match std::fs::read(root.join(crate::geo::CONFIG_FILE)) {
                Ok(b) => Some(b),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            },
            None => self
                .geo_status()
                .map(|s| serde_json::to_vec_pretty(&s.config).unwrap()),
        };
        if let Some(cfg) = geo_cfg {
            write_atomic(&dir.join(crate::geo::CONFIG_FILE), &cfg)?;
        }
        // write-time validation stays configured; the clone judges its first write in full
        if let Some(root) = &self.root {
            for f in ["validation.json", "validation-shapes.ttl"] {
                match std::fs::read(root.join(f)) {
                    Ok(b) => write_atomic(&dir.join(f), &b)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        // CURRENT last: the commit point of the new database
        write_atomic(&dir.join("CURRENT"), name.as_bytes())?;
        sync_dir(dir)?;
        guard.armed = false;
        report(1.0, "done");
        Ok(CloneReport {
            dataset_id: id,
            forked_from,
            version: snap.version,
            generation: snap.generation.name.clone(),
            source_quads: snap.len(),
            quads: meta.quads,
            graphs,
            millis: t0.elapsed().as_millis() as u64,
        })
    }

    /// `forkedFrom` of a database made by [`clone_to`](Self::clone_to) (or restored
    /// from a backup under a new identity).
    pub fn forked_from(&self) -> Option<ForkedFrom> {
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

    pub fn disk_bytes(&self) -> u64 {
        match &self.root {
            Some(r) => dir_size(r),
            None => 0,
        }
    }

    /// Size of the current generation's write-ahead log (0 for an in-memory store): one
    /// `stat`, without the writer lock, so buffered records may not be counted yet.
    pub fn wal_bytes(&self) -> u64 {
        let Some(root) = &self.root else { return 0 };
        let wal = root.join(&self.snapshot().generation.name).join("wal.log");
        std::fs::metadata(wal).map_or(0, |m| m.len())
    }
}

/// Push the quads of `snap` for which `keep` returns true, then `extra` (store ids of
/// `snap`), into `builder`. Vocabulary ids are passed as their keys; inline and
/// blank-node ids pass through unchanged, which keeps blank-node identity. Triple-term
/// keys embed their blank-node payloads, so blank nodes inside them keep it too.
fn write_snapshot(
    builder: &Builder,
    snap: &Snapshot,
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
    snap.for_each_quad(|q| if keep(q)? { push(q) } else { Ok(()) })?;
    for q in extra {
        push(q)?;
    }
    enc.flush()
}

/// What [`Store::clone_to`] leaves out, and how it reports progress.
#[derive(Clone, Default)]
pub struct CloneOptions {
    /// graphs to leave out (e.g. the materialized inferences)
    pub exclude_graphs: Vec<NamedNode>,
    /// set to `true` to cancel (checked every 65536 quads)
    pub cancel: Option<Arc<AtomicBool>>,
    pub progress: Option<ProgressFn>,
}

/// Progress callback: (fraction done in `[0, 1]`, message).
pub type ProgressFn = Arc<dyn Fn(f32, &str) + Send + Sync>;

/// Outcome of [`Store::clone_to`].
#[derive(Clone, Debug)]
pub struct CloneReport {
    /// the new database's dataset id
    pub dataset_id: uuid::Uuid,
    /// this store's id and the commit of the copied snapshot
    pub forked_from: ForkedFrom,
    /// snapshot version and generation of the source
    pub version: u64,
    pub generation: String,
    /// quads in the source snapshot
    pub source_quads: u64,
    /// quads in the clone (fewer when graphs were excluded)
    pub quads: u64,
    /// graphs in the clone, the default graph included when it has quads
    pub graphs: u64,
    pub millis: u64,
}

/// Removes (or empties) a directory on drop unless disarmed.
struct CleanDir<'a> {
    dir: &'a Path,
    /// remove the directory itself (it did not exist before)
    remove: bool,
    armed: bool,
}

impl Drop for CleanDir<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.remove {
            let _ = std::fs::remove_dir_all(self.dir);
        } else if let Ok(rd) = std::fs::read_dir(self.dir) {
            for e in rd.flatten() {
                let p = e.path();
                let _ = if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
            }
        }
    }
}

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
        if insert {
            if delta.del[i].remove(&k).is_none() && !in_base {
                delta.ins[i].insert(k);
            }
        } else if delta.ins[i].remove(&k).is_none() && in_base {
            delta.del[i].insert(k);
        }
    }
}

/// The single write transaction. Changes are invisible until [`commit`](Self::commit).
pub struct WriteTxn<'s> {
    store: &'s Store,
    base: Arc<Snapshot>,
    delta: Delta,
    guard: MutexGuard<'s, WriterState>,
    log: Vec<(u8, [Id; 4])>,
    /// large insert batches applied by rebuilding the generation on commit
    bulk: Vec<[Id; 4]>,
    kind: CommitKind,
    /// net quads added / removed relative to `base` (the commit's counts)
    net_ins: u64,
    net_del: u64,
    /// options for the write guard
    opts: crate::guard::WriteOptions,
}

impl WriteTxn<'_> {
    /// Snapshot view including this transaction's uncommitted changes. It keeps the
    /// committed version number but not its result cache: the data differs from that
    /// version, so cached results must neither be read nor written through this view.
    pub fn view(&self) -> Snapshot {
        Snapshot {
            generation: self.base.generation.clone(),
            delta: self.delta.clone(),
            version: self.base.version,
            cache: self.base.cache.clone(),
            results: Arc::new(crate::sparql::cache::ResultCache::new(0, 0.0)),
            dvocab_len: self.base.generation.dvocab.len(),
            commit: self.base.commit,
            text: self.base.text.clone(),
            // the index does not cover this transaction's changes: plans without it
            geo: self.base.geo.as_ref().map(|v| Arc::new(v.for_txn())),
            union_default_graph: self.base.union_default_graph,
            geo_op_vertices: self.base.geo_op_vertices,
            delta_stats: Default::default(),
            historical: false,
        }
    }

    pub fn base(&self) -> &Arc<Snapshot> {
        &self.base
    }

    /// Get or create the id for a term (new terms go to the delta vocabulary).
    pub fn intern(&mut self, t: &Term) -> Result<Id> {
        if let Some(id) = id::inline_id(t) {
            return Ok(id);
        }
        if let Term::BlankNode(b) = t
            && let Some(id) = parse_bnode_label(b.as_str())
        {
            return Ok(id);
        }
        self.intern_key(&id::term_key(t))
    }

    /// Intern a term whose blank nodes (including those inside RDF 1.2 triple terms) are
    /// scoped by `labels`: unseen labels get fresh blank node ids.
    pub fn intern_scoped(
        &mut self,
        t: &Term,
        labels: &mut std::collections::HashMap<String, Id>,
    ) -> Result<Id> {
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

    pub fn intern_key(&mut self, key: &[u8]) -> Result<Id> {
        if let Ok(i) = self.base.generation.vocab.find(key) {
            return Ok(Id::vocab(i));
        }
        Ok(Id::delta(self.base.generation.dvocab.insert(key)?))
    }

    pub fn new_bnode(&mut self) -> Id {
        let id = self.guard.next_bnode;
        self.guard.next_bnode += 1;
        Id::bnode(id)
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

    /// Whether the quad was present in the committed snapshot this transaction started
    /// from (`in_base`: present in the base index).
    fn present_at_start(&self, q: &[Id; 4], in_base: bool) -> bool {
        let k = Perm::Spo.to_key(q);
        let i = Perm::Spo.index();
        self.base.delta.ins[i].contains(&k) || (in_base && !self.base.delta.del[i].contains(&k))
    }

    /// Insert a quad; returns true if it was not present.
    pub fn insert(&mut self, q: [Id; 4]) -> Result<bool> {
        if q.iter()
            .any(|id| matches!(id.tag(), Tag::Local | Tag::Undef))
        {
            return Err(Error::invalid("cannot store query-local or unbound terms"));
        }
        if self.contains(&q)? {
            return Ok(false);
        }
        let ib = self.in_base(&q)?;
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
        if !self.contains(&q)? {
            return Ok(false);
        }
        let ib = self.in_base(&q)?;
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
        if (quads.len() as u64) < self.store.opts.bulk_threshold {
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
        if self.guard.poisoned {
            return Err(Error::Poisoned);
        }
        // a write cancelled (its client gone) or past its deadline publishes nothing
        self.opts.check()?;
        if self.bulk.is_empty() {
            if self.net_ins == 0 && self.net_del == 0 {
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
        if self.net_ins == 0 && self.net_del == 0 {
            // nothing changed (or every change was undone): no commit, nothing published
            return Ok(Receipt {
                dataset_id: self.store.dataset_id,
                committed: false,
                commit: head,
                validation: None,
            });
        }
        // nothing is written when the disk (or an in-memory store's limit) has no room
        self.store
            .check_disk((self.log.len() as u64 + 1) * WAL_REC as u64)?;
        if self.net_ins > 0
            && self.store.root.is_none()
            && self.store.opts.max_memory_bytes.is_some()
        {
            let size = gen_.disk_bytes()
                + delta_bytes(&self.delta)
                + gen_.dvocab.with(|v| v.bytes()) as u64;
            self.store.check_memory(size)?;
        }
        gen_.dvocab.sync()?;
        let c = CommitInfo {
            seq: head.seq + 1,
            timestamp_ms: self.store.commit_time(&head),
            kind: self.kind,
            inserted: self.net_ins,
            deleted: self.net_del,
            quads: (head.quads + self.net_ins).saturating_sub(self.net_del),
            generation: commit::generation_number(&gen_.name),
            bulk: false,
            exact: true,
            reconstructed: false,
        };
        let next_bnode = self.guard.next_bnode;
        if let Some(wal) = self.guard.wal.as_mut() {
            let mut data = Vec::with_capacity(self.log.len() * WAL_REC);
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
            commit::seal_wal_commit(&mut rec, c.seq, c.timestamp_ms, c.kind, &data);
            data.extend_from_slice(&rec);
            // once the first byte is written, a failure leaves the WAL in an unknown
            // state: refuse further writes, so a seq can never be written twice
            let written = wal
                .write_all(&data)
                .and_then(|_| wal.flush())
                .and_then(|_| wal.get_ref().sync_data());
            if let Err(e) = written {
                self.guard.poisoned = true;
                return Err(e.into());
            }
        }
        self.guard.head = c;
        self.store.catalog.lock().append(c);
        let version = self.base.version + 1;
        let mut snap = Snapshot {
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
            historical: false,
        };
        self.store.maintain_text(&mut snap, &self.log);
        self.store.maintain_geo(&mut snap, &self.log);
        self.store.current.store(Arc::new(snap));
        if validation.is_some() {
            self.store.guard_committed(c.seq);
        }
        Ok(Receipt {
            dataset_id: self.store.dataset_id,
            committed: true,
            commit: c,
            validation,
        })
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
    match std::fs::read(root.join("validation.json")) {
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

/// Remove the generations history no longer needs (`current` is the current generation
/// and `head` the latest commit); call with the writer lock held.
fn collect_generations(
    root: &Path,
    now_ms: i64,
    cat: &Catalog,
    h: &mut crate::history::HistoryState,
    current: u32,
    head: u64,
    max_gens: usize,
) {
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
    for (no, dir) in doomed {
        h.open.retain(|(n, _)| *n != no);
        h.cache.retain(|(k, _, _)| k.0 != no);
        match crate::history::delete_generation(root, &dir) {
            Ok(()) => {
                h.gens.remove(&no);
            }
            Err(e) => tracing::warn!("could not remove {}: {e}", dir.display()),
        }
    }
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
        let Some(w) = writer.try_lock() else { return };
        if w.closed {
            return;
        }
        let now = match &*clock.lock() {
            Some(c) => c(),
            None => commit::now_ms(),
        };
        let cur = commit::generation_number(&current.load().generation.name);
        // lock order: writer, history, catalog
        let mut h = history.lock();
        collect_generations(
            &self.root,
            now,
            &catalog.lock(),
            &mut h,
            cur,
            w.head.seq,
            self.max_gens,
        );
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // a lease guard outliving the store must not collect in a directory that another
        // process (or a restore's swap) may own next
        self.writer.lock().closed = true;
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
    let (pins, retention) = history::read_file(root, dataset_id)?;
    let mut h = history::HistoryState::new(pins, retention);
    for (no, name, base, fold_legacy) in history::scan_generations(root, dataset_id)? {
        let dir = root.join(&name);
        if no > current {
            if let Err(e) = history::delete_generation(root, &dir) {
                tracing::warn!("could not remove the interrupted rebuild {name}: {e}");
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
                    Some(Ok((seq, _, _))) => {
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

/// Where replay stops.
pub(crate) enum Stop {
    /// at the end of the log (a torn final transaction is left out)
    End,
    /// right after the commit with this sequence number
    AfterSeq(u64),
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
    /// `Stop::AfterSeq` found its commit
    pub reached: bool,
}

/// Replay the records of a generation's WAL (`buf`) onto its base index. A checksum
/// mismatch in the final transaction is a torn tail and ends the replay; one before it
/// is [`Error::Corrupt`]. `check` runs every 64 Ki records with the delta so far (for
/// cancellation and memory budgets). The same inputs always give the same commit
/// numbers, whether the replay is for opening the store or for reading a past state.
pub(crate) fn replay_wal(
    from: &ReplayFrom<'_>,
    buf: &[u8],
    stop: Stop,
    check: &mut dyn FnMut(u64, &Delta) -> Result<()>,
) -> Result<Replay> {
    let cache = from.cache;
    let recs = buf.as_chunks::<WAL_REC>().0;
    // the last complete transaction may be torn; damage before it is corruption
    let last_commit = recs.iter().rposition(|r| r[0] == WAL_COMMIT);
    let mut out = Replay {
        delta: Delta::default(),
        version: 0,
        commits: Vec::new(),
        base_quads: from.generation.meta.quads,
        next_bnode: from.next_bnode,
        good: 0,
        touched: Vec::new(),
        reached: false,
    };
    let mut pending: Vec<(u8, [Id; 4])> = Vec::new();
    let mut txn_start = 0usize;
    let mut start_delta = out.delta.clone();
    let probe = Snapshot {
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
        historical: false,
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
                let (mut ins, mut del) = (0i64, 0i64);
                let touched: Vec<[Id; 4]> = if from.keep_touched {
                    pending.iter().map(|(_, q)| *q).collect()
                } else {
                    Vec::new()
                };
                let before = out.commits.len();
                for (op, q) in pending.drain(..) {
                    let k = Perm::Spo.to_key(&q);
                    let in_base = probe.perm(Perm::Spo).contains(cache, &k)?;
                    let spo = Perm::Spo.index();
                    let present = start_delta.ins[spo].contains(&k)
                        || (in_base && !start_delta.del[spo].contains(&k));
                    match (op == WAL_INSERT, present) {
                        (true, false) => ins += 1,
                        (true, true) => del -= 1,
                        (false, true) => del += 1,
                        (false, false) => ins -= 1,
                    }
                    apply(&mut out.delta, &q, op == WAL_INSERT, in_base);
                }
                start_delta = out.delta.clone();
                let (ins, del) = (ins.max(0) as u64, del.max(0) as u64);
                quads = (quads + ins).saturating_sub(del);
                out.next_bnode = out.next_bnode.max(q[0].0);
                out.version += 1;
                let prev = out.commits.last().copied().unwrap_or(from.base);
                match meta {
                    Some(Ok((seq, ts, kind))) => {
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
                    }),
                }
                if from.keep_touched && out.commits.len() > before {
                    out.touched.push((out.commits.last().unwrap().seq, touched));
                }
                out.good = (i + 1) * WAL_REC;
                txn_start = i + 1;
                if let Stop::AfterSeq(s) = stop
                    && out.commits.last().is_some_and(|c| c.seq >= s)
                {
                    out.reached = true;
                    break;
                }
            }
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

    const TTL: &str = r#"
@prefix ex: <http://ex.org/> .
ex:a ex:p 1, 2, 3 . ex:b ex:p 2 . ex:c ex:q "hello"@en .
"#;

    fn src() -> Source {
        Source::from_bytes(TTL.as_bytes().to_vec(), RdfFormat::Turtle, None)
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
