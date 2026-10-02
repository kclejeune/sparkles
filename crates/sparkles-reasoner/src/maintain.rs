//! Where an incremental run finds the previous closure, and what it keeps for the next.
//!
//! The previous closure comes from memory when the caller keeps a [`Cache`] (the server
//! does), and otherwise from the dataset: the default graph and the inferred graph as of
//! the previous run, recovered from their state now and the commit diff since, plus the
//! derived facts that are not valid RDF. Those generalized facts (a literal subject, a
//! blank node predicate) are never written to the inferred graph but can take part in
//! derivations, so a persistent dataset keeps them in `reasoning-generalized.bin`, bound
//! to the run's commit and rules by `reasoning-generalized.json`.

use crate::INFERRED_GRAPH;
use crate::graph::{Graph, Triple};
use crate::incremental::{Closure, Fallback};
use crate::terms::Terms;
use oxrdf::{GraphName, NamedNode, Term};
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles::history::At;
use sparkles::id::Id;
use sparkles::index::Perm;
use sparkles::store::{Chunk, DiffOp, DiffOptions, Snapshot, Store};
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The closure of a dataset's last materialization, kept in memory for the next run.
///
/// A run that updates the closure takes it out and puts the new one back after its
/// commit. Only closures of at most `max_triples` facts are kept.
pub struct Cache {
    kept: Mutex<Option<Kept>>,
    max_triples: AtomicUsize,
}

/// Default of [`Cache::new`]'s limit.
pub const DEFAULT_CACHE_TRIPLES: usize = 20_000_000;

impl Default for Cache {
    fn default() -> Cache {
        Cache::new(DEFAULT_CACHE_TRIPLES)
    }
}

impl Cache {
    /// A cache that keeps closures of at most `max_triples` facts (0 keeps none).
    pub fn new(max_triples: usize) -> Cache {
        Cache {
            kept: Mutex::new(None),
            max_triples: AtomicUsize::new(max_triples),
        }
    }

    /// Change the limit (a kept closure over it is dropped at the next run).
    pub fn set_max_triples(&self, n: usize) {
        self.max_triples.store(n, Ordering::Relaxed);
    }

    /// Drop the kept closure.
    pub fn clear(&self) {
        *self.kept.lock() = None;
    }

    /// Facts in the kept closure, and the commit it belongs to.
    pub fn kept(&self) -> Option<(usize, u64)> {
        self.kept
            .lock()
            .as_ref()
            .map(|k| (k.closure.graph.live(), k.commit))
    }

    fn take(&self) -> Option<Kept> {
        self.kept.lock().take()
    }

    fn put(&self, k: Kept) {
        if k.closure.graph.live() <= self.max_triples.load(Ordering::Relaxed) {
            *self.kept.lock() = Some(k);
        }
    }
}

pub(crate) struct Kept {
    dataset_id: uuid::Uuid,
    /// the commit the closure is the materialization of
    commit: u64,
    digest: u64,
    /// the snapshot at `commit`
    snap: Arc<Snapshot>,
    closure: Closure,
    generalized: FxHashSet<Triple>,
    /// the generalized facts as saved with the dataset for `commit`
    saved: Option<SavedMeta>,
}

/// The default graph's and the inferred graph's changes between two commits, as ids of
/// the later one (or local ids for terms it lacks).
#[derive(Default)]
pub(crate) struct Changes {
    pub base_added: Vec<Triple>,
    pub base_removed: Vec<Triple>,
    pub inferred_added: Vec<Triple>,
    pub inferred_removed: Vec<Triple>,
}

/// The previous closure, ready for an update.
pub(crate) struct Previous {
    pub closure: Closure,
    pub generalized: FxHashSet<Triple>,
    pub changes: Changes,
    /// `memory` or `store`
    pub source: &'static str,
    /// the generalized facts as the dataset keeps them, when read from it
    pub saved: Option<SavedMeta>,
}

/// Find the closure of the materialization at commit `since`, and the changes from
/// there to `snap`.
/// The changes from commit `since` to `snap`, read before the writer lock is taken (the
/// commit diff takes it): from the commit diff, or from the deltas of the closure kept
/// for `since` when the diff cannot reach it (in-memory datasets keep no log). `Err`
/// says why neither can.
pub(crate) fn changes(
    store: &Store,
    since: u64,
    snap: &Arc<Snapshot>,
    cache: Option<&Cache>,
) -> Result<RawChanges, String> {
    let e = match logged_changes(store, since, snap.commit) {
        Ok(r) => return Ok(r),
        Err(e) => e,
    };
    let kept = cache.and_then(|c| {
        let k = c.kept.lock();
        k.as_ref()
            .filter(|k| {
                k.dataset_id == store.dataset_id()
                    && k.commit == since
                    && Arc::ptr_eq(&k.snap.generation, &snap.generation)
            })
            .map(|k| k.snap.clone())
    });
    match kept {
        Some(old) => delta_changes(&old, snap).map_err(|e| e.to_string()),
        None => Err(format!(
            "commit {since} cannot be compared with the head: {e:#}"
        )),
    }
}

