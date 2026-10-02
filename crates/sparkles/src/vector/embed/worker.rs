//! The embedding state of a store: per index, the queue of (subject, graph) pairs to
//! reconcile, the pending full pass, the record of what was embedded and the counters
//! of the status. The store's code (`store/embed.rs`) reads and writes it.

use super::client::{self, CallError, Waits};
use super::config::EmbeddingConfig;
use super::{EmbeddingBatch, EmbeddingError, Environment, Fnv};
use crate::id::{Id, Tag};
use crate::store::Snapshot;
use parking_lot::{Condvar, Mutex};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// A subject or graph by a key that survives compactions: its vocabulary key, or the
/// raw id of an inline term (blank nodes, the default graph).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Node {
    Key(Box<[u8]>),
    Inline(u64),
}

impl Node {
    pub fn of(snap: &Snapshot, id: Id) -> Option<Node> {
        match id.tag() {
            Tag::Vocab | Tag::Delta => snap.key(id).map(|k| Node::Key(k.into())),
            _ => Some(Node::Inline(id.0)),
        }
    }

    /// The id in `snap` (`None`: the term is not in it).
    pub fn id(&self, snap: &Snapshot) -> Option<Id> {
        match self {
            Node::Key(k) => snap.lookup_key(k),
            Node::Inline(r) => Some(Id(*r)),
        }
    }

    fn hash_into(&self, h: &mut Fnv) {
        match self {
            Node::Key(k) => {
                h.write(&[1]);
                h.write(&(k.len() as u64).to_le_bytes());
                h.write(k);
            }
            Node::Inline(r) => {
                h.write(&[2]);
                h.write(&r.to_le_bytes());
            }
        }
    }
}

/// A subject and the graph of its text and vectors.
pub(crate) type Pair = (Node, Node);

/// The record key of a pair.
pub(crate) fn pair_hash(p: &Pair) -> u64 {
    let mut h = Fnv::default();
    p.1.hash_into(&mut h);
    p.0.hash_into(&mut h);
    h.0.max(1)
}

/// The record value of a pair's (sorted, distinct) inputs; never 0, which marks a
/// removed pair.
pub(crate) fn inputs_hash(inputs: &[String]) -> u64 {
    let mut h = Fnv::default();
    h.write(&(inputs.len() as u64).to_le_bytes());
    for i in inputs {
        h.write(&(i.len() as u64).to_le_bytes());
        h.write(i.as_bytes());
    }
    h.0.max(1)
}

const MAGIC: &[u8; 8] = b"SPKEMB1\n";
const HEADER: usize = 16;
const REC: usize = 16;

/// The record of reconciled pairs: pair hash → inputs hash, as of the last
/// reconciliation. A persistent store appends it to `embed/<name>.log`; the file's
/// header holds the provider identity, and a different one starts it over.
pub(crate) struct Record {
    map: FxHashMap<u64, u64>,
    path: Option<PathBuf>,
    out: Option<std::io::BufWriter<std::fs::File>>,
    /// records in the file
    written: u64,
    identity: u64,
}

