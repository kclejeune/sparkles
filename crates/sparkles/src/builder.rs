//! Bulk index builder (QLever `IndexImpl::createFromFiles` pipeline, TDB2 `xloader` role).
//!
//! 1. **Parse & encode** (parallel): each parser chunk owns an [`Encoder`] that maps
//!    terms to inline ids / blank-node ids, or to *batch-local* ids for vocabulary terms.
//!    Every `batch_quads` quads, the batch's distinct keys are sorted and written as a
//!    partial vocabulary together with the batch's quads.
//! 2. **Vocabulary merge**: k-way merge of all partial vocabularies into the sorted,
//!    front-coded base vocabulary; per-batch `rank → global id` maps are written
//!    sequentially along the way.
//! 3. **Remap** (parallel) batch quads to global ids.
//! 4. **Permutations**: for each permutation, sort (in memory with a parallel sort if it
//!    fits into the memory budget, otherwise via sorted runs + k-way merge) and stream
//!    into the block writer. Statistics for the planner are gathered on the way.

use crate::error::{Error, Result};
use crate::id::{self, Id, Tag};
use crate::index::{Key, Perm, PermWriter};
use crate::io::{QuadSink, Source, parse_source};
use crate::vocab::{Vocab, VocabWriter};
use memmap2::Mmap;
use oxrdf::{GraphName, NamedOrBlankNode, Quad, Term};
use parking_lot::Mutex;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[derive(Clone, Debug)]
pub struct BuildOptions {
    pub threads: usize,
    /// Quads per partial vocabulary batch.
    pub batch_quads: usize,
    /// Bytes of distinct vocabulary keys per batch: a batch also ends here, so data with
    /// long literals (abstracts) does not hold `batch_quads` of them per parser thread.
    pub batch_key_bytes: usize,
    /// Max quads sorted in memory at once (32 bytes each).
    pub sort_mem_quads: usize,
    /// First blank node id to allocate.
    pub first_bnode: u64,
}

