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

use crate::builder::{BuildOptions, Builder, IndexMeta, Slot, Stats};
use crate::commit::{self, Catalog, CommitInfo, CommitKind, CommitPage, CommitRange, Receipt};
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

/// Immutable base index generation.
pub struct Generation {
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
}

impl Generation {
    fn empty(dvocab: DeltaVocab) -> Generation {
        Generation {
            name: "mem".into(),
            dir: None,
            vocab: Vocab::empty(),
            perms: Perm::ALL.iter().map(|&p| PermIndex::empty(p)).collect(),
            stats: Stats::default(),
            meta: IndexMeta::default(),
            dvocab,
            _tmp: None,
            vectors: Default::default(),
        }
    }

    fn open(dir: &Path, name: &str, persistent: bool) -> Result<Generation> {
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
        let dvocab = if persistent {
            DeltaVocab::open(&dir.join("delta.vocab"))?
        } else {
            DeltaVocab::in_memory()
        };
        Ok(Generation {
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
    pub union_default_graph: bool,
    /// per-predicate statistics of the delta (computed lazily, once per snapshot)
    pub delta_stats:
        Arc<std::sync::OnceLock<rustc_hash::FxHashMap<u64, crate::builder::PredicateStat>>>,
}

/// A contiguous run of rows produced by a scan.
pub enum Chunk<'a> {
    /// rows `[start, end)` of a base block (no delta changes in between)
    Block(&'a Block, usize, usize),
    /// a single row from a merge with the delta
    Row(Key),
}

impl Snapshot {
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
}

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
    current: ArcSwap<Snapshot>,
    writer: Mutex<WriterState>,
    cache: Arc<BlockCache>,
    results: Arc<crate::sparql::cache::ResultCache>,
    prefixes: Mutex<BTreeMap<String, String>>,
    /// exclusive OS lock on `<root>/sparkles.lock` (TDB2 `tdb.lock`), held while open
    _lock: Option<File>,
    dataset_id: uuid::Uuid,
    catalog: Mutex<Catalog>,
    /// test hook replacing the wall clock (milliseconds since the epoch)
    clock: Mutex<Option<Clock>>,
    /// full-text index, when enabled for this dataset
    text: arc_swap::ArcSwapOption<crate::text::TextIndex>,
}

const WAL_INSERT: u8 = 1;

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
const WAL_DELETE: u8 = 2;
const WAL_COMMIT: u8 = 3;
const WAL_REC: usize = 1 + 32;

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
        Store {
            root: None,
            current: ArcSwap::from_pointee(Snapshot {
                generation: gen_,
                delta: Delta::default(),
                version: 0,
                cache: cache.clone(),
                results: results.clone(),
                dvocab_len: 0,
                commit: 0,
                text: None,
                union_default_graph: opts.union_default_graph,
                delta_stats: Default::default(),
            }),
            writer: Mutex::new(WriterState {
                wal: None,
                next_bnode: 0,
                head: root,
                poisoned: false,
            }),
            cache,
            results,
            prefixes: Mutex::new(BTreeMap::new()),
            _lock: None,
            dataset_id,
            catalog: Mutex::new(Catalog::memory(root, opts.memory_commit_ring)),
            clock: Mutex::new(None),
            text: Default::default(),
            opts,
        }
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
        let mut prefixes = gen_.meta.prefixes.clone();
        if let Ok(p) = std::fs::read(root.join("prefixes.json"))
            && let Ok(p) = serde_json::from_slice::<BTreeMap<String, String>>(&p)
        {
            prefixes.extend(p);
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
        // quads of the base plus WAL transactions folded into a baseline commit
        let mut base_quads = gen_.meta.quads;
        if wal_path.exists() {
            let mut buf = Vec::new();
            File::open(&wal_path)?.read_to_end(&mut buf)?;
            let recs = buf.as_chunks::<WAL_REC>().0;
            // the last complete transaction may be torn; damage before it is corruption
            let last_commit = recs.iter().rposition(|r| r[0] == WAL_COMMIT);
            let mut pending: Vec<(u8, [Id; 4])> = Vec::new();
            let mut txn_start = 0usize;
            let mut good = 0usize;
            let mut start_delta = delta.clone();
            let probe = Snapshot {
                generation: gen_.clone(),
                delta: Delta::default(),
                version: 0,
                cache: cache.clone(),
                results: results.clone(),
                dvocab_len: u64::MAX,
                commit: 0,
                text: None,
                union_default_graph: false,
                delta_stats: Default::default(),
            };
            let mut quads = base_quads;
            let mut seen_v2 = false;
            for (i, rec) in recs.iter().enumerate() {
                let q: [Id; 4] = std::array::from_fn(|j| {
                    Id(u64::from_le_bytes(
                        rec[1 + j * 8..9 + j * 8].try_into().unwrap(),
                    ))
                });
                match rec[0] {
                    WAL_INSERT | WAL_DELETE => pending.push((rec[0], q)),
                    WAL_COMMIT => {
                        let meta =
                            commit::open_wal_commit(rec, &buf[txn_start * WAL_REC..i * WAL_REC]);
                        if matches!(meta, Some(Err(()))) {
                            if Some(i) == last_commit {
                                break; // torn tail: truncated below
                            }
                            return Err(Error::Corrupt(format!(
                                "{}: checksum mismatch in the transaction ending at byte {}",
                                wal_path.display(),
                                (i + 1) * WAL_REC
                            )));
                        }
                        let (mut ins, mut del) = (0i64, 0i64);
                        for (op, q) in pending.drain(..) {
                            let k = Perm::Spo.to_key(&q);
                            let in_base = probe.perm(Perm::Spo).contains(&cache, &k)?;
                            let spo = Perm::Spo.index();
                            let present = start_delta.ins[spo].contains(&k)
                                || (in_base && !start_delta.del[spo].contains(&k));
                            match (op == WAL_INSERT, present) {
                                (true, false) => ins += 1,
                                (true, true) => del -= 1,
                                (false, true) => del += 1,
                                (false, false) => ins -= 1,
                            }
                            apply(&mut delta, &q, op == WAL_INSERT, in_base);
                        }
                        start_delta = delta.clone();
                        let (ins, del) = (ins.max(0) as u64, del.max(0) as u64);
                        quads = (quads + ins).saturating_sub(del);
                        next_bnode = next_bnode.max(q[0].0);
                        version += 1;
                        let prev = replayed.last().copied().unwrap_or(base);
                        match meta {
                            Some(Ok((seq, ts, kind))) => {
                                seen_v2 = true;
                                if seq != prev.seq + 1 {
                                    return Err(Error::Corrupt(format!(
                                        "{}: commit {seq} follows commit {}",
                                        wal_path.display(),
                                        prev.seq
                                    )));
                                }
                                replayed.push(CommitInfo {
                                    seq,
                                    timestamp_ms: ts,
                                    kind,
                                    inserted: ins,
                                    deleted: del,
                                    quads,
                                    generation: gen_no,
                                    bulk: false,
                                    exact: true,
                                    reconstructed: false,
                                });
                            }
                            // a legacy commit record: folded into the baseline when the
                            // database is being upgraded, otherwise numbered in order
                            _ if fold_legacy && !seen_v2 => base_quads = quads,
                            _ => replayed.push(CommitInfo {
                                seq: prev.seq + 1,
                                timestamp_ms: prev.timestamp_ms,
                                kind: CommitKind::Unknown,
                                inserted: ins,
                                deleted: del,
                                quads,
                                generation: gen_no,
                                bulk: false,
                                exact: true,
                                reconstructed: true,
                            }),
                        }
                        good = (i + 1) * WAL_REC;
                        txn_start = i + 1;
                    }
                    _ => break,
                }
            }
            if good != buf.len() {
                OpenOptions::new()
                    .write(true)
                    .open(&wal_path)?
                    .set_len(good as u64)?;
            }
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
        let store = Store {
            root: Some(root.to_path_buf()),
            current: ArcSwap::from_pointee(Snapshot {
                generation: gen_,
                delta,
                version,
                cache: cache.clone(),
                results: results.clone(),
                dvocab_len,
                commit: head.seq,
                text: None,
                union_default_graph: opts.union_default_graph,
                delta_stats: Default::default(),
            }),
            writer: Mutex::new(WriterState {
                wal: Some(BufWriter::new(wal)),
                next_bnode,
                head,
                poisoned: false,
            }),
            cache,
            results,
            prefixes: Mutex::new(prefixes),
            _lock: Some(lock),
            dataset_id,
            catalog: Mutex::new(catalog),
            clock: Mutex::new(None),
            text: Default::default(),
            opts,
        };
        store.open_text()?;
        Ok(store)
    }

    /// The dataset id (a UUID created with the database).
    pub fn dataset_id(&self) -> uuid::Uuid {
        self.dataset_id
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
    fn open_text(&self) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        #[cfg(feature = "text")]
        {
            let Some(cfg) = crate::text::imp_read_config(root)? else {
                return Ok(());
            };
            let snap = self.snapshot();
            match crate::text::TextIndex::open(Some(root), cfg, &snap) {
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
            let touched: Vec<[Id; 4]> = log.iter().map(|(_, q)| *q).collect();
            let prev = snap.text.take();
            snap.text = ti.apply_commit(snap, &touched, prev.as_ref());
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
        let (ti, view) = crate::text::TextIndex::open(self.root.as_deref(), cfg, &snap)?;
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
            ti.fail_next_commit
                .store(true, std::sync::atomic::Ordering::SeqCst);
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

    pub fn add_prefixes(&self, p: BTreeMap<String, String>) -> Result<()> {
        if p.is_empty() {
            return Ok(());
        }
        let mut cur = self.prefixes.lock();
        let before = cur.len();
        for (k, v) in p {
            cur.entry(k).or_insert(v);
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

    /// Begin the (single) write transaction; blocks while another writer is active.
    pub fn write(&self) -> WriteTxn<'_> {
        self.write_as(CommitKind::Transaction)
    }

    /// Begin the write transaction, recording its commit as `kind`.
    pub fn write_as(&self, kind: CommitKind) -> WriteTxn<'_> {
        let guard = self.writer.lock();
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
        let snap = self.snapshot();
        let mut size_hint: u64 = 0;
        for s in sources {
            size_hint += match &s.data {
                crate::io::SourceData::Bytes(b) => b.len() as u64,
                crate::io::SourceData::File(p) => {
                    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
                }
            } * if s.gzip { 8 } else { 1 };
        }
        // ~80 bytes per quad in text formats
        let est_quads = size_hint / 80;
        if snap.is_empty() || est_quads > self.opts.bulk_threshold {
            let mut w = self.writer.lock();
            if w.poisoned {
                return Err(Error::Poisoned);
            }
            let snap = self.snapshot();
            let bulk = BulkCommit {
                kind,
                net_del: 0,
                start_len: snap.len(),
            };
            Ok(self
                .rebuild_locked(&mut w, &snap, sources, &[], Some(bulk))?
                .1)
        } else {
            let mut txn = self.write_as(kind);
            let mut prefixes = BTreeMap::new();
            for s in sources {
                let (quads, p) = crate::io::parse_to_vec(s)?;
                prefixes.extend(p);
                let mut labels = std::collections::HashMap::new();
                for q in &quads {
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
        let mut parsed = Vec::with_capacity(sources.len());
        let mut prefixes = BTreeMap::new();
        for s in sources {
            let (quads, p) = crate::io::parse_to_vec(s)?;
            prefixes.extend(p);
            parsed.push(quads);
        }
        let mut txn = self.write_as(kind);
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
            for k in view.scan_keys(Perm::Gspo, &[g.0])? {
                txn.delete(Perm::Gspo.to_quad(&k))?;
            }
        }
        let mut ids = Vec::new();
        for quads in &parsed {
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

    /// Compact: merge base ⊕ delta into a new generation. The data does not change, so
    /// neither does the head commit.
    pub fn compact(&self) -> Result<()> {
        let mut w = self.writer.lock();
        if w.poisoned {
            return Err(Error::Poisoned);
        }
        let snap = self.snapshot();
        self.rebuild_locked(&mut w, &snap, &[], &[], None)?;
        Ok(())
    }

    /// Rebuild with the writer lock held: a new generation from `snap` plus `extra`
    /// sources and `extra_quads` (encoded store ids from a bulk write transaction).
    /// With `bulk`, the rebuild is a new commit; without it (compaction), the head stays.
    /// Returns the number of new quads and the receipt.
    fn rebuild_locked(
        &self,
        w: &mut WriterState,
        snap: &Snapshot,
        extra: &[Source],
        extra_quads: &[[Id; 4]],
        bulk: Option<BulkCommit>,
    ) -> Result<(u64, Receipt)> {
        let before = snap.len();
        // the old generation's WAL is the only other copy of the recent commits' ids:
        // the catalog must be durable before it is discarded
        self.catalog.lock().sync()?;
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
        let builder = Builder::new(&dir, bopts)?;
        if !snap.is_empty() || !extra_quads.is_empty() {
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
            snap.for_each_quad(&mut push)?;
            for q in extra_quads {
                push(q)?;
            }
            enc.flush()?;
        }
        for s in extra {
            builder.add_source(s)?;
        }
        builder.add_prefixes(self.prefixes());
        let meta = builder.finish()?;
        let mut gen_ = Generation::open(&dir, &name, self.root.is_some())?;
        gen_._tmp = tmp;
        w.next_bnode = w.next_bnode.max(meta.next_bnode);
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
        let dvocab_len = gen_.dvocab.len();
        let mut new_snap = Snapshot {
            generation: Arc::new(gen_),
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
            union_default_graph: self.opts.union_default_graph,
            delta_stats: Default::default(),
        };
        if bulk.is_some() {
            self.rebuild_text_locked(&mut new_snap, snap.text.clone());
        }
        self.current.store(Arc::new(new_snap));
        // Old generation files are unlinked; open readers keep their mmaps alive.
        if let (Some(root), Some(old)) = (&self.root, old)
            && old.starts_with(root)
            && old != dir
        {
            let _ = std::fs::remove_dir_all(old);
        }
        let receipt = Receipt {
            dataset_id: self.dataset_id,
            committed: bulk.is_some(),
            commit: head,
        };
        Ok((meta.quads.saturating_sub(before), receipt))
    }

    /// Write all quads as N-Quads to `w`.
    pub fn dump_nquads(&self, w: impl Write) -> Result<u64> {
        let snap = self.snapshot();
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

    /// Gzipped N-Quads backup into `dir` (Fuseki `/$/backup`). Returns the file path.
    pub fn backup(&self, dir: &Path, name: &str) -> Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let ts = crate::builder::now_rfc3339().replace(':', "-");
        let path = dir.join(format!("{name}_{ts}.nq.gz"));
        let f = BufWriter::new(File::create(&path)?);
        let gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        self.dump_nquads(gz)?;
        Ok(path)
    }

    pub fn disk_bytes(&self) -> u64 {
        match &self.root {
            Some(r) => dir_size(r),
            None => 0,
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
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
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
            union_default_graph: self.base.union_default_graph,
            delta_stats: Default::default(),
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
    pub fn commit(mut self) -> Result<Receipt> {
        if self.guard.poisoned {
            return Err(Error::Poisoned);
        }
        if self.bulk.is_empty() {
            return self.publish_log();
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
        let (_, receipt) =
            self.store
                .rebuild_locked(&mut self.guard, &view, &[], &bulk, Some(commit))?;
        Ok(receipt)
    }

    fn publish_log(&mut self) -> Result<Receipt> {
        let gen_ = &self.base.generation;
        let head = self.guard.head;
        if self.net_ins == 0 && self.net_del == 0 {
            // nothing changed (or every change was undone): no commit, nothing published
            return Ok(Receipt {
                dataset_id: self.store.dataset_id,
                committed: false,
                commit: head,
            });
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
            union_default_graph: self.base.union_default_graph,
            delta_stats: Default::default(),
        };
        self.store.maintain_text(&mut snap, &self.log);
        self.store.current.store(Arc::new(snap));
        Ok(Receipt {
            dataset_id: self.store.dataset_id,
            committed: true,
            commit: c,
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
