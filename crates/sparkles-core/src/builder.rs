//! Bulk index builder (QLever `IndexImpl::createFromFiles` pipeline, TDB2 `xloader` role).
//!
//! 1. **Parse & encode** (parallel): each parser chunk owns an [`Encoder`] that maps
//!    terms to inline ids / blank-node ids, or to *batch-local* ids for vocabulary terms.
//!    Every `batch_quads` quads, the batch's distinct keys are sorted and written as a
//!    partial vocabulary (front-coded and LZ4-compressed blocks, see
//!    [`vocabmerge::write_partial`]) together with the batch's quads (compressed blocks
//!    of columns, in the format of a sorted run).
//! 2. **Vocabulary merge**: k-way merge of all partial vocabularies into the sorted,
//!    front-coded base vocabulary; per-batch `rank → global id` maps are written
//!    along the way as delta-coded records (see [`vocabmerge::Maps`]).
//! 3. **Remap and sort**: the batches are read once, in chunks of `sort_mem_quads`
//!    quads, and remapped to global ids in parallel. Each chunk is sorted in place in the
//!    order of SPO, OSP and PSO (and GSPO when there are named graphs) and written as a
//!    compressed sorted run per order (see [`runs`]). Input that fits into the budget is
//!    one chunk, kept in memory.
//! 4. **Permutations**: the runs of each order are merged, one order per thread, and
//!    streamed into its permutation. The order's partner permutation (SOP from SPO, OPS
//!    from OSP, POS from PSO) shares its first column and is produced from the same
//!    stream by sorting each run of that column (QLever builds its permutations in
//!    these pairs). Without named graphs GSPO is SPO behind the one graph. Writers run
//!    in threads of their own. Statistics for the planner are gathered on the way.

mod iostat;
mod runs;
mod vocabmerge;

use crate::error::{Error, Result};
use crate::id::{self, Id, Tag};
use crate::index::{Key, Perm, PermWriter};
use crate::io::{QuadSink, Source, parse_source};
use crate::sparql::cdt;
use crate::vocab::Vocab;
use oxrdf::{GraphName, NamedOrBlankNode, Quad, Term};
use parking_lot::Mutex;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

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
    /// the characteristic sets of the subjects, most subjects first (see [`CharSet`])
    #[serde(default)]
    pub charsets: Vec<CharSet>,
    /// subjects whose characteristic set is not in `charsets` (rare sets beyond
    /// [`MAX_CHARSETS`])
    #[serde(default)]
    pub charset_others: u64,
}

/// A characteristic set (Neumann and Moerkotte, ICDE 2011): a set of predicates, the
/// number of subjects whose predicates are exactly these, and the number of triples each
/// predicate has over those subjects. The classes a subject has by `rdf:type` count as
/// predicates of their own (see [`class_item`]), so that a set tells which predicates the
/// instances of a class have.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CharSet {
    /// sorted: the predicates, then the classes
    pub preds: Vec<u64>,
    pub subjects: u64,
    /// per predicate of `preds`
    pub triples: Vec<u64>,
}

/// Characteristic sets kept in the statistics at most, those of the most subjects.
pub const MAX_CHARSETS: usize = 10_000;

/// The item standing for class `class` (a base vocabulary id) in a characteristic set: its
/// payload under a tag no term has, so that it sorts after every predicate.
pub fn class_item(class: u64) -> Option<u64> {
    (Id(class).tag() == Tag::Vocab).then_some(0xF << id::PAYLOAD_BITS | (class & id::PAYLOAD_MASK))
}

/// The class a characteristic set item stands for, if it is one.
pub fn item_class(item: u64) -> Option<u64> {
    (item >> id::PAYLOAD_BITS == 0xF).then(|| Id::new(Tag::Vocab, item & id::PAYLOAD_MASK).0)
}