impl Default for BuildOptions {
    fn default() -> Self {
        BuildOptions {
            threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
            batch_quads: 4_000_000,
            batch_key_bytes: 128 << 20,
            sort_mem_quads: 64_000_000,
            first_bnode: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PredicateStat {
    pub p: u64,
    pub count: u64,
    pub distinct_subjects: u64,
    pub distinct_objects: u64,
}

/// Planner statistics gathered at build time (ids are base ids).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    pub quads: u64,
    pub distinct_subjects: u64,
    pub distinct_predicates: u64,
    pub distinct_objects: u64,
    pub predicates: Vec<PredicateStat>,
    /// (graph id, quads)
    pub graphs: Vec<(u64, u64)>,
    /// (class id, distinct instances) for `rdf:type`
    pub classes: Vec<(u64, u64)>,
}

impl Stats {
    pub fn predicate(&self, p: u64) -> Option<&PredicateStat> {
        self.predicates
            .binary_search_by_key(&p, |s| s.p)
            .ok()
            .map(|i| &self.predicates[i])
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct IndexMeta {
    pub format_version: u32,
    pub quads: u64,
    pub terms: u64,
    pub next_bnode: u64,
    pub prefixes: BTreeMap<String, String>,
    pub created: String,
}

pub const FORMAT_VERSION: u32 = 2;

struct BatchInfo {
    id: usize,
    keys: u64,
    quads: u64,
}

/// Blank node label scope (one per source document), sharded to reduce contention.
pub struct LabelScope {
    shards: Vec<Mutex<FxHashMap<Box<str>, u64>>>,
}

impl LabelScope {
    fn new() -> LabelScope {
        LabelScope {
            shards: (0..32).map(|_| Mutex::new(FxHashMap::default())).collect(),
        }
    }
    fn get(&self, label: &str, next: &AtomicU64) -> u64 {
        let h = label
            .bytes()
            .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
        let mut shard = self.shards[(h % 32) as usize].lock();
        if let Some(&id) = shard.get(label) {
            return id;
        }
        let id = next.fetch_add(1, Ordering::Relaxed);
        shard.insert(label.into(), id);
        id
    }
}

pub struct Builder {
    dir: PathBuf,
    tmp: PathBuf,
    opts: BuildOptions,
    batches: Mutex<Vec<BatchInfo>>,
    next_batch: AtomicUsize,
    next_bnode: AtomicU64,
    prefixes: Mutex<BTreeMap<String, String>>,
    input_quads: AtomicU64,
    progress: Option<ProgressFn>,
    interrupt: Option<InterruptFn>,
}

/// Progress callback for long-running builds.
pub type ProgressFn = Arc<dyn Fn(&str) + Send + Sync>;

/// Called every [`INTERRUPT_EVERY`] quads an encoder takes and between build phases: an
/// error stops the build (a cancelled write, a deadline, too little disk space).
pub type InterruptFn = Arc<dyn Fn() -> Result<()> + Send + Sync>;

/// Quads an encoder takes between two [`InterruptFn`] calls.
pub const INTERRUPT_EVERY: u64 = 65_536;

pub enum Slot<'a> {
    Id(Id),
    Key(&'a [u8]),
}

impl Builder {
    /// Start building an index generation in `dir` (must be empty or not exist).
    pub fn new(dir: &Path, opts: BuildOptions) -> Result<Builder> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join("tmp");
        std::fs::create_dir_all(&tmp)?;
        Ok(Builder {
            dir: dir.to_path_buf(),
            tmp,
            next_bnode: AtomicU64::new(opts.first_bnode),
            opts,
            batches: Mutex::new(Vec::new()),
            next_batch: AtomicUsize::new(0),
            prefixes: Mutex::new(BTreeMap::new()),
            input_quads: AtomicU64::new(0),
            progress: None,
            interrupt: None,
        })
    }

    pub fn with_progress(mut self, f: ProgressFn) -> Self {
        self.progress = Some(f);
        self
    }

    pub fn with_interrupt(mut self, f: InterruptFn) -> Self {
        self.interrupt = Some(f);
        self
    }

    fn interrupted(&self) -> Result<()> {
        match &self.interrupt {
            Some(f) => f(),
            None => Ok(()),
        }
    }

    fn report(&self, msg: &str) {
        if let Some(p) = &self.progress {
            p(msg);
        }
        tracing::info!("{msg}");
    }

    pub fn add_prefixes(&self, p: BTreeMap<String, String>) {
        self.prefixes.lock().extend(p);
    }

    /// Parse and encode a source (in parallel where the format allows it).
    pub fn add_source(&self, src: &Source) -> Result<()> {
        let scope = Arc::new(LabelScope::new());
        self.report(&format!("parsing {}", src.name));
        let prefixes = parse_source(src, self.opts.threads, || self.encoder(scope.clone()))?;
        self.add_prefixes(prefixes);
        Ok(())
    }

    pub fn new_scope(&self) -> Arc<LabelScope> {
        Arc::new(LabelScope::new())
    }

    pub fn encoder(&self, scope: Arc<LabelScope>) -> Encoder<'_> {
        Encoder {
            b: self,
            scope,
            keys: FxHashMap::default(),
            key_bytes: 0,
            quads: Vec::new(),
            keybuf: Vec::with_capacity(128),
            taken: 0,
        }
    }

    fn write_batch(&self, keys: FxHashMap<Box<[u8]>, u32>, mut quads: Vec<[u64; 4]>) -> Result<()> {
        if quads.is_empty() {
            return Ok(());
        }
        let id = self.next_batch.fetch_add(1, Ordering::Relaxed);
        let mut entries: Vec<(Box<[u8]>, u32)> = keys.into_iter().collect();
        entries.par_sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut rank = vec![0u64; entries.len()];
        for (r, (_, local)) in entries.iter().enumerate() {
            rank[*local as usize] = r as u64;
        }
        let mut w = BufWriter::new(File::create(self.tmp.join(format!("b{id}.voc")))?);
        for (k, _) in &entries {
            w.write_all(&(k.len() as u32).to_le_bytes())?;
            w.write_all(k)?;
        }
        w.flush()?;
        for q in quads.iter_mut() {
            for v in q.iter_mut() {
                if Id(*v).tag() == Tag::Local {
                    *v = Id::local(rank[Id(*v).payload() as usize]).0;
                }
            }
        }
        write_u64s(&self.tmp.join(format!("b{id}.q")), quads.as_flattened())?;
        self.input_quads
            .fetch_add(quads.len() as u64, Ordering::Relaxed);
        self.batches.lock().push(BatchInfo {
            id,
            keys: entries.len() as u64,
            quads: quads.len() as u64,
        });
        Ok(())
    }

    /// Run the merge / sort phases and write the generation. Returns index metadata.
    pub fn finish(self) -> Result<IndexMeta> {
        let mut batches = std::mem::take(&mut *self.batches.lock());
        batches.sort_by_key(|b| b.id);
        let total_in: u64 = batches.iter().map(|b| b.quads).sum();
        self.report(&format!(
            "merging {} partial vocabularies ({} input quads)",
            batches.len(),
            total_in
        ));

        // ---- 2. vocabulary merge -------------------------------------------------
        self.interrupted()?;
        let terms = self.merge_vocab(&batches)?;
        self.report(&format!("vocabulary: {terms} terms"));

        // ---- 3. remap ------------------------------------------------------------
        self.interrupted()?;
        let in_memory = total_in as usize <= self.opts.sort_mem_quads;
        let remapped: Vec<Vec<[u64; 4]>> = batches
            .par_iter()
            .map(|b| -> Result<Vec<[u64; 4]>> {
                let map = read_map(&self.tmp.join(format!("b{}.map", b.id)))?;
                let mut quads = read_quads(&self.tmp.join(format!("b{}.q", b.id)))?;
                for q in quads.iter_mut() {
                    for v in q.iter_mut() {
                        if Id(*v).tag() == Tag::Local {
                            *v = Id::vocab(map[Id(*v).payload() as usize]).0;
                        }
                    }
                }
                std::fs::remove_file(self.tmp.join(format!("b{}.map", b.id)))?;
                if in_memory {
                    std::fs::remove_file(self.tmp.join(format!("b{}.q", b.id)))?;
                    Ok(quads)
                } else {
                    write_u64s(&self.tmp.join(format!("b{}.q", b.id)), quads.as_flattened())?;
                    Ok(Vec::new())
                }
            })
            .collect::<Result<_>>()?;

        // ---- 4. permutations -----------------------------------------------------
        let vocab = Vocab::open(&self.dir)?;
        let rdf_type = vocab
            .find(&id::iri_key(oxrdf::vocab::rdf::TYPE.as_str()))
            .ok()
            .map(|i| Id::vocab(i).0);
        let mut stats = Stats::default();
        let all: Vec<[u64; 4]> = if in_memory {
            remapped.into_iter().flatten().collect()
        } else {
            Vec::new()
        };
        let mut keys: Vec<Key> = Vec::new();
        let mut rows = 0;
        for perm in Perm::ALL {
            self.interrupted()?;
            self.report(&format!("building permutation {}", perm.name()));
            let mut w = PermWriter::create(&self.dir, perm)?;
            let mut col = StatsCollector::new(perm, rdf_type);
            if in_memory {
                keys.clear();
                keys.par_extend(
                    all.par_iter()
                        .map(|q| perm.to_key(&[Id(q[0]), Id(q[1]), Id(q[2]), Id(q[3])])),
                );
                keys.par_sort_unstable();
                keys.dedup();
                for k in &keys {
                    col.push(k);
                    w.push(*k)?;
                }
            } else {
                // the merged runs can repeat a quad that occurs in several batches:
                // drop repeats before both consumers, so statistics match the index
                let mut last: Option<Key> = None;
                self.external_sort(perm, &batches, |k| {
                    if last == Some(k) {
                        return Ok(());
                    }
                    last = Some(k);
                    col.push(&k);
                    w.push(k)
                })?;
            }
            col.finish(&mut stats);
            rows = w.finish(&self.dir, perm)?;
        }
        drop(keys);
        stats.quads = rows;
        stats.predicates.sort_by_key(|p| p.p);

        let meta = IndexMeta {
            format_version: FORMAT_VERSION,
            quads: rows,
            terms,
            next_bnode: self.next_bnode.load(Ordering::Relaxed),
            prefixes: std::mem::take(&mut *self.prefixes.lock()),
            created: now_rfc3339(),
        };
        crate::store::write_synced(
            &self.dir.join("stats.json"),
            &serde_json::to_vec(&stats).unwrap(),
        )?;
        crate::store::write_synced(
            &self.dir.join("meta.json"),
            &serde_json::to_vec_pretty(&meta).unwrap(),
        )?;
        let _ = std::fs::remove_dir_all(&self.tmp);
        crate::store::sync_dir(&self.dir)?;
        self.report(&format!("index complete: {rows} quads, {terms} terms"));
        Ok(meta)
    }

    fn merge_vocab(&self, batches: &[BatchInfo]) -> Result<u64> {
        struct Run {
            r: BufReader<File>,
            map: BufWriter<File>,
            left: u64,
        }
        let mut runs: Vec<Run> = batches
            .iter()
            .map(|b| -> Result<Run> {
                Ok(Run {
                    r: BufReader::with_capacity(
                        1 << 16,
                        File::open(self.tmp.join(format!("b{}.voc", b.id)))?,
                    ),
                    map: BufWriter::new(File::create(self.tmp.join(format!("b{}.map", b.id)))?),
                    left: b.keys,
                })
            })
            .collect::<Result<_>>()?;
        fn next_key(run: &mut Run) -> Result<Option<Vec<u8>>> {
            if run.left == 0 {
                return Ok(None);
            }
            run.left -= 1;
            let mut len = [0u8; 4];
            run.r.read_exact(&mut len)?;
            let mut k = vec![0u8; u32::from_le_bytes(len) as usize];
            run.r.read_exact(&mut k)?;
            Ok(Some(k))
        }
        let mut heap = BinaryHeap::new();
        for (i, run) in runs.iter_mut().enumerate() {
            if let Some(k) = next_key(run)? {
                heap.push(Reverse((k, i)));
            }
        }
        let mut w = VocabWriter::create(&self.dir)?;
        let mut last: Option<(Vec<u8>, u64)> = None;
        while let Some(Reverse((k, i))) = heap.pop() {
            let gid = match &last {
                Some((lk, gid)) if *lk == k => *gid,
                _ => {
                    let gid = w.push(&k)?;
                    last = Some((k, gid));
                    gid
                }
            };
            runs[i].map.write_all(&gid.to_le_bytes())?;
            if let Some(k) = next_key(&mut runs[i])? {
                heap.push(Reverse((k, i)));
            }
        }
        for (b, mut run) in batches.iter().zip(runs) {
            run.map.flush()?;
            std::fs::remove_file(self.tmp.join(format!("b{}.voc", b.id)))?;
        }
        w.finish()
    }

    /// Sorted runs + k-way merge for inputs that exceed the in-memory budget.
    fn external_sort(
        &self,
        perm: Perm,
        batches: &[BatchInfo],
        mut out: impl FnMut(Key) -> Result<()>,
    ) -> Result<()> {
        let mut runs = Vec::new();
        let mut buf: Vec<Key> = Vec::new();
        let flush = |buf: &mut Vec<Key>, runs: &mut Vec<PathBuf>| -> Result<()> {
            buf.par_sort_unstable();
            buf.dedup();
            let p = self.tmp.join(format!("run-{}-{}", perm.name(), runs.len()));
            write_u64s(&p, buf.as_flattened())?;
            runs.push(p);
            buf.clear();
            Ok(())
        };
        for b in batches {
            let quads = read_quads(&self.tmp.join(format!("b{}.q", b.id)))?;
            for q in quads {
                buf.push(perm.to_key(&[Id(q[0]), Id(q[1]), Id(q[2]), Id(q[3])]));
                if buf.len() >= self.opts.sort_mem_quads {
                    flush(&mut buf, &mut runs)?;
                }
            }
        }
        flush(&mut buf, &mut runs)?;
        struct RunReader {
            r: BufReader<File>,
        }
        impl RunReader {
            fn next(&mut self) -> Result<Option<Key>> {
                let mut b = [0u8; 32];
                match self.r.read_exact(&mut b) {
                    Ok(()) => Ok(Some(std::array::from_fn(|i| {
                        u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap())
                    }))),
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
        }
        let mut readers: Vec<RunReader> = runs
            .iter()
            .map(|p| -> Result<_> {
                Ok(RunReader {
                    r: BufReader::with_capacity(1 << 20, File::open(p)?),
                })
            })
            .collect::<Result<_>>()?;
        let mut heap = BinaryHeap::new();
        for (i, r) in readers.iter_mut().enumerate() {
            if let Some(k) = r.next()? {
                heap.push(Reverse((k, i)));
            }
        }
        while let Some(Reverse((k, i))) = heap.pop() {
            out(k)?;
            if let Some(n) = readers[i].next()? {
                heap.push(Reverse((n, i)));
            }
        }
        for p in runs {
            std::fs::remove_file(p)?;
        }
        Ok(())
    }
}

/// Per-chunk encoder: terms → ids, vocabulary terms → batch-local ids.
pub struct Encoder<'b> {
    b: &'b Builder,
    scope: Arc<LabelScope>,
    keys: FxHashMap<Box<[u8]>, u32>,
    /// total length of `keys`
    key_bytes: usize,
    quads: Vec<[u64; 4]>,
    keybuf: Vec<u8>,
    /// quads taken, for the [`InterruptFn`] calls
    taken: u64,
}

impl Encoder<'_> {
    fn local(&mut self, key: &[u8]) -> u64 {
        if let Some(&i) = self.keys.get(key) {
            return Id::local(i as u64).0;
        }
        let i = self.keys.len() as u32;
        self.keys.insert(key.into(), i);
        self.key_bytes += key.len();
        Id::local(i as u64).0
    }

    fn term(&mut self, t: &Term) -> u64 {
        match t {
            Term::BlankNode(b) => Id::bnode(self.scope.get(b.as_str(), &self.b.next_bnode)).0,
            Term::Literal(l) => {
                if let Some(id) = id::inline_literal(l.value(), l.datatype().as_str()) {
                    return id.0;
                }
                let mut kb = std::mem::take(&mut self.keybuf);
                kb.clear();
                id::write_literal_key(l, &mut kb);
                let r = self.local(&kb);
                self.keybuf = kb;
                r
            }
            Term::NamedNode(n) => self.iri(n.as_str()),
            Term::Triple(_) => {
                let mut kb = std::mem::take(&mut self.keybuf);
                kb.clear();
                let (scope, next) = (self.scope.clone(), &self.b.next_bnode);
                id::write_term_key_with(t, &mut kb, &mut |b| scope.get(b.as_str(), next));
                let r = self.local(&kb);
                self.keybuf = kb;
                r
            }
        }
    }

    fn iri(&mut self, iri: &str) -> u64 {
        let mut kb = std::mem::take(&mut self.keybuf);
        kb.clear();
        kb.push(b'<');
        kb.extend_from_slice(iri.as_bytes());
        let r = self.local(&kb);
        self.keybuf = kb;
        r
    }

    pub fn push_quad(&mut self, q: &Quad) -> Result<()> {
        let s = match &q.subject {
            NamedOrBlankNode::NamedNode(n) => self.iri(n.as_str()),
            NamedOrBlankNode::BlankNode(b) => {
                Id::bnode(self.scope.get(b.as_str(), &self.b.next_bnode)).0
            }
            #[allow(unreachable_patterns)]
            _ => {
                return Err(Error::unsupported(
                    "RDF 1.2 triple terms in subject position",
                ));
            }
        };
        let p = self.iri(q.predicate.as_str());
        let o = self.term(&q.object);
        let g = match &q.graph_name {
            GraphName::DefaultGraph => Id::DEFAULT_GRAPH.0,
            GraphName::NamedNode(n) => self.iri(n.as_str()),
            GraphName::BlankNode(b) => Id::bnode(self.scope.get(b.as_str(), &self.b.next_bnode)).0,
        };
        self.quads.push([s, p, o, g]);
        self.maybe_flush()
    }

    /// Push a pre-encoded quad (compaction path): ids are kept, keys go to the vocabulary.
    pub fn push_slots(&mut self, slots: [Slot<'_>; 4]) -> Result<()> {
        let mut q = [0u64; 4];
        for (i, s) in slots.into_iter().enumerate() {
            q[i] = match s {
                Slot::Id(id) => id.0,
                Slot::Key(k) => self.local(k),
            };
        }
        self.quads.push(q);
        self.maybe_flush()
    }

    fn maybe_flush(&mut self) -> Result<()> {
        self.taken += 1;
        if self.taken.is_multiple_of(INTERRUPT_EVERY) {
            self.b.interrupted()?;
        }
        if self.quads.len() >= self.b.opts.batch_quads
            || self.key_bytes >= self.b.opts.batch_key_bytes
        {
            self.flush()?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let keys = std::mem::take(&mut self.keys);
        let quads = std::mem::take(&mut self.quads);
        self.key_bytes = 0;
        self.b.write_batch(keys, quads)
    }
}

impl QuadSink for Encoder<'_> {
    fn quad(&mut self, q: Quad) -> Result<()> {
        self.push_quad(&q)
    }
    fn finish(mut self) -> Result<()> {
        self.flush()
    }
}

impl Drop for Encoder<'_> {
    fn drop(&mut self) {
        if !self.quads.is_empty() {
            let _ = self.flush();
        }
    }
}

/// Collects planner statistics while a permutation is streamed in sorted order.
struct StatsCollector {
    perm: Perm,
    rdf_type: Option<u64>,
    prev: Option<Key>,
    distinct0: u64,
    // per col0 run
    run_count: u64,
    run_distinct1: u64,
    out: Vec<(u64, u64, u64)>, // (col0, count, distinct col1)
    // rdf:type classes in POS: (class, distinct subjects)
    classes: Vec<(u64, u64)>,
    class_cur: Option<(u64, u64, u64)>, // (class, n, last subject)
}

impl StatsCollector {
    fn new(perm: Perm, rdf_type: Option<u64>) -> Self {
        StatsCollector {
            perm,
            rdf_type,
            prev: None,
            distinct0: 0,
            run_count: 0,
            run_distinct1: 0,
            out: Vec::new(),
            classes: Vec::new(),
            class_cur: None,
        }
    }