/// Find the closure of the materialization at commit `since`, for an update with the
/// `raw` changes from there to `snap`.
pub(crate) fn previous(
    store: &Store,
    snap: &Arc<Snapshot>,
    since: u64,
    digest: u64,
    cache: Option<&Cache>,
    raw: Result<RawChanges, String>,
) -> anyhow::Result<Previous> {
    let dataset_id = store.dataset_id();
    let rules_changed = || Fallback("the rules changed since the previous run".into());
    let kept = cache.and_then(Cache::take).filter(|k| {
        k.dataset_id == dataset_id
            && k.commit == since
            && Arc::ptr_eq(&k.snap.generation, &snap.generation)
    });
    if kept.as_ref().is_some_and(|k| k.digest != digest) {
        return Err(rules_changed().into());
    }
    if let Some(k) = kept {
        let Kept {
            mut closure,
            mut generalized,
            saved,
            ..
        } = k;
        let raw = raw.map_err(Fallback)?;
        let moved = closure.terms.rebase(snap.clone());
        closure.remap(&moved);
        remap_set(&mut generalized, &moved);
        let changes = raw.resolve(&closure.terms);
        return Ok(Previous {
            closure,
            generalized,
            changes,
            source: "memory",
            saved,
        });
    }
    let Some(root) = store.root() else {
        return Err(Fallback("no closure of the previous run is kept in memory".into()).into());
    };
    let (meta, keys) = match read_saved(root, dataset_id, since, digest)? {
        Saved::Found(meta, keys) => (meta, keys),
        Saved::OtherRules => return Err(rules_changed().into()),
        Saved::Missing => {
            return Err(Fallback(format!("no closure state is saved for commit {since}")).into());
        }
    };
    let raw = raw.map_err(Fallback)?;
    let terms = Terms::new(snap.clone());
    let changes = raw.resolve(&terms);
    let generalized: FxHashSet<Triple> = keys
        .iter()
        .map(|k| {
            let id = |b: &[u8]| terms.id_for(&sparkles::id::key_to_term(b));
            [id(&k[0]), id(&k[1]), id(&k[2])]
        })
        .collect();
    let closure = reconstruct(snap, terms, &changes, &generalized)?;
    Ok(Previous {
        closure,
        generalized,
        changes,
        source: "store",
        saved: Some(meta),
    })
}

fn remap_set(s: &mut FxHashSet<Triple>, moved: &FxHashMap<u64, u64>) {
    if moved.is_empty() || !s.iter().flatten().any(|x| moved.contains_key(x)) {
        return;
    }
    let m = |x: u64| moved.get(&x).copied().unwrap_or(x);
    *s = s.iter().map(|t| [m(t[0]), m(t[1]), m(t[2])]).collect();
}

/// The closure as of the previous run: the default graph and the inferred graph then,
/// and the generalized facts.
fn reconstruct(
    snap: &Arc<Snapshot>,
    terms: Terms,
    ch: &Changes,
    generalized: &FxHashSet<Triple>,
) -> anyhow::Result<Closure> {
    let scan = |g: Id, skip: &FxHashSet<Triple>, out: &mut Vec<Triple>| -> anyhow::Result<()> {
        snap.scan(Perm::Gspo, &[g.0], |c| {
            match c {
                Chunk::Block(b, s, e) => out.extend(
                    (s..e)
                        .map(|i| [b.cols[1][i], b.cols[2][i], b.cols[3][i]])
                        .filter(|t| !skip.contains(t)),
                ),
                Chunk::Row(k) => {
                    let t = [k[1], k[2], k[3]];
                    if !skip.contains(&t) {
                        out.push(t);
                    }
                }
            }
            Ok(true)
        })?;
        Ok(())
    };
    let mut base = Vec::with_capacity(snap.count(Perm::Gspo, &[Id::DEFAULT_GRAPH.0])? as usize);
    scan(
        Id::DEFAULT_GRAPH,
        &ch.base_added.iter().copied().collect(),
        &mut base,
    )?;
    base.extend(&ch.base_removed);
    let mut derived = Vec::new();
    if let Some(g) = snap.lookup_iri(INFERRED_GRAPH) {
        scan(
            g,
            &ch.inferred_added.iter().copied().collect(),
            &mut derived,
        )?;
    }
    derived.extend(&ch.inferred_removed);
    derived.extend(generalized);
    let mut graph = Graph::with_capacity(base.len() + derived.len());
    graph.add_batch(base);
    let n = graph.len();
    graph.add_batch(derived);
    Ok(Closure::new(graph, terms, n))
}

