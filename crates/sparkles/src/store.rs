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
        set.range((
            Bound::Included(pad(prefix, 0)),
            Bound::Included(pad(prefix, u64::MAX)),
        ))
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
    pub union_default_graph: bool,
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
        mut f: impl FnMut(Chunk<'_>) -> Result<bool>,
    ) -> Result<()> {
        let pi = perm.index();
        let mut ins = Delta::range(&self.delta.ins[pi], prefix).peekable();
        let mut del = Delta::range(&self.delta.del[pi], prefix).peekable();
        let base = self.perm(perm);
        let mut stop = false;
        if base.rows > 0 {
            let r = base.for_each_range_until(&self.cache, prefix, |blk, s, e| {
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
        }
    }
}

struct WriterState {
    wal: Option<BufWriter<File>>,
    next_bnode: u64,
}

pub struct Store {
    root: Option<PathBuf>,
    opts: StoreOptions,
    current: ArcSwap<Snapshot>,
    writer: Mutex<WriterState>,
    cache: Arc<BlockCache>,
    results: Arc<crate::sparql::cache::ResultCache>,
    prefixes: Mutex<BTreeMap<String, String>>,
}

const WAL_INSERT: u8 = 1;
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
        Store {
            root: None,
            current: ArcSwap::from_pointee(Snapshot {
                generation: gen_,
                delta: Delta::default(),
                version: 0,
                cache: cache.clone(),
                results: results.clone(),
                dvocab_len: 0,
                union_default_graph: opts.union_default_graph,
            }),
            writer: Mutex::new(WriterState {
                wal: None,
                next_bnode: 0,
            }),
            cache,
            results,
            prefixes: Mutex::new(BTreeMap::new()),
            opts,
        }
    }

    /// Open (or create) a persistent store rooted at `root`.
    pub fn open(root: &Path, opts: StoreOptions) -> Result<Store> {
        std::fs::create_dir_all(root)?;
        let current_file = root.join("CURRENT");
        if !current_file.exists() {
            // create an empty generation
            let name = "gen-0001";
            Builder::new(&root.join(name), opts.build.clone())?.finish()?;
            write_atomic(&current_file, name.as_bytes())?;
        }
        let name = std::fs::read_to_string(&current_file)?.trim().to_string();
        let gen_ = Generation::open(&root.join(&name), &name, true)?;
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
        // replay the WAL
        let wal_path = root.join(&name).join("wal.log");
        let mut delta = Delta::default();
        let mut version = 0;
        let gen_ = Arc::new(gen_);
        if wal_path.exists() {
            let mut buf = Vec::new();
            File::open(&wal_path)?.read_to_end(&mut buf)?;
            let mut pending: Vec<(u8, [Id; 4])> = Vec::new();
            let mut good = 0usize;
            let probe = Snapshot {
                generation: gen_.clone(),
                delta: Delta::default(),
                version: 0,
                cache: cache.clone(),
                results: results.clone(),
                dvocab_len: u64::MAX,
                union_default_graph: false,
            };
            for (i, rec) in buf.as_chunks::<WAL_REC>().0.iter().enumerate() {
                let q: [Id; 4] = std::array::from_fn(|j| {
                    Id(u64::from_le_bytes(
                        rec[1 + j * 8..9 + j * 8].try_into().unwrap(),
                    ))
                });
                match rec[0] {
                    WAL_INSERT | WAL_DELETE => pending.push((rec[0], q)),
                    WAL_COMMIT => {
                        for (op, q) in pending.drain(..) {
                            let in_base = probe
                                .perm(Perm::Spo)
                                .contains(&cache, &Perm::Spo.to_key(&q))?;
                            apply(&mut delta, &q, op == WAL_INSERT, in_base);
                        }
                        next_bnode = next_bnode.max(q[0].0);
                        version += 1;
                        good = (i + 1) * WAL_REC;
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
        let wal = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&wal_path)?;
        let dvocab_len = gen_.dvocab.len();
        Ok(Store {
            root: Some(root.to_path_buf()),
            current: ArcSwap::from_pointee(Snapshot {
                generation: gen_,
                delta,
                version,
                cache: cache.clone(),
                results: results.clone(),
                dvocab_len,
                union_default_graph: opts.union_default_graph,
            }),
            writer: Mutex::new(WriterState {
                wal: Some(BufWriter::new(wal)),
                next_bnode,
            }),
            cache,
            results,
            prefixes: Mutex::new(prefixes),
            opts,
        })
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
        let guard = self.writer.lock();
        let base = self.snapshot();
        WriteTxn {
            store: self,
            delta: base.delta.clone(),
            base,
            guard,
            log: Vec::new(),
            bulk: Vec::new(),
        }
    }

    /// Load RDF sources. Into an empty store (or for large inputs) this runs the bulk
    /// builder and rebuilds the base generation; small loads are transactional inserts.
    pub fn load(&self, sources: &[Source]) -> Result<u64> {
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
            self.rebuild(sources)
        } else {
            let before = snap.len();
            let mut txn = self.write();
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
            txn.commit()?;
            self.add_prefixes(prefixes)?;
            Ok(self.snapshot().len().saturating_sub(before))
        }
    }

    /// Compact: merge base ⊕ delta into a new generation.
    pub fn compact(&self) -> Result<()> {
        self.rebuild(&[])?;
        Ok(())
    }

    /// Build a new generation from the current contents plus `extra` sources and
    /// switch to it.
    fn rebuild(&self, extra: &[Source]) -> Result<u64> {
        let mut w = self.writer.lock();
        let snap = self.snapshot();
        self.rebuild_locked(&mut w, &snap, extra, &[])
    }

    /// Rebuild with the writer lock held; `extra_quads` are encoded store ids (from a
    /// bulk write transaction) added to the new generation.
    fn rebuild_locked(
        &self,
        w: &mut WriterState,
        snap: &Snapshot,
        extra: &[Source],
        extra_quads: &[[Id; 4]],
    ) -> Result<u64> {
        let before = snap.len();
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
        if let Some(root) = &self.root {
            write_atomic(&root.join("CURRENT"), name.as_bytes())?;
            let wal = OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("wal.log"))?;
            w.wal = Some(BufWriter::new(wal));
        }
        self.add_prefixes(meta.prefixes.clone())?;
        let old = snap.generation.dir.clone();
        let dvocab_len = gen_.dvocab.len();
        self.current.store(Arc::new(Snapshot {
            generation: Arc::new(gen_),
            delta: Delta::default(),
            version: snap.version + 1,
            cache: self.cache.clone(),
            results: self.results.clone(),
            dvocab_len,
            union_default_graph: self.opts.union_default_graph,
        }));
        // Old generation files are unlinked; open readers keep their mmaps alive.
        if let (Some(root), Some(old)) = (&self.root, old)
            && old.starts_with(root)
            && old != dir
        {
            let _ = std::fs::remove_dir_all(old);
        }
        Ok(meta.quads.saturating_sub(before))
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

fn dir_size(p: &Path) -> u64 {
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

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(tmp, path)?;
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
}

impl WriteTxn<'_> {
    /// Snapshot view including this transaction's uncommitted changes.
    pub fn view(&self) -> Snapshot {
        Snapshot {
            generation: self.base.generation.clone(),
            delta: self.delta.clone(),
            version: self.base.version,
            cache: self.base.cache.clone(),
            results: self.base.results.clone(),
            dvocab_len: self.base.generation.dvocab.len(),
            union_default_graph: self.base.union_default_graph,
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

    /// Durably commit and publish a new snapshot.
    pub fn commit(mut self) -> Result<u64> {
        if self.bulk.is_empty() {
            return self.publish_log();
        }
        // Build the new generation from this transaction's view (base + uncommitted delta)
        // plus the bulk quads; switching CURRENT is the atomic commit point, so the
        // transaction's small changes need no WAL records.
        let bulk = std::mem::take(&mut self.bulk);
        let view = self.view();
        self.base.generation.dvocab.sync()?;
        self.store
            .rebuild_locked(&mut self.guard, &view, &[], &bulk)?;
        Ok(self.store.snapshot().version)
    }

    fn publish_log(&mut self) -> Result<u64> {
        let gen_ = &self.base.generation;
        if self.log.is_empty() {
            return Ok(self.base.version);
        }
        gen_.dvocab.sync()?;
        let next_bnode = self.guard.next_bnode;
        if let Some(wal) = self.guard.wal.as_mut() {
            let mut rec = [0u8; WAL_REC];
            for (op, q) in &self.log {
                rec[0] = *op;
                for j in 0..4 {
                    rec[1 + j * 8..9 + j * 8].copy_from_slice(&q[j].0.to_le_bytes());
                }
                wal.write_all(&rec)?;
            }
            rec[0] = WAL_COMMIT;
            rec[1..9].copy_from_slice(&next_bnode.to_le_bytes());
            rec[9..].fill(0);
            wal.write_all(&rec)?;
            wal.flush()?;
            wal.get_ref().sync_data()?;
        }
        let version = self.base.version + 1;
        self.store.current.store(Arc::new(Snapshot {
            generation: gen_.clone(),
            delta: std::mem::take(&mut self.delta),
            version,
            cache: self.base.cache.clone(),
            results: self.base.results.clone(),
            dvocab_len: gen_.dvocab.len(),
            union_default_graph: self.base.union_default_graph,
        }));
        Ok(version)
    }
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