    #[inline]
    fn push(&mut self, k: &Key) {
        let new0 = self.prev.is_none_or(|p| p[0] != k[0]);
        if new0 {
            if let Some(p) = self.prev {
                self.out.push((p[0], self.run_count, self.run_distinct1));
            }
            self.distinct0 += 1;
            self.run_count = 0;
            self.run_distinct1 = 0;
        }
        if new0 || self.prev.is_some_and(|p| p[1] != k[1]) {
            self.run_distinct1 += 1;
        }
        self.run_count += 1;
        if self.perm == Perm::Pos && Some(k[0]) == self.rdf_type {
            // key = (type, class, subject, g)
            match &mut self.class_cur {
                Some((c, n, last)) if *c == k[1] => {
                    if *last != k[2] {
                        *n += 1;
                        *last = k[2];
                    }
                }
                cur => {
                    if let Some((c, n, _)) = cur.take() {
                        self.classes.push((c, n));
                    }
                    *cur = Some((k[1], 1, k[2]));
                }
            }
        }
        self.prev = Some(*k);
    }

    fn finish(mut self, stats: &mut Stats) {
        if let Some(p) = self.prev {
            self.out.push((p[0], self.run_count, self.run_distinct1));
        }
        if let Some((c, n, _)) = self.class_cur.take() {
            self.classes.push((c, n));
        }
        match self.perm {
            Perm::Spo => stats.distinct_subjects = self.distinct0,
            Perm::Osp => stats.distinct_objects = self.distinct0,
            Perm::Pso => {
                stats.distinct_predicates = self.distinct0;
                stats.predicates = self
                    .out
                    .iter()
                    .map(|&(p, count, ds)| PredicateStat {
                        p,
                        count,
                        distinct_subjects: ds,
                        distinct_objects: 0,
                    })
                    .collect();
            }
            Perm::Pos => {
                // PSO ran before POS (Perm::ALL order), so predicates are populated.
                let m: FxHashMap<u64, u64> = self.out.iter().map(|&(p, _, d)| (p, d)).collect();
                for ps in &mut stats.predicates {
                    ps.distinct_objects = m.get(&ps.p).copied().unwrap_or(0);
                }
                self.classes.sort_by_key(|&(_, n)| Reverse(n));
                stats.classes = self.classes;
            }
            Perm::Gspo => {
                stats.graphs = self.out.iter().map(|&(g, n, _)| (g, n)).collect();
            }
            _ => {}
        }
    }
}

fn write_u64s(path: &Path, vals: &[u64]) -> Result<()> {
    let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
    for v in vals {
        w.write_all(&v.to_le_bytes())?;
    }
    w.flush()?;
    Ok(())
}

fn read_map(path: &Path) -> Result<Vec<u64>> {
    let f = File::open(path)?;
    if f.metadata()?.len() == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: temporary file written by this builder and not modified concurrently.
    let m = unsafe { Mmap::map(&f)? };
    Ok(m.as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_le_bytes(*c))
        .collect())
}