/// Changes as the commit diff reports them, or as ids of one generation.
pub(crate) enum RawChanges {
    Quads {
        base: Vec<(DiffOp, oxrdf::Quad)>,
        inferred: Vec<(DiffOp, oxrdf::Quad)>,
    },
    Ids(Changes),
}

impl RawChanges {
    fn resolve(self, terms: &Terms) -> Changes {
        match self {
            RawChanges::Ids(c) => c,
            RawChanges::Quads { base, inferred } => {
                let id = |t: Term| terms.id_for(&t);
                let triple = |q: oxrdf::Quad| -> Triple {
                    [id(q.subject.into()), id(q.predicate.into()), id(q.object)]
                };
                let mut c = Changes::default();
                for (op, q) in base {
                    match op {
                        DiffOp::Add => c.base_added.push(triple(q)),
                        DiffOp::Remove => c.base_removed.push(triple(q)),
                    }
                }
                for (op, q) in inferred {
                    match op {
                        DiffOp::Add => c.inferred_added.push(triple(q)),
                        DiffOp::Remove => c.inferred_removed.push(triple(q)),
                    }
                }
                c
            }
        }
    }
}

/// The changes between two commits from the store's commit diff.
fn logged_changes(store: &Store, since: u64, head: u64) -> anyhow::Result<RawChanges> {
    if since == head {
        return Ok(RawChanges::Ids(Changes::default()));
    }
    let diff = |g: GraphName| -> anyhow::Result<Vec<(DiffOp, oxrdf::Quad)>> {
        let o = DiffOptions {
            graph: Some(g),
            ..Default::default()
        };
        let d = store.diff(&At::Commit(since), &At::Commit(head), &o)?;
        Ok(d.iter().collect())
    };
    Ok(RawChanges::Quads {
        base: diff(GraphName::DefaultGraph)?,
        inferred: diff(GraphName::NamedNode(NamedNode::new_unchecked(
            INFERRED_GRAPH,
        )))?,
    })
}

/// The changes between two snapshots of one generation, from their deltas.
fn delta_changes(old: &Snapshot, new: &Snapshot) -> anyhow::Result<RawChanges> {
    let i = Perm::Gspo.index();
    let inferred = new
        .lookup_iri(INFERRED_GRAPH)
        .or_else(|| old.lookup_iri(INFERRED_GRAPH));
    let mut c = Changes::default();
    let mut seen: FxHashSet<[u64; 4]> = FxHashSet::default();
    for set in [
        &old.delta.ins[i],
        &old.delta.del[i],
        &new.delta.ins[i],
        &new.delta.del[i],
    ] {
        for k in set.iter() {
            let base = k[0] == Id::DEFAULT_GRAPH.0;
            if !(base || inferred.is_some_and(|g| g.0 == k[0])) || !seen.insert(*k) {
                continue;
            }
            let q = Perm::Gspo.to_quad(k);
            let (a, b) = (old.contains(&q)?, new.contains(&q)?);
            let t = [q[0].0, q[1].0, q[2].0];
            match (a, b, base) {
                (false, true, true) => c.base_added.push(t),
                (true, false, true) => c.base_removed.push(t),
                (false, true, false) => c.inferred_added.push(t),
                (true, false, false) => c.inferred_removed.push(t),
                _ => {}
            }
        }
    }
    Ok(RawChanges::Ids(c))
}