/// Distinct characteristic sets counted while a build streams SPO at most; the subjects
/// of sets found after that are counted as others.
const MAX_CHARSETS_SEEN: usize = 1 << 20;

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
    /// the place of the batch in the order of `id`, set before the vocabulary merge
    pos: usize,
    keys: u64,
    /// the size of the partial vocabulary file
    voc_bytes: u64,
    quads: u64,
    /// every [`vocabmerge::SAMPLE_EVERY`]th key of the partial vocabulary
    samples: Vec<vocabmerge::Sample>,
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
    /// whether a quad is in a graph other than the default graph
    named_graphs: AtomicBool,
    progress: Option<MessageFn>,
    interrupt: Option<InterruptFn>,
    /// bytes written to temporary files, for the phase log (see [`iostat`])
    tmp_bytes: iostat::TmpBytes,
    /// the start of the current phase and of the build
    phases: Mutex<(iostat::Phases, iostat::Phases)>,
}

/// The messages of a long-running build, one per phase (`building POS`). A build does
/// not know its fraction done, so this is not a [`ProgressFn`](crate::task::ProgressFn).
pub type MessageFn = Arc<dyn Fn(&str) + Send + Sync>;

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
            named_graphs: AtomicBool::new(false),
            progress: None,
            interrupt: None,
            tmp_bytes: Default::default(),
            phases: Mutex::new((iostat::Phases::start(), iostat::Phases::start())),
        })
    }

    pub fn with_progress(mut self, f: MessageFn) -> Self {
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

    /// Log the phase `name` that ends now (see [`iostat`]).
    fn phase_done(&self, name: &str) {
        self.phases.lock().0.end(name, &self.tmp_bytes);
    }

    fn report(&self, msg: &str) {
        if let Some(p) = &self.progress {
            p(msg);
        }
        tracing::info!(target: "sparkles::builder", "{msg}");
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
            last: Default::default(),
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
        let (samples, voc_bytes) = vocabmerge::write_partial(
            &self.tmp.join(format!("b{id}.voc")),
            entries.iter().map(|(k, _)| &**k),
        )?;
        self.tmp_bytes.add(iostat::Tmp::PartialVocab, voc_bytes);
        for q in quads.iter_mut() {
            for v in q.iter_mut() {
                if Id(*v).tag() == Tag::Local {
                    *v = Id::local(rank[Id(*v).payload() as usize]).0;
                }
            }
        }
        let q_bytes = runs::write_run(&self.tmp.join(format!("b{id}.q")), &quads)?;
        self.tmp_bytes.add(iostat::Tmp::Quads, q_bytes);
        self.input_quads
            .fetch_add(quads.len() as u64, Ordering::Relaxed);
        if quads.iter().any(|q| q[3] != Id::DEFAULT_GRAPH.0) {
            self.named_graphs.store(true, Ordering::Relaxed);
        }
        self.batches.lock().push(BatchInfo {
            id,
            pos: 0,
            keys: entries.len() as u64,
            voc_bytes,
            quads: quads.len() as u64,
            samples,
        });
        Ok(())
    }

    /// Run the merge / sort phases and write the generation. Returns index metadata.
    pub fn finish(self) -> Result<IndexMeta> {
        let mut batches = std::mem::take(&mut *self.batches.lock());
        batches.sort_by_key(|b| b.id);
        for (i, b) in batches.iter_mut().enumerate() {
            b.pos = i;
        }
        let total_in: u64 = batches.iter().map(|b| b.quads).sum();
        self.phase_done("parse");
        self.report(&format!(
            "merging {} partial vocabularies ({} input quads)",
            batches.len(),
            total_in
        ));

        // ---- 2. vocabulary merge -------------------------------------------------
        self.interrupted()?;
        // each merging thread reads every batch's vocabulary at once: as many threads as
        // the open-file limit allows, down to one, which needs as many files as a merge
        // in one thread
        let files = |t: usize| t as u64 * batches.len() as u64 + 256;
        let mut threads = self.opts.threads.max(1);
        while threads > 1 && crate::disk::ensure_open_files(files(threads)).is_err() {
            threads -= 1;
        }
        crate::disk::ensure_open_files(files(threads))?;
        let parts: Vec<vocabmerge::Partial> = batches
            .iter_mut()
            .map(|b| vocabmerge::Partial {
                voc: self.tmp.join(format!("b{}.voc", b.id)),
                keys: b.keys,
                bytes: b.voc_bytes,
                samples: std::mem::take(&mut b.samples),
            })
            .collect();
        let (terms, maps) = vocabmerge::merge(
            &self.dir,
            &self.tmp,
            &parts,
            threads,
            &self.tmp_bytes,
            &|| self.interrupted(),
        )?;
        for p in parts {
            std::fs::remove_file(p.voc)?;
        }
        self.phase_done("vocabulary merge");
        self.report(&format!("vocabulary: {terms} terms"));

        // ---- 3. remap and sort; 4. permutations ------------------------------------
        let vocab = Vocab::open(&self.dir)?;
        let rdf_type = vocab
            .find(&id::iri_key(oxrdf::vocab::rdf::TYPE.as_str()))
            .ok()
            .map(|i| Id::vocab(i).0);
        drop(vocab);
        let plan = Plan::new(self.named_graphs.load(Ordering::Relaxed));
        let mut built = if total_in as usize <= self.opts.sort_mem_quads {
            let built = self.build_in_memory(&batches, maps, &plan, rdf_type)?;
            self.phase_done("sort and permutations");
            built
        } else {
            let runs = self.sorted_runs(&batches, maps, &plan)?;
            self.phase_done("chunk sorts");
            let built = self.merge_runs(runs, &plan, rdf_type)?;
            self.phase_done("permutations");
            built
        };
        // statistics in Perm::ALL order: POS adds to the predicates PSO found
        built.sort_by_key(|(p, _, _)| p.index());
        let mut stats = Stats::default();
        let mut rows = 0;
        for (perm, col, n) in built {
            if let Some(col) = col {
                col.finish(&mut stats);
            }
            if perm == Perm::Spo {
                rows = n;
            } else if n != rows {
                return Err(Error::Corrupt(format!(
                    "permutation {} has {n} rows, spo {rows}",
                    perm.name()
                )));
            }
        }
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
        {
            let mut phases = self.phases.lock();
            phases.0.end("statistics", &self.tmp_bytes);
            phases.1.end("total", &self.tmp_bytes);
        }
        let index_bytes: u64 = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok()?.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum();
        tracing::info!(
            target: "sparkles::builder",
            "index files: {:.1} MB",
            index_bytes as f64 / 1e6
        );
        self.report(&format!("index complete: {rows} quads, {terms} terms"));
        Ok(meta)
    }

    /// Read the batches of `chunk` into `buf`, remapped to global ids by `maps`, and
    /// delete them.
    fn load_chunk(
        &self,
        chunk: &[BatchInfo],
        maps: &vocabmerge::Maps,
        buf: &mut Vec<Key>,
    ) -> Result<()> {
        let n: usize = chunk.iter().map(|b| b.quads as usize).sum();
        buf.clear();
        buf.resize(n, [0; 4]);
        let mut parts = Vec::with_capacity(chunk.len());
        let mut rest = buf.as_mut_slice();
        for b in chunk {
            let (part, r) = rest.split_at_mut(b.quads as usize);
            parts.push(part);
            rest = r;
        }
        chunk
            .par_iter()
            .zip(parts)
            .try_for_each(|(b, out)| -> Result<()> {
                let qp = self.tmp.join(format!("b{}.q", b.id));
                let map = maps.read(b.pos, b.keys)?;
                let bad = || Error::Corrupt(format!("batch file {}", qp.display()));
                let mut r = runs::RunReader::open(&qp)?;
                let mut at = 0;
                while let Some(block) = r.next_block()? {
                    let dst = out.get_mut(at..at + block.len()).ok_or_else(bad)?;
                    for (q, k) in dst.iter_mut().zip(&block) {
                        for (v, &x) in q.iter_mut().zip(k) {
                            *v = if Id(x).tag() == Tag::Local {
                                let g = map.get(Id(x).payload() as usize).ok_or_else(bad)?;
                                Id::vocab(*g).0
                            } else {
                                x
                            };
                        }
                    }
                    at += block.len();
                }
                if at != out.len() {
                    return Err(bad());
                }
                std::fs::remove_file(&qp)?;
                Ok(())
            })
    }

    /// Sort `buf`, keys in `from` order, in place in the order of `to`.
    fn sort_as(buf: &mut [Key], from: Perm, to: Perm) {
        if from != to {
            buf.par_iter_mut()
                .with_min_len(1 << 14)
                .for_each(|k| *k = reorder(from, to, k));
        }
        buf.par_sort_unstable();
    }

    /// Phase 3 for input larger than the sort budget: per chunk of batches, one sorted
    /// run per order of the plan. Returns the run files of each order.
    fn sorted_runs(
        &self,
        batches: &[BatchInfo],
        maps: vocabmerge::Maps,
        plan: &Plan,
    ) -> Result<Vec<Vec<PathBuf>>> {
        let budget = self.opts.sort_mem_quads.max(1) as u64;
        let mut chunks: Vec<&[BatchInfo]> = Vec::new();
        let mut start = 0;
        let mut size = 0;
        for (i, b) in batches.iter().enumerate() {
            if i > start && size + b.quads > budget {
                chunks.push(&batches[start..i]);
                start = i;
                size = 0;
            }
            size += b.quads;
        }
        if start < batches.len() {
            chunks.push(&batches[start..]);
        }
        let mut runs: Vec<Vec<PathBuf>> = vec![Vec::new(); plan.orders.len()];
        let mut buf: Vec<Key> = Vec::new();
        for (c, chunk) in chunks.iter().enumerate() {
            self.interrupted()?;
            self.report(&format!(
                "sorting chunk {} of {} ({} quads)",
                c + 1,
                chunks.len(),
                chunk.iter().map(|b| b.quads).sum::<u64>()
            ));
            self.load_chunk(chunk, &maps, &mut buf)?;
            let mut cur = Perm::Spo;
            for (o, (first, _)) in plan.orders.iter().enumerate() {
                Self::sort_as(&mut buf, cur, *first);
                cur = *first;
                if o == 0 {
                    buf.dedup();
                }
                let p = self.tmp.join(format!("run-{}-{c}", first.name()));
                let n = runs::write_run(&p, &buf)?;
                self.tmp_bytes.add(iostat::Tmp::Runs, n);
                runs[o].push(p);
            }
        }
        maps.remove()?;
        Ok(runs)
    }

    /// Phase 4 for input larger than the sort budget: the runs of each order are merged
    /// in a thread of their own.
    fn merge_runs(
        &self,
        runs: Vec<Vec<PathBuf>>,
        plan: &Plan,
        rdf_type: Option<u64>,
    ) -> Result<Vec<Built>> {
        self.interrupted()?;
        self.report(&format!("building permutations {}", plan.names()));
        let results: Vec<Result<Vec<Built>>> = std::thread::scope(|s| {
            let handles: Vec<_> = plan
                .orders
                .iter()
                .zip(runs)
                .map(|((first, derived), files)| {
                    s.spawn(move || {
                        let out = std::thread::scope(|rs| {
                            let mut cursors: Vec<runs::Cursor> = files
                                .iter()
                                .map(|p| runs::Cursor::threaded(rs, p.clone()))
                                .collect();
                            self.build_perms(*first, derived, rdf_type, |emit| {
                                runs::merge(&mut cursors, emit)
                            })
                        });
                        for p in &files {
                            let _ = std::fs::remove_file(p);
                        }
                        out
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err(panicked())))
                .collect()
        });
        let mut out = Vec::new();
        for r in results {
            out.extend(r?);
        }
        Ok(out)
    }

    /// Phases 3 and 4 for input that fits into the sort budget: one chunk, sorted in
    /// memory per order.
    fn build_in_memory(
        &self,
        batches: &[BatchInfo],
        maps: vocabmerge::Maps,
        plan: &Plan,
        rdf_type: Option<u64>,
    ) -> Result<Vec<Built>> {
        let mut buf: Vec<Key> = Vec::new();
        self.load_chunk(batches, &maps, &mut buf)?;
        maps.remove()?;
        let mut cur = Perm::Spo;
        let mut out = Vec::new();
        for (first, derived) in &plan.orders {
            self.interrupted()?;
            let names: Vec<&str> = std::iter::once(first)
                .chain(derived)
                .map(|p| p.name())
                .collect();
            self.report(&format!("building permutations {}", names.join(", ")));
            Self::sort_as(&mut buf, cur, *first);
            cur = *first;
            out.extend(self.build_perms(*first, derived, rdf_type, |emit| {
                buf.iter().try_for_each(|&k| emit(k))
            })?);
        }
        Ok(out)
    }

    /// Stream the sorted keys that `feed` produces in `first` order into the permutation
    /// `first` and those `derived` from it, each written by a thread of its own. Repeated
    /// keys are dropped before every consumer, so statistics match the index.
    fn build_perms(
        &self,
        first: Perm,
        derived: &[Perm],
        rdf_type: Option<u64>,
        feed: impl FnOnce(&mut dyn FnMut(Key) -> Result<()>) -> Result<()>,
    ) -> Result<Vec<Built>> {
        const BATCH: usize = 1 << 16;
        let dir = &self.dir;
        std::thread::scope(|s| {
            let mut senders = Vec::new();
            let mut handles = Vec::new();
            for &perm in std::iter::once(&first).chain(derived) {
                let (tx, rx) = std::sync::mpsc::sync_channel::<Arc<Vec<Key>>>(4);
                senders.push(tx);
                let regroup = (perm != first && perm.order()[0] == first.order()[0]).then(|| {
                    runs::Regroup::new(&self.tmp, perm.name(), self.opts.sort_mem_quads / 4)
                });
                handles.push(s.spawn(move || -> Result<Built> {
                    let mut w = PermWriter::create(dir, perm)?;
                    let mut col = has_stats(perm).then(|| StatsCollector::new(perm, rdf_type));
                    let mut put = |k: Key| -> Result<()> {
                        if let Some(c) = &mut col {
                            c.push(&k);
                        }
                        w.push(k)
                    };
                    match regroup {
                        Some(mut rg) => {
                            for batch in rx {
                                for k in batch.iter() {
                                    rg.push(reorder(first, perm, k), &mut put)?;
                                }
                            }
                            rg.finish(&mut put)?;
                            self.tmp_bytes.add(iostat::Tmp::Spills, rg.spilled_bytes);
                        }
                        // the same order behind a constant column: GSPO from SPO in one graph
                        None => {
                            for batch in rx {
                                for k in batch.iter() {
                                    put(reorder(first, perm, k))?;
                                }
                            }
                        }
                    }
                    let rows = w.finish(dir, perm)?;
                    Ok((perm, col, rows))
                }));
            }
            let mut batch: Vec<Key> = Vec::with_capacity(BATCH);
            let mut last: Option<Key> = None;
            let send = |batch: &mut Vec<Key>| -> Result<()> {
                self.interrupted()?;
                let b = Arc::new(std::mem::replace(batch, Vec::with_capacity(BATCH)));
                for tx in &senders {
                    // a writer stopped: its error is reported below
                    tx.send(b.clone()).map_err(|_| Error::Cancelled)?;
                }
                Ok(())
            };
            let fed = feed(&mut |k| {
                if last == Some(k) {
                    return Ok(());
                }
                last = Some(k);
                batch.push(k);
                if batch.len() == BATCH {
                    send(&mut batch)?;
                }
                Ok(())
            })
            .and_then(|()| {
                if batch.is_empty() {
                    Ok(())
                } else {
                    send(&mut batch)
                }
            });
            drop(senders);
            let mut built = Vec::new();
            let mut err = None;
            for h in handles {
                match h.join().unwrap_or_else(|_| Err(panicked())) {
                    Ok(b) => built.push(b),
                    Err(e) => {
                        err.get_or_insert(e);
                    }
                }
            }
            match (err, fed) {
                (Some(e), _) | (None, Err(e)) => Err(e),
                (None, Ok(())) => Ok(built),
            }
        })
    }
}

/// A permutation written: its statistics (if it gathers any) and its rows.
type Built = (Perm, Option<StatsCollector>, u64);

fn panicked() -> Error {
    Error::Corrupt("a thread of the index build panicked".into())
}

/// Key `k` of permutation `from` as a key of `to`.
#[inline]
fn reorder(from: Perm, to: Perm, k: &Key) -> Key {
    to.to_key(&from.to_quad(k))
}

/// Whether the statistics read anything from permutation `perm`.
fn has_stats(perm: Perm) -> bool {
    matches!(
        perm,
        Perm::Spo | Perm::Osp | Perm::Pso | Perm::Pos | Perm::Gspo
    )
}

/// The orders the build sorts, each with the permutations produced from its stream.
struct Plan {
    orders: Vec<(Perm, Vec<Perm>)>,
}

impl Plan {
    fn new(named_graphs: bool) -> Plan {
        let mut orders = vec![
            (Perm::Spo, vec![Perm::Sop]),
            (Perm::Osp, vec![Perm::Ops]),
            (Perm::Pso, vec![Perm::Pos]),
        ];
        if named_graphs {
            orders.push((Perm::Gspo, Vec::new()));
        } else {
            orders[0].1.push(Perm::Gspo);
        }
        Plan { orders }
    }

    fn names(&self) -> String {
        let names: Vec<&str> = self
            .orders
            .iter()
            .flat_map(|(f, d)| std::iter::once(f).chain(d))
            .map(|p| p.name())
            .collect();
        names.join(", ")
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
    /// the last subject and predicate IRIs and their batch-local ids: N-Triples files
    /// usually repeat them on consecutive lines
    last: [(String, u64); 2],
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
                if cdt::may_name_bnodes(l) {
                    return self.cdt_literal(t);
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
                let relabeled = self.relabel(t);
                let t = relabeled.as_ref().unwrap_or(t);
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

    /// `t` with the blank node labels inside its composite literals (`cdt:List`,
    /// `cdt:Map`) replaced by the labels of the nodes this source's labels name, or
    /// `None` when it holds no such literal.
    fn relabel(&self, t: &Term) -> Option<Term> {
        let (scope, next) = (&self.scope, &self.b.next_bnode);
        cdt::relabel_term(t, &mut |b| id::bnode_label(scope.get(b, next)))
    }

    /// The id of a composite literal that may name blank nodes (see [`Self::relabel`]).
    #[cold]
    fn cdt_literal(&mut self, t: &Term) -> u64 {
        let relabeled = self.relabel(t);
        let Term::Literal(l) = relabeled.as_ref().unwrap_or(t) else {
            unreachable!("a literal stays a literal")
        };
        let mut kb = std::mem::take(&mut self.keybuf);
        kb.clear();
        id::write_literal_key(l, &mut kb);
        let r = self.local(&kb);
        self.keybuf = kb;
        r
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

    /// [`Self::iri`] for the subject (`slot` 0) or predicate (1), with the last one kept.
    #[inline]
    fn iri_cached(&mut self, iri: &str, slot: usize) -> u64 {
        let (last, id) = &self.last[slot];
        if *id != 0 && last == iri {
            return *id;
        }
        let id = self.iri(iri);
        let (last, last_id) = &mut self.last[slot];
        last.clear();
        last.push_str(iri);
        *last_id = id;
        id
    }

    pub fn push_quad(&mut self, q: &Quad) -> Result<()> {
        let s = match &q.subject {
            NamedOrBlankNode::NamedNode(n) => self.iri_cached(n.as_str(), 0),
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
        let p = self.iri_cached(q.predicate.as_str(), 1);
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
        // the next batch is about as large as this one: no rehashing on the way
        let (nk, nq) = (self.keys.len(), self.quads.len());
        let keys = std::mem::replace(
            &mut self.keys,
            FxHashMap::with_capacity_and_hasher(nk, Default::default()),
        );
        let quads = std::mem::replace(&mut self.quads, Vec::with_capacity(nq));
        self.key_bytes = 0;
        // batch-local ids start over
        for (_, id) in &mut self.last {
            *id = 0;
        }
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
    /// whether `out` is kept
    runs: bool,
    // per col0 run
    run_count: u64,
    run_distinct1: u64,
    out: Vec<(u64, u64, u64)>, // (col0, count, distinct col1)
    // rdf:type classes in POS: (class, distinct subjects)
    classes: Vec<(u64, u64)>,
    class_cur: Option<(u64, u64, u64)>, // (class, n, last subject)
    // characteristic sets in SPO: the current subject's (predicate, triples), and per
    // set of predicates (subjects, triples per predicate)
    cs_cur: Vec<(u64, u64)>,
    cs_classes: Vec<(u64, u64)>,
    cs_preds: Vec<u64>,
    charsets: FxHashMap<Box<[u64]>, (u64, Vec<u64>)>,
    cs_others: u64,
}

impl StatsCollector {
    fn new(perm: Perm, rdf_type: Option<u64>) -> Self {
        StatsCollector {
            perm,
            rdf_type,
            prev: None,
            distinct0: 0,
            runs: matches!(perm, Perm::Pso | Perm::Pos | Perm::Gspo),
            run_count: 0,
            run_distinct1: 0,
            out: Vec::new(),
            classes: Vec::new(),
            class_cur: None,
            cs_cur: Vec::new(),
            cs_classes: Vec::new(),
            cs_preds: Vec::new(),
            charsets: FxHashMap::default(),
            cs_others: 0,
        }
    }

    /// Count the finished subject's characteristic set.
    fn charset_done(&mut self) {
        if self.cs_cur.is_empty() {
            return;
        }
        // class items sort after every predicate
        self.cs_cur.append(&mut self.cs_classes);
        self.cs_preds.clear();
        self.cs_preds.extend(self.cs_cur.iter().map(|&(p, _)| p));
        let room = self.charsets.len() < MAX_CHARSETS_SEEN;
        match self.charsets.get_mut(&self.cs_preds[..]) {
            Some((n, t)) => {
                *n += 1;
                for (x, &(_, c)) in t.iter_mut().zip(&self.cs_cur) {
                    *x += c;
                }
            }
            None if room => {
                self.charsets.insert(
                    self.cs_preds.clone().into_boxed_slice(),
                    (1, self.cs_cur.iter().map(|&(_, c)| c).collect()),
                );
            }
            None => self.cs_others += 1,
        }
        self.cs_cur.clear();
    }

    #[inline]
    fn push(&mut self, k: &Key) {
        let new0 = self.prev.is_none_or(|p| p[0] != k[0]);
        if new0 {
            // only the predicate and graph statistics read the runs: SPO and OSP would
            // keep one per subject or object
            if let Some(p) = self.prev
                && self.runs
            {
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
        if self.perm == Perm::Spo {
            // key = (subject, predicate, object, graph)
            if new0 {
                self.charset_done();
            }
            match self.cs_cur.last_mut() {
                Some((p, c)) if *p == k[1] => *c += 1,
                _ => self.cs_cur.push((k[1], 1)),
            }
            if Some(k[1]) == self.rdf_type
                && let Some(item) = class_item(k[2])
            {
                match self.cs_classes.last_mut() {
                    Some((x, c)) if *x == item => *c += 1,
                    _ => self.cs_classes.push((item, 1)),
                }
            }
        }
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
        if let Some(p) = self.prev
            && self.runs
        {
            self.out.push((p[0], self.run_count, self.run_distinct1));
        }
        if let Some((c, n, _)) = self.class_cur.take() {
            self.classes.push((c, n));
        }
        self.charset_done();
        match self.perm {
            Perm::Spo => {
                stats.distinct_subjects = self.distinct0;
                let mut sets: Vec<CharSet> = std::mem::take(&mut self.charsets)
                    .into_iter()
                    .map(|(preds, (subjects, triples))| CharSet {
                        preds: preds.into_vec(),
                        subjects,
                        triples,
                    })
                    .collect();
                sets.sort_unstable_by(|a, b| {
                    b.subjects
                        .cmp(&a.subjects)
                        .then_with(|| a.preds.cmp(&b.preds))
                });
                let others: u64 = sets.iter().skip(MAX_CHARSETS).map(|c| c.subjects).sum();
                sets.truncate(MAX_CHARSETS);
                stats.charsets = sets;
                stats.charset_others = self.cs_others + others;
            }
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

    /// Every permutation holds each distinct quad once, in its own order, whether it is
    /// sorted from runs or produced from its partner's stream (with spilled first-column
    /// runs), with and without named graphs.
    #[test]
    fn permutations_hold_every_quad_in_order() {
        let mut nq = String::new();
        for rep in 0..2 {
            for i in 0..300 {
                let g = match (i % 4, rep) {
                    (0, _) => String::new(),
                    (k, _) => format!("<http://ex.org/g{k}>"),
                };
                nq.push_str(&format!(
                    "<http://ex.org/s{}> <http://ex.org/p{}> \"o{}\" {g} .\n",
                    i % 23,
                    i % 3,
                    i % 41
                ));
            }
        }
        let default_only: String = nq
            .lines()
            .filter(|l| !l.contains("/g"))
            .map(|l| format!("{l}\n"))
            .collect();
        for data in [&nq, &default_only] {
            for opts in [
                BuildOptions {
                    batch_quads: 13,
                    sort_mem_quads: 40,
                    threads: 2,
                    ..Default::default()
                },
                BuildOptions::default(),
            ] {
                let dir = tempfile::tempdir().unwrap();
                let b = Builder::new(dir.path(), opts).unwrap();
                b.add_source(&Source::from_bytes(
                    data.as_bytes().to_vec(),
                    RdfFormat::NQuads,
                    None,
                ))
                .unwrap();
                b.finish().unwrap();
                let cache = BlockCache::new(1 << 20);
                let read = |p: Perm| {
                    let idx = PermIndex::open(dir.path(), p).unwrap();
                    let mut keys = Vec::new();
                    idx.for_each_range(&cache, &[], |b, s, e| {
                        keys.extend((s..e).map(|i| b.key(i)));
                        Ok(())
                    })
                    .unwrap();
                    keys
                };
                let quads: Vec<[Id; 4]> = read(Perm::Spo)
                    .iter()
                    .map(|k| Perm::Spo.to_quad(k))
                    .collect();
                let distinct: std::collections::HashSet<_> = data.lines().collect();
                assert_eq!(quads.len(), distinct.len());
                assert!(std::fs::read_dir(dir.path().join("tmp")).is_err());
                for p in Perm::ALL {
                    let mut want: Vec<Key> = quads.iter().map(|q| p.to_key(q)).collect();
                    want.sort_unstable();
                    assert_eq!(read(p), want, "{}", p.name());
                }
            }
        }
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