fn read_quads(path: &Path) -> Result<Vec<[u64; 4]>> {
    let v = read_map(path)?;
    Ok(v.as_chunks::<4>().0.to_vec())
}

pub fn now_rfc3339() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() as i64;
    // civil-from-days (Howard Hinnant)
    let days = secs.div_euclid(86400);
    let sod = secs.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dd = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{dd:02}T{:02}:{:02}:{:02}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{BlockCache, PermIndex};
    use crate::io::{RdfFormat, Source};

    #[test]
    fn an_interrupt_stops_the_build() {
        let mut nt = String::new();
        for i in 0..INTERRUPT_EVERY + 10 {
            nt.push_str(&format!(
                "<http://ex.org/s{i}> <http://ex.org/p> \"{i}\" .\n"
            ));
        }
        let src = Source::from_bytes(nt.into_bytes(), RdfFormat::NTriples, None);
        let calls = Arc::new(AtomicU64::new(0));
        let build = |stop_at: u64| {
            let dir = tempfile::tempdir().unwrap();
            let calls = calls.clone();
            let opts = BuildOptions {
                threads: 1,
                ..Default::default()
            };
            let b = Builder::new(dir.path(), opts)
                .unwrap()
                .with_interrupt(Arc::new(move || {
                    if calls.fetch_add(1, Ordering::Relaxed) + 1 >= stop_at {
                        return Err(Error::Cancelled);
                    }
                    Ok(())
                }));
            b.add_source(&src)?;
            b.finish()
        };
        // while encoding
        let r = build(1);
        assert!(
            matches!(r, Err(Error::Cancelled)),
            "{:?}",
            r.map(|m| m.quads)
        );
        assert_eq!(calls.swap(0, Ordering::Relaxed), 1);
        // between the phases of the build
        assert!(matches!(build(3), Err(Error::Cancelled)));
        calls.store(0, Ordering::Relaxed);
        assert_eq!(build(u64::MAX).unwrap().quads, INTERRUPT_EVERY + 10);
    }

    /// Tiny batch / sort budgets force many partial vocabularies and the external
    /// sorted-runs + k-way-merge path; the result must equal the in-memory build.
    #[test]
    fn external_sort_matches_in_memory() {
        let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
        for i in 0..500 {
            ttl.push_str(&format!(
                "ex:s{} ex:p{} ex:o{} , \"lit {}\" , {} .\n",
                i % 37,
                i % 5,
                i % 91,
                i % 13,
                i % 17
            ));
        }
        let build = |opts: BuildOptions| {
            let dir = tempfile::tempdir().unwrap();
            let b = Builder::new(dir.path(), opts).unwrap();
            b.add_source(&Source::from_bytes(
                ttl.as_bytes().to_vec(),
                RdfFormat::Turtle,
                None,
            ))
            .unwrap();
            let meta = b.finish().unwrap();
            let v = Vocab::open(dir.path()).unwrap();
            let cache = BlockCache::new(1 << 20);
            let mut perms = Vec::new();
            for p in Perm::ALL {
                let idx = PermIndex::open(dir.path(), p).unwrap();
                let mut keys = Vec::new();
                idx.for_each_range(&cache, &[], |b, s, e| {
                    for i in s..e {
                        let k = b.key(i);
                        // compare by term keys (ids may differ only if vocab differs)
                        keys.push(k.map(|x| match Id(x).tag() {
                            Tag::Vocab => v.get(Id(x).payload()).unwrap(),
                            _ => x.to_le_bytes().to_vec(),
                        }));
                    }
                    Ok(())
                })
                .unwrap();
                perms.push(keys);
            }
            (meta.quads, meta.terms, perms)
        };
        let small = build(BuildOptions {
            batch_quads: 7,
            batch_key_bytes: usize::MAX,
            sort_mem_quads: 50,
            threads: 3,
            first_bnode: 0,
        });
        // batches that end at a few keys' bytes rather than at a quad count
        let short = build(BuildOptions {
            batch_key_bytes: 100,
            sort_mem_quads: 50,
            threads: 3,
            ..Default::default()
        });
        let big = build(BuildOptions::default());
        assert!(small.0 > 1000);
        assert_eq!(small.0, big.0);
        assert_eq!(small.1, big.1);
        assert_eq!(small.2, big.2);
        assert_eq!(short, big);
    }

    #[test]
    fn external_sort_statistics_skip_duplicate_quads() {
        // every quad appears three times, in different batches
        let mut nt = String::new();
        for _ in 0..3 {
            for i in 0..40 {
                nt.push_str(&format!(
                    "<http://ex.org/s{i}> <http://ex.org/p{}> <http://ex.org/o{}> .\n\
                     <http://ex.org/s{i}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/C{}> .\n",
                    i % 3,
                    i % 7,
                    i % 2
                ));
            }
        }
        let stats = |opts: BuildOptions| {
            let dir = tempfile::tempdir().unwrap();
            let b = Builder::new(dir.path(), opts).unwrap();
            b.add_source(&Source::from_bytes(
                nt.as_bytes().to_vec(),
                RdfFormat::NTriples,
                None,
            ))
            .unwrap();
            let meta = b.finish().unwrap();
            let stats: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.path().join("stats.json")).unwrap())
                    .unwrap();
            (meta.quads, stats)
        };
        let (quads, external) = stats(BuildOptions {
            batch_quads: 5,
            sort_mem_quads: 20,
            threads: 2,
            ..Default::default()
        });
        let (_, in_memory) = stats(BuildOptions::default());
        assert_eq!(quads, 80);
        assert_eq!(external, in_memory);
        let counted: u64 = external["predicates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["count"].as_u64().unwrap())
            .sum();
        assert_eq!(counted, 80);
    }

    #[test]
    fn build_small() {
        let dir = tempfile::tempdir().unwrap();
        let ttl = r#"
@prefix ex: <http://ex.org/> .
ex:a a ex:C ; ex:p 1, 2, "x" ; ex:q _:b1 .
ex:b a ex:C ; ex:p 2 .
_:b1 ex:p ex:a .
"#;
        let b = Builder::new(dir.path(), BuildOptions::default()).unwrap();
        b.add_source(&Source::from_bytes(
            ttl.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        ))
        .unwrap();
        let meta = b.finish().unwrap();
        assert_eq!(meta.quads, 8);
        assert!(meta.prefixes.contains_key("ex"));
        let v = Vocab::open(dir.path()).unwrap();
        // ex:a ex:b ex:C ex:p ex:q "x" rdf:type
        assert_eq!(v.len(), 7);
        let cache = BlockCache::new(1 << 20);
        for p in Perm::ALL {
            let idx = PermIndex::open(dir.path(), p).unwrap();
            assert_eq!(idx.count(&cache, &[]).unwrap(), 8);
        }
        let stats: Stats =
            serde_json::from_slice(&std::fs::read(dir.path().join("stats.json")).unwrap()).unwrap();
        assert_eq!(stats.classes.len(), 1);
        assert_eq!(stats.classes[0].1, 2);
        assert_eq!(stats.distinct_predicates, 3);
        let pstat = stats
            .predicate(Id::vocab(v.find(&id::iri_key("http://ex.org/p")).unwrap()).0)
            .unwrap();
        assert_eq!(pstat.count, 5);
        assert_eq!(pstat.distinct_subjects, 3);
        assert_eq!(pstat.distinct_objects, 4);
    }
}