/// Keep the closure after the run's commit: in the cache, while the commit kept the
/// generation (a commit that rebuilt it changed every id), with the local ids the commit
/// stored replaced by their store ids.
#[allow(clippy::too_many_arguments)]
pub(crate) fn keep(
    store: &Store,
    cache: Option<&Cache>,
    receipt: &sparkles::commit::Receipt,
    before: &Arc<Snapshot>,
    digest: u64,
    mut closure: Closure,
    mut generalized: FxHashSet<Triple>,
    stored: &FxHashMap<u64, Id>,
    saved: Option<SavedMeta>,
) {
    let Some(cache) = cache else { return };
    if closure.graph.live() > cache.max_triples.load(Ordering::Relaxed) {
        return;
    }
    let snap = if receipt.committed {
        store.snapshot()
    } else {
        before.clone()
    };
    if snap.commit != receipt.commit.seq || !Arc::ptr_eq(&snap.generation, &before.generation) {
        return;
    }
    let mut moved: FxHashMap<u64, u64> = stored.iter().map(|(l, id)| (*l, id.0)).collect();
    moved.extend(closure.terms.rebase(snap.clone()));
    closure.remap(&moved);
    remap_set(&mut generalized, &moved);
    closure.compact_if_needed();
    // the searches for other proofs look facts up by subject and by object: build those
    // indexes now, after the commit, rather than in the next run
    closure.graph.ensure_s_index();
    closure.graph.ensure_o_index();
    cache.put(Kept {
        dataset_id: receipt.dataset_id,
        commit: receipt.commit.seq,
        digest,
        snap,
        closure,
        generalized,
        saved,
    });
}

// ------------------------------------------------------------ saved state ----
//
// `reasoning-generalized.bin` holds the generalized facts of one run, and
// `reasoning-generalized.log` the facts later incremental runs added (`+`) and removed
// (`-`), so that a run that changes a few of them appends a few records. The `.json`
// binds both to the commit and the rules of the last run: the checksum of the `.bin`,
// and the length and checksum of the part of the log that belongs to it. The `.bin` is
// written again once the log is longer than the `.bin`.

const SAVED_BIN: &str = "reasoning-generalized.bin";
const SAVED_LOG: &str = "reasoning-generalized.log";
const SAVED_META: &str = "reasoning-generalized.json";
const MAGIC: &[u8] = b"SPKGEN1\n";

/// What `reasoning-generalized.json` records.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedMeta {
    dataset_id: String,
    commit: u64,
    /// the rules digest, hex
    rules: String,
    /// facts in the `.bin`
    triples: u64,
    /// size and FNV-1a of the `.bin`, hex
    bytes: u64,
    checksum: String,
    /// the length and FNV-1a of the log's records that belong to the `.bin`
    #[serde(default)]
    log_bytes: u64,
    #[serde(default)]
    log_checksum: String,
}

/// The generalized facts a run added and removed.
#[derive(Default)]
pub(crate) struct GeneralizedChanges {
    pub added: Vec<Triple>,
    pub removed: Vec<Triple>,
}

pub(crate) fn fnv(bytes: &[u8], mut h: u64) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

pub(crate) const FNV_START: u64 = 0xcbf2_9ce4_8422_2325;

type Keys = [Vec<u8>; 3];

enum Saved {
    Found(SavedMeta, Vec<Keys>),
    /// saved for the commit, with other rules
    OtherRules,
    Missing,
}

/// Reads length-prefixed keys.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let (a, b) = self.0.split_at_checked(n)?;
        self.0 = b;
        Some(a)
    }

    fn keys(&mut self) -> Option<Keys> {
        let mut t: Keys = Default::default();
        for k in &mut t {
            let len = u32::from_le_bytes(self.take(4)?.try_into().ok()?) as usize;
            *k = self.take(len)?.to_vec();
        }
        Some(t)
    }
}

fn put_keys(out: &mut Vec<u8>, k: &Keys) {
    for x in k {
        out.extend((x.len() as u32).to_le_bytes());
        out.extend(x);
    }
}

/// The generalized facts saved for the materialization at `commit`, as term keys.
fn read_saved(
    root: &Path,
    dataset_id: uuid::Uuid,
    commit: u64,
    digest: u64,
) -> anyhow::Result<Saved> {
    let Ok(m) = std::fs::read(root.join(SAVED_META)) else {
        return Ok(Saved::Missing);
    };
    let Ok(meta) = serde_json::from_slice::<SavedMeta>(&m) else {
        return Ok(Saved::Missing);
    };
    if meta.dataset_id != dataset_id.to_string() || meta.commit != commit {
        return Ok(Saved::Missing);
    }
    if meta.rules != format!("{digest:016x}") {
        return Ok(Saved::OtherRules);
    }
    let Ok(bin) = std::fs::read(root.join(SAVED_BIN)) else {
        return Ok(Saved::Missing);
    };
    if bin.len() as u64 != meta.bytes || format!("{:016x}", fnv(&bin, FNV_START)) != meta.checksum {
        return Ok(Saved::Missing);
    }
    let log = if meta.log_bytes > 0 {
        let Ok(log) = std::fs::read(root.join(SAVED_LOG)) else {
            return Ok(Saved::Missing);
        };
        let Some(log) = log.get(..meta.log_bytes as usize).map(<[u8]>::to_vec) else {
            return Ok(Saved::Missing);
        };
        if format!("{:016x}", fnv(&log, FNV_START)) != meta.log_checksum {
            return Ok(Saved::Missing);
        }
        log
    } else {
        Vec::new()
    };
    let parse = || -> Option<Vec<Keys>> {
        let mut r = Reader(bin.strip_prefix(MAGIC)?);
        let n = u64::from_le_bytes(r.take(8)?.try_into().ok()?);
        if n != meta.triples {
            return None;
        }
        let mut set: FxHashSet<Keys> = FxHashSet::default();
        for _ in 0..n {
            set.insert(r.keys()?);
        }
        let mut r = Reader(&log);
        while !r.0.is_empty() {
            let op = r.take(1)?[0];
            let k = r.keys()?;
            match op {
                b'+' => set.insert(k),
                b'-' => set.remove(&k),
                _ => return None,
            };
        }
        Some(set.into_iter().collect())
    };
    Ok(match parse() {
        Some(keys) => Saved::Found(meta, keys),
        None => Saved::Missing,
    })
}