impl Record {
    pub fn open(dir: Option<&Path>, name: &str, identity: u64) -> Record {
        let mut r = Record {
            map: FxHashMap::default(),
            path: dir.map(|d| d.join(format!("{name}.log"))),
            out: None,
            written: 0,
            identity,
        };
        if let Some(p) = r.path.clone() {
            match std::fs::File::open(&p) {
                Ok(mut f) => {
                    let mut b = Vec::new();
                    if f.read_to_end(&mut b).is_ok()
                        && b.len() >= HEADER
                        && &b[..8] == MAGIC
                        && u64::from_le_bytes(b[8..16].try_into().unwrap()) == identity
                    {
                        for c in b[HEADER..].as_chunks::<REC>().0 {
                            let k = u64::from_le_bytes(c[..8].try_into().unwrap());
                            let v = u64::from_le_bytes(c[8..].try_into().unwrap());
                            if v == 0 {
                                r.map.remove(&k);
                            } else {
                                r.map.insert(k, v);
                            }
                            r.written += 1;
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("embedding record {}: {e}", p.display()),
            }
            // a damaged or foreign file is written anew
            r.rewrite();
        }
        r
    }

    pub fn get(&self, k: u64) -> Option<u64> {
        self.map.get(&k).copied()
    }

    /// Record a pair (`v == 0`: removed).
    pub fn set(&mut self, k: u64, v: u64) {
        let changed = if v == 0 {
            self.map.remove(&k).is_some()
        } else {
            self.map.insert(k, v) != Some(v)
        };
        if !changed {
            return;
        }
        if let Some(out) = self.out.as_mut() {
            let mut rec = [0u8; REC];
            rec[..8].copy_from_slice(&k.to_le_bytes());
            rec[8..].copy_from_slice(&v.to_le_bytes());
            if let Err(e) = out.write_all(&rec) {
                tracing::warn!("embedding record: {e}");
                self.out = None;
            }
            self.written += 1;
        }
    }

    /// Flush the records written so far, and rewrite the file once it holds more than
    /// twice as many records as pairs.
    pub fn flush(&mut self) {
        if let Some(out) = self.out.as_mut()
            && let Err(e) = out.flush()
        {
            tracing::warn!("embedding record: {e}");
            self.out = None;
        }
        if self.written > 2 * self.map.len() as u64 + 1024 {
            self.rewrite();
        }
    }

    /// Forget every pair (`reembed`, a new identity).
    pub fn clear(&mut self, identity: u64) {
        self.map.clear();
        self.identity = identity;
        self.rewrite();
    }

    fn rewrite(&mut self) {
        let Some(p) = self.path.clone() else {
            return;
        };
        self.out = None;
        let write = || -> std::io::Result<std::fs::File> {
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
            let tmp = p.with_extension("log.tmp");
            let mut b = Vec::with_capacity(HEADER + self.map.len() * REC);
            b.extend_from_slice(MAGIC);
            b.extend_from_slice(&self.identity.to_le_bytes());
            for (k, v) in &self.map {
                b.extend_from_slice(&k.to_le_bytes());
                b.extend_from_slice(&v.to_le_bytes());
            }
            std::fs::write(&tmp, &b)?;
            std::fs::rename(&tmp, &p)?;
            std::fs::OpenOptions::new().append(true).open(&p)
        };
        match write() {
            Ok(f) => {
                self.out = Some(std::io::BufWriter::new(f));
                self.written = self.map.len() as u64;
            }
            Err(e) => tracing::warn!("embedding record {}: {e}", p.display()),
        }
    }

    pub fn remove_file(&mut self) {
        self.out = None;
        if let Some(p) = &self.path {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// A full pass over every pair of an index.
pub(crate) struct Pass {
    /// the commit that asked for it: pairs it schedules count from there
    pub seq: u64,
    /// not before (passes of query sources are spaced)
    pub not_before: Instant,
    /// the pairs to look at, read from `snap` when the pass started
    pub running: Option<RunningPass>,
}

pub(crate) struct RunningPass {
    pub snap: Arc<Snapshot>,
    pub pairs: Vec<(u64, u64)>,
    pub pos: usize,
    /// the inputs a query source gave each pair
    pub query_inputs: Option<FxHashMap<(u64, u64), Vec<String>>>,
}

/// The counters of the status.
#[derive(Default)]
pub(crate) struct Stats {
    pub embedded: u64,
    pub requests: u64,
    pub failed: u64,
    pub last_error: Option<EmbeddingError>,
    pub last_batch: Option<EmbeddingBatch>,
}

/// The embedding state of one index.
pub(crate) struct Work {
    pub name: String,
    pub emb: EmbeddingConfig,
    pub predicate: String,
    pub dimension: usize,
    /// +1 whenever the configuration is replaced: a batch of another epoch is dropped
    pub epoch: u64,
    pub queue: VecDeque<(Pair, u64)>,
    pub queued: FxHashSet<Pair>,
    /// the seq of the oldest pair of the batch out, if one is
    pub in_flight: Option<u64>,
    pub pass: Option<Pass>,
    pub last_pass: Option<Instant>,
    pub record: Record,
    /// pairs that failed for good, with the inputs that failed: skipped until those
    /// change or a full pass runs
    pub failed: FxHashMap<u64, u64>,
    pub stats: Stats,
    pub retry_at: Option<(Instant, i64)>,
    pub next_request: Option<Instant>,
}

impl Work {
    pub fn new(
        name: &str,
        predicate: &str,
        dimension: usize,
        emb: EmbeddingConfig,
        dir: Option<&Path>,
        epoch: u64,
        seq: u64,
    ) -> Work {
        let identity = emb.identity(dimension);
        Work {
            name: name.into(),
            predicate: predicate.into(),
            dimension,
            record: Record::open(dir, name, identity),
            emb,
            epoch,
            queue: VecDeque::new(),
            queued: FxHashSet::default(),
            in_flight: None,
            pass: Some(Pass {
                seq,
                not_before: Instant::now(),
                running: None,
            }),
            last_pass: None,
            failed: FxHashMap::default(),
            stats: Stats::default(),
            retry_at: None,
            next_request: None,
        }
    }

    /// Schedule a pair, dirtied by commit `seq`.
    pub fn push(&mut self, p: Pair, seq: u64) {
        if self.queued.insert(p.clone()) {
            self.queue.push_back((p, seq));
        }
    }

    /// Schedule a full pass for commit `seq` (spaced by `gap` from the last one).
    pub fn schedule_pass(&mut self, seq: u64, gap: Duration) {
        match &mut self.pass {
            // a pass that has not started yet covers this commit too
            Some(p) if p.running.is_none() => p.seq = p.seq.min(seq),
            _ => {
                let not_before = self
                    .last_pass
                    .map_or_else(Instant::now, |t| (t + gap).max(Instant::now()));
                // a running pass read an older state: another one follows it
                self.pass = Some(Pass {
                    seq,
                    not_before,
                    running: None,
                });
            }
        }
    }

    /// The newest commit whose pairs are all reconciled.
    pub fn applied(&self, head: u64) -> u64 {
        let mut oldest = self.queue.front().map(|q| q.1);
        for s in [self.in_flight, self.pass.as_ref().map(|p| p.seq)]
            .into_iter()
            .flatten()
        {
            oldest = Some(oldest.map_or(s, |o| o.min(s)));
        }
        oldest.map_or(head, |s| s.saturating_sub(1).min(head))
    }

    pub fn error(&mut self, message: String, subject: Option<String>) {
        tracing::warn!(target: "sparkles::embed", "vector index {}: {message}", self.name);
        self.stats.last_error = Some(EmbeddingError {
            at: crate::commit::rfc3339_ms(crate::commit::now_ms()),
            message,
            subject,
        });
    }
}

/// The embedding state of a store, shared with its worker thread.
#[derive(Default)]
pub struct Embedder {
    pub(crate) works: Mutex<std::collections::BTreeMap<String, Work>>,
    cv: Condvar,
    /// guards the condition variable's waits
    signal: Mutex<bool>,
    closed: AtomicBool,
    /// worker threads running
    attached: AtomicUsize,
    /// overrides the process's environment
    pub(crate) env: parking_lot::RwLock<Option<Arc<Environment>>>,
    /// vectors of inputs and query texts by (identity, input)
    pub(crate) cache: QueryCache,
    /// round robin over the indexes
    pub(crate) next: AtomicUsize,
    /// a worker thread was started for this store (by the server)
    pub(crate) spawned: AtomicBool,
}

/// Recently embedded inputs: (identity, input) → vector.
pub(crate) struct QueryCache(quick_cache::sync::Cache<(u64, String), Arc<[f32]>>);

impl Default for QueryCache {
    fn default() -> QueryCache {
        QueryCache(quick_cache::sync::Cache::new(4096))
    }
}

impl QueryCache {
    pub fn get(&self, identity: u64, input: &str) -> Option<Arc<[f32]>> {
        self.0.get(&(identity, input.to_string()))
    }
    pub fn insert(&self, identity: u64, input: String, v: Arc<[f32]>) {
        self.0.insert((identity, input), v);
    }
    pub fn clear(&self) {
        self.0.clear();
    }
}

/// Decrements the attached count when a worker ends.
pub struct Attached(Arc<Embedder>);

impl Drop for Attached {
    fn drop(&mut self) {
        self.0.attached.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Embedder {
    pub fn env(&self) -> Arc<Environment> {
        self.env.read().clone().unwrap_or_else(super::environment)
    }

    pub(crate) fn attach(self: &Arc<Self>) -> Attached {
        self.attached.fetch_add(1, Ordering::SeqCst);
        Attached(self.clone())
    }

    /// Whether a worker thread runs.
    pub fn attached(&self) -> bool {
        self.attached.load(Ordering::SeqCst) > 0
    }

    /// Claim the right to start this store's worker thread: `true` once.
    pub fn claim_spawn(&self) -> bool {
        !self.spawned.swap(true, Ordering::SeqCst)
    }

    /// Whether any index embeds.
    pub fn active(&self) -> bool {
        !self.works.lock().is_empty()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.notify();
    }

    /// Wake the worker: there is work.
    pub(crate) fn notify(&self) {
        *self.signal.lock() = true;
        self.cv.notify_all();
    }

    /// Wait for work, at most `d`.
    pub(crate) fn wait(&self, d: Duration) {
        let mut s = self.signal.lock();
        if !*s && !self.is_closed() {
            self.cv.wait_for(&mut s, d);
        }
        *s = false;
    }

    /// Sleep `d` (a retry's backoff); `false` when the store closes meanwhile.
    pub(crate) fn sleep(&self, d: Duration) -> bool {
        let end = Instant::now() + d;
        let mut s = self.signal.lock();
        while !self.is_closed() {
            let now = Instant::now();
            if now >= end {
                return true;
            }
            self.cv.wait_for(&mut s, end - now);
        }
        false
    }
}

/// One pair of a batch and its inputs when the batch was prepared.
pub(crate) struct Item {
    pub pair: Pair,
    pub seq: u64,
    pub inputs: Vec<String>,
    pub subject: String,
}

/// A batch to embed: the pairs, and the distinct inputs to send.
pub struct Batch {
    pub(crate) index: String,
    /// the index's predicate
    pub(crate) target: String,
    /// the commit whose state the inputs were read from
    pub(crate) prepared_at: u64,
    pub(crate) epoch: u64,
    pub(crate) emb: EmbeddingConfig,
    pub(crate) dimension: usize,
    pub(crate) env: Arc<Environment>,
    pub(crate) items: Vec<Item>,
    pub(crate) cache: Arc<Embedder>,
}

/// What [`Store::embed_prepare`](crate::store::Store::embed_prepare) found.
pub enum Prepared {
    /// a batch to send
    Batch(Box<Batch>),
    /// some work done (a part of a full pass); call again
    Progress,
    /// nothing to do
    Idle,
    /// nothing to do before this long (a backoff or rate limit)
    Wait(Duration),
}

/// The vector (or why there is none) of each distinct input of a batch.
pub(crate) type Vectors = FxHashMap<String, Result<Arc<[f32]>, String>>;

/// A batch after its request.
pub struct Embedded {
    pub(crate) batch: Batch,
    /// the vector (or failure) of each distinct input
    pub(crate) vectors: Result<Vectors, CallError>,
    pub(crate) requests: u64,
    pub(crate) ms: f64,
    pub(crate) sent: u64,
}

impl Batch {
    /// Ask the provider for the vectors of the batch's inputs (those not in the cache).
    /// `sleep` waits out retries and returns `false` when the store closes.
    pub fn run(self, sleep: &dyn Fn(Duration) -> bool) -> Embedded {
        let t0 = Instant::now();
        let identity = self.emb.identity(self.dimension);
        let mut vectors: FxHashMap<String, Result<Arc<[f32]>, String>> = FxHashMap::default();
        let mut todo: Vec<String> = Vec::new();
        for i in self.items.iter().flat_map(|i| i.inputs.iter()) {
            if vectors.contains_key(i) || todo.contains(i) {
                continue;
            }
            match self.cache.cache.get(identity, i) {
                Some(v) => {
                    vectors.insert(i.clone(), Ok(v));
                }
                None => todo.push(i.clone()),
            }
        }
        let mut requests = 0;
        let sent = todo.len() as u64;
        // at most `batchSize` inputs per request: a pair can have several
        let mut r = Ok(());
        for chunk in todo.chunks(self.emb.batch_size.max(1)) {
            match client::embed(
                &self.env,
                &self.emb,
                self.dimension,
                chunk,
                &Waits { sleep },
                &mut requests,
            ) {
                Ok(got) => {
                    for (input, v) in chunk.iter().zip(got) {
                        let v = v.map(Arc::<[f32]>::from);
                        if let Ok(v) = &v {
                            self.cache.cache.insert(identity, input.clone(), v.clone());
                        }
                        vectors.insert(input.clone(), v);
                    }
                }
                Err(e) => {
                    r = Err(e);
                    break;
                }
            }
        }
        let r = r.map(|()| vectors);
        Embedded {
            vectors: r,
            requests,
            ms: t0.elapsed().as_secs_f64() * 1000.0,
            sent,
            batch: self,
        }
    }
}