/// Replace a file by renaming a new one over it. Nothing is synced: after a crash, a
/// state file that is missing, torn or of another commit only makes the next run a full
/// one (the commit and the checksums are checked on reading).
fn write_atomic(root: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = root.join(format!("{name}.tmp"));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    drop(f);
    std::fs::rename(&tmp, root.join(name))
}

/// Save the generalized facts of the materialization at the run's commit. With `prev`,
/// the saved state the run started from, and the run's changes, only those changes are
/// appended to the log.
pub(crate) fn save(
    store: &Store,
    receipt: &sparkles::commit::Receipt,
    digest: u64,
    terms: &Terms,
    generalized: &FxHashSet<Triple>,
    prev: Option<(&SavedMeta, &GeneralizedChanges)>,
) -> anyhow::Result<Option<SavedMeta>> {
    let Some(root) = store.root() else {
        return Ok(None);
    };
    let keys = |t: &Triple| -> Option<Keys> {
        Some([
            terms.key_of(t[0])?,
            terms.key_of(t[1])?,
            terms.key_of(t[2])?,
        ])
    };
    let mut meta = match prev {
        Some((m, ch)) if m.log_bytes < m.bytes.max(1 << 20) => {
            let mut rec = Vec::new();
            for (op, ts) in [(b'-', &ch.removed), (b'+', &ch.added)] {
                for k in ts.iter().filter_map(keys) {
                    rec.push(op);
                    put_keys(&mut rec, &k);
                }
            }
            let mut m = m.clone();
            if !rec.is_empty() {
                // drop what a run that never saved its metadata appended
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(false)
                    .open(root.join(SAVED_LOG))?;
                f.set_len(m.log_bytes)?;
                use std::io::Seek as _;
                f.seek(std::io::SeekFrom::End(0))?;
                f.write_all(&rec)?;
                let h = match m.log_bytes {
                    0 => FNV_START,
                    _ => u64::from_str_radix(&m.log_checksum, 16)?,
                };
                m.log_bytes += rec.len() as u64;
                m.log_checksum = format!("{:016x}", fnv(&rec, h));
            }
            m
        }
        _ => {
            let mut bin = MAGIC.to_vec();
            let n_at = bin.len();
            bin.extend(0u64.to_le_bytes());
            let mut n = 0u64;
            for k in generalized.iter().filter_map(keys) {
                put_keys(&mut bin, &k);
                n += 1;
            }
            bin[n_at..n_at + 8].copy_from_slice(&n.to_le_bytes());
            write_atomic(root, SAVED_BIN, &bin)?;
            let _ = std::fs::remove_file(root.join(SAVED_LOG));
            SavedMeta {
                dataset_id: String::new(),
                commit: 0,
                rules: String::new(),
                triples: n,
                bytes: bin.len() as u64,
                checksum: format!("{:016x}", fnv(&bin, FNV_START)),
                log_bytes: 0,
                log_checksum: String::new(),
            }
        }
    };
    meta.dataset_id = receipt.dataset_id.to_string();
    meta.commit = receipt.commit.seq;
    meta.rules = format!("{digest:016x}");
    write_atomic(root, SAVED_META, &serde_json::to_vec_pretty(&meta)?)?;
    Ok(Some(meta))
}

/// Remove the saved state (the inferred graph was cleared).
pub(crate) fn remove_saved(store: &Store) {
    if let Some(root) = store.root() {
        for f in [SAVED_META, SAVED_BIN, SAVED_LOG] {
            let _ = std::fs::remove_file(root.join(f));
        }
    }
}
