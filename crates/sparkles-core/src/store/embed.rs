//! Embeddings computed on write (F08) in the store: scheduling pairs from commits,
//! full passes, the prepare and apply steps of the worker, the status, and the
//! embedding of a search's text.

use super::{Chunk, Snapshot, Store};
use crate::commit::CommitKind;
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::Perm;
use crate::vector::config::VectorIndexConfig;
use crate::vector::embed::client::{self, CallError, Waits};
use crate::vector::embed::worker::{
    Embedded, Item, Node, Pair, Pass, Prepared, RunningPass, Work, inputs_hash, pair_hash,
};
use crate::vector::embed::{
    Batch, EmbeddingConfig, EmbeddingMetrics, EmbeddingScan, EmbeddingStatus, Environment,
    FailureKind,
};
use oxrdf::Term;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Texts per pair (ids of one snapshot), with their source position.
type PairTexts = FxHashMap<(u64, u64), Vec<(usize, String)>>;

/// Pairs one commit may schedule; past it, a full pass replaces them.
const MAX_SCHEDULED: usize = 10_000;
/// Pairs a full pass looks at per step.
const PASS_STEP: usize = 2000;
/// The least time between two passes of a query source.
const QUERY_PASS_GAP: Duration = Duration::from_secs(1);
/// The backoff after a request failed for good, and after a refused key.
const BACKOFF: Duration = Duration::from_secs(60);
const AUTH_BACKOFF: Duration = Duration::from_secs(300);

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";

/// The ids a predicate source needs, resolved in one snapshot.
struct Source<'a> {
    emb: &'a EmbeddingConfig,
    preds: Vec<Option<Id>>,
    classes: Vec<Id>,
    rdf_type: Option<Id>,
    target: Option<Id>,
}

impl<'a> Source<'a> {
    fn of(snap: &Snapshot, emb: &'a EmbeddingConfig, target: &str) -> Source<'a> {
        Source {
            emb,
            preds: emb.predicates.iter().map(|p| snap.lookup_iri(p)).collect(),
            classes: emb
                .classes
                .iter()
                .filter_map(|c| snap.lookup_iri(c))
                .collect(),
            rdf_type: snap.lookup_iri(RDF_TYPE),
            target: snap.lookup_iri(target),
        }
    }
}

/// Quads with a key prefix of `perm`, as `[s, p, o, g]`.
fn quads(snap: &Snapshot, perm: Perm, prefix: &[u64], mut f: impl FnMut([Id; 4])) -> Result<()> {
    snap.scan(perm, prefix, |c| {
        match c {
            Chunk::Block(b, s, e) => {
                for i in s..e {
                    f(perm.to_quad(&b.key(i)));
                }
            }
            Chunk::Row(k) => f(perm.to_quad(&k)),
        }
        Ok(true)
    })
}

/// The vectors of `(s, g)` under the index's predicate.
fn vectors_of(snap: &Snapshot, target: Option<Id>, s: Id, g: Id) -> Result<Vec<Id>> {
    let mut out = Vec::new();
    if let Some(t) = target {
        quads(snap, Perm::Spo, &[s.0, t.0], |q| {
            if q[3] == g {
                out.push(q[2]);
            }
        })?;
    }
    Ok(out)
}

/// A selected literal's text: a string or language-tagged literal whose tag the
/// configuration selects.
fn selected_text(emb: &EmbeddingConfig, t: Option<Term>) -> Option<String> {
    let Some(Term::Literal(l)) = t else {
        return None;
    };
    let dt = l.datatype().as_str();
    if dt != XSD_STRING && dt != RDF_LANG_STRING {
        return None;
    }
    let lang = l.language().unwrap_or("").to_ascii_lowercase();
    emb.language_selected(&lang).then(|| l.value().to_string())
}

/// The sorted, distinct inputs of `texts` (by source position, then text).
fn make_inputs(emb: &EmbeddingConfig, mut texts: Vec<(usize, String)>) -> Vec<String> {
    texts.sort();
    texts.dedup();
    let mut inputs: Vec<String> = if emb.combine {
        if texts.is_empty() {
            Vec::new()
        } else {
            let joined: Vec<&str> = texts.iter().map(|(_, t)| t.as_str()).collect();
            emb.inputs(&joined.join("\n"))
        }
    } else {
        texts.iter().flat_map(|(_, t)| emb.inputs(t)).collect()
    };
    inputs.sort();
    inputs.dedup();
    inputs
}

/// The inputs of `(s, g)` from a predicate source.
fn predicate_inputs(snap: &Snapshot, src: &Source<'_>, s: Id, g: Id) -> Result<Vec<String>> {
    let mut texts = Vec::new();
    for (i, p) in src.preds.iter().enumerate() {
        let Some(p) = p else { continue };
        quads(snap, Perm::Spo, &[s.0, p.0], |q| {
            if q[3] == g
                && let Some(t) = selected_text(src.emb, snap.term(q[2]))
            {
                texts.push((i, t));
            }
        })?;
    }
    if texts.is_empty() || src.emb.classes.is_empty() {
        return Ok(make_inputs(src.emb, texts));
    }
    let typed = match src.rdf_type {
        Some(ty) => {
            let mut any = false;
            for c in &src.classes {
                if snap.contains(&[s, ty, *c, g])? {
                    any = true;
                    break;
                }
            }
            any
        }
        None => false,
    };
    Ok(if typed {
        make_inputs(src.emb, texts)
    } else {
        Vec::new()
    })
}

/// The rows of a query source, as texts per pair (ids of `snap`). With `subject`, the
/// query runs with `?s` bound to it.
fn query_rows(
    snap: &Arc<Snapshot>,
    emb: &EmbeddingConfig,
    subject: Option<Id>,
) -> Result<PairTexts> {
    let mut opts = crate::sparql::QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    if let Some(s) = subject {
        match snap.term(s) {
            Some(t) => opts.initial_bindings.push(("s".into(), t)),
            None => return Ok(FxHashMap::default()),
        }
    }
    let r = crate::sparql::query(snap.clone(), emb.query.as_deref().unwrap_or(""), &opts)?;
    let col = |n: &str| r.vars.iter().position(|v| v == n);
    let (cs, ct, cg) = (col("s"), col("text"), col("g"));
    let mut out: PairTexts = FxHashMap::default();
    let (Some(cs), Some(ct)) = (cs, ct) else {
        return Ok(out);
    };
    for row in r.rows() {
        let s = match &row[cs] {
            Some(t @ (Term::NamedNode(_) | Term::BlankNode(_))) => snap.lookup_term(t),
            _ => None,
        };
        let text = match &row[ct] {
            Some(Term::Literal(l)) => Some(l.value().to_string()),
            _ => None,
        };
        let g = match cg.and_then(|c| row[c].as_ref()) {
            Some(t @ (Term::NamedNode(_) | Term::BlankNode(_))) => snap.lookup_term(t),
            Some(_) => None,
            None => Some(Id::DEFAULT_GRAPH),
        };
        if let (Some(s), Some(text), Some(g)) = (s, text, g) {
            out.entry((s.0, g.0)).or_default().push((0, text));
        }
    }
    Ok(out)
}

/// The inputs of `(s, g)` in `snap`, for either kind of source.
fn pair_inputs(
    snap: &Arc<Snapshot>,
    emb: &EmbeddingConfig,
    src: &Source<'_>,
    s: Id,
    g: Id,
) -> Result<Vec<String>> {
    if emb.query.is_some() {
        let mut rows = query_rows(snap, emb, Some(s))?;
        Ok(make_inputs(
            emb,
            rows.remove(&(s.0, g.0)).unwrap_or_default(),
        ))
    } else {
        predicate_inputs(snap, src, s, g)
    }
}

fn subject_label(snap: &Snapshot, s: Id) -> String {
    snap.term(s).map(|t| t.to_string()).unwrap_or_default()
}

impl Store {
    /// The embedding state of this store, for a worker thread
    /// ([`run_worker`](crate::vector::embed::run_worker)).
    pub fn embedder(&self) -> Arc<crate::vector::embed::Embedder> {
        self.embed.clone()
    }

    /// Use `env` for this store's embedding requests instead of the process's
    /// ([`set_environment`](crate::vector::embed::set_environment)); `None` goes back to
    /// it.
    pub fn set_embedding_environment(&self, env: Option<Environment>) {
        *self.embed.env.write() = env.map(Arc::new);
        self.embed.notify();
    }

    /// The directory of the record files (`None`: an in-memory store).
    fn embed_dir(&self) -> Option<std::path::PathBuf> {
        self.root.as_ref().map(|r| r.join("embed"))
    }

    /// Set (or remove) the embedding state of index `name` after its configuration was
    /// written. An unchanged embedding keeps its queue and counters.
    pub(super) fn embed_configure(&self, name: &str, cfg: Option<&VectorIndexConfig>) {
        let seq = self.snapshot().commit;
        let mut works = self.embed.works.lock();
        let dir = self.embed_dir();
        match cfg.and_then(|c| c.embedding.as_ref().map(|e| (c, e))) {
            None => {
                if let Some(mut w) = works.remove(name) {
                    w.record.remove_file();
                }
            }
            Some((c, e)) => {
                if let Some(w) = works.get_mut(name)
                    && w.emb == *e
                    && w.predicate == c.predicate
                    && w.dimension == c.dimension
                {
                    return;
                }
                let epoch = works.get(name).map_or(0, |w| w.epoch + 1);
                let old = works.remove(name);
                let mut w = Work::new(
                    name,
                    &c.predicate,
                    c.dimension,
                    e.clone(),
                    dir.as_deref(),
                    epoch,
                    seq,
                );
                if let Some(o) = old {
                    // the queue still names the pairs to look at
                    for (p, s) in o.queue {
                        w.push(p, s);
                    }
                    w.stats = o.stats;
                }
                works.insert(name.to_string(), w);
            }
        }
        drop(works);
        self.embed.notify();
    }

    /// Note the pairs commit `seq`'s changes (`log`) scheduled, in the published
    /// snapshot `snap`.
    pub(super) fn embed_noted(
        &self,
        snap: &Snapshot,
        log: &[(u8, [Id; 4])],
        seq: u64,
        kind: CommitKind,
    ) {
        if kind == CommitKind::Embed {
            return;
        }
        let mut works = self.embed.works.lock();
        if works.is_empty() {
            return;
        }
        let mut any = false;
        for w in works.values_mut() {
            if w.emb.query.is_some() {
                w.schedule_pass(seq, QUERY_PASS_GAP);
                any = true;
                continue;
            }
            let src = Source::of(snap, &w.emb, &w.predicate);
            let preds: FxHashSet<Id> = src.preds.iter().flatten().copied().collect();
            let classes: FxHashSet<Id> = src.classes.iter().copied().collect();
            let mut pairs: FxHashSet<(Id, Id)> = FxHashSet::default();
            let mut overflow = false;
            for (_, q) in log {
                if preds.contains(&q[1]) || (Some(q[1]) == src.rdf_type && classes.contains(&q[2]))
                {
                    pairs.insert((q[0], q[3]));
                    if pairs.len() > MAX_SCHEDULED {
                        overflow = true;
                        break;
                    }
                }
            }
            if overflow {
                w.schedule_pass(seq, Duration::ZERO);
                any = true;
                continue;
            }
            for (s, g) in pairs {
                if let (Some(s), Some(g)) = (Node::of(snap, s), Node::of(snap, g)) {
                    w.push((s, g), seq);
                    any = true;
                }
            }
        }
        drop(works);
        if any {
            self.embed.notify();
        }
    }

    /// A bulk commit (`seq`) rebuilt the generation without log records: every index
    /// looks at all its pairs.
    pub(super) fn embed_bulk(&self, seq: u64) {
        let mut works = self.embed.works.lock();
        for w in works.values_mut() {
            w.schedule_pass(seq, Duration::ZERO);
        }
        let any = !works.is_empty();
        drop(works);
        if any {
            self.embed.notify();
        }
    }

    /// End every backoff now: the next step tries the provider again.
    pub fn retry_embedding(&self) {
        for w in self.embed.works.lock().values_mut() {
            w.retry_at = None;
            w.next_request = None;
        }
        self.embed.notify();
    }

    /// Embed every selected text of index `name` again, for example after the model
    /// behind its name changed (`404` without an embedding index of that name).
    pub fn reembed(&self, name: &str) -> Result<()> {
        let seq = self.snapshot().commit;
        let mut works = self.embed.works.lock();
        let w = works.get_mut(name).ok_or_else(|| {
            Error::NotFound(format!("vector index {name} computes no embeddings"))
        })?;
        let identity = w.emb.identity(w.dimension);
        w.record.clear(identity);
        w.failed.clear();
        w.pass = Some(Pass {
            seq,
            not_before: Instant::now(),
            running: None,
        });
        w.epoch += 1;
        w.in_flight = None;
        drop(works);
        self.embed.cache.clear();
        self.embed.notify();
        Ok(())
    }

    /// The embedding status of index `name` (`None`: it computes no embeddings).
    pub fn embedding_status(&self, name: &str) -> Option<EmbeddingStatus> {
        let env = self.embed.env();
        let head = self.snapshot().commit;
        let attached = self.embed.attached();
        let works = self.embed.works.lock();
        let w = works.get(name)?;
        let now = Instant::now();
        let backoff = w.retry_at.filter(|(t, _)| *t > now);
        let running = w.pass.as_ref().and_then(|p| p.running.as_ref());
        let busy = !w.queue.is_empty() || w.in_flight.is_some() || w.pass.is_some();
        let state = if !env.enabled {
            "disabled"
        } else if backoff.is_some() {
            "backoff"
        } else if !attached && busy {
            "paused"
        } else if running.is_some() {
            "scanning"
        } else if busy {
            "embedding"
        } else {
            "idle"
        };
        Some(EmbeddingStatus {
            state: state.into(),
            model: w.emb.model.clone(),
            endpoint: w.emb.endpoint(),
            backlog: w.queue.len() as u64,
            scan: running.map(|r| EmbeddingScan {
                done: r.pos as u64,
                total: r.pairs.len() as u64,
            }),
            applied_seq: w.applied(head),
            head_seq: head,
            embedded: w.stats.embedded,
            requests: w.stats.requests,
            failed: w.stats.failed,
            last_error: w.stats.last_error.clone(),
            retry_at: backoff.map(|(_, ms)| crate::commit::rfc3339_ms(ms)),
            last_batch: w.stats.last_batch.clone(),
            config: w.emb.clone(),
        })
    }

    /// The embedding counters and gauges of every index that embeds, for metrics.
    pub fn embedding_metrics(&self) -> Vec<EmbeddingMetrics> {
        let head = self.snapshot().commit;
        let works = self.embed.works.lock();
        works
            .values()
            .map(|w| EmbeddingMetrics {
                index: w.name.clone(),
                requests: w.stats.requests,
                inputs: w.stats.inputs,
                vectors: w.stats.embedded,
                failures: w.stats.failures,
                backlog: w.queue.len() as u64,
                lag: head.saturating_sub(w.applied(head)),
            })
            .collect()
    }

    /// The worker's first step: start or continue a full pass, or take a batch of
    /// scheduled pairs and compute their inputs. Holds no lock a commit needs while it
    /// reads the store.
    pub fn embed_prepare(&self) -> Prepared {
        let env = self.embed.env();
        if !env.enabled || self.embed.is_closed() {
            return Prepared::Idle;
        }
        let now = Instant::now();
        enum Act {
            StartPass,
            Pass(
                Arc<Snapshot>,
                Vec<(u64, u64)>,
                Option<FxHashMap<(u64, u64), Vec<String>>>,
            ),
            Batch(Vec<(Pair, u64)>),
        }
        let mut wait: Option<Duration> = None;
        let mut soon = |d: Duration| wait = Some(wait.map_or(d, |w: Duration| w.min(d)));
        let (name, epoch, emb, predicate, dimension, act) = {
            let mut works = self.embed.works.lock();
            let names: Vec<String> = works.keys().cloned().collect();
            if names.is_empty() {
                return Prepared::Idle;
            }
            let start = self.embed.next.fetch_add(1, Ordering::Relaxed) % names.len();
            let mut found = None;
            for k in 0..names.len() {
                let name = &names[(start + k) % names.len()];
                let w = works.get_mut(name).expect("listed");
                if w.in_flight.is_some() {
                    continue;
                }
                if let Some((t, _)) = w.retry_at {
                    if t > now {
                        soon(t - now);
                        continue;
                    }
                    w.retry_at = None;
                }
                let act = match &w.pass {
                    Some(p) => match &p.running {
                        Some(r) => {
                            let end = (r.pos + PASS_STEP).min(r.pairs.len());
                            let qi = r.query_inputs.as_ref().map(|m| {
                                r.pairs[r.pos..end]
                                    .iter()
                                    .filter_map(|k| m.get(k).map(|v| (*k, v.clone())))
                                    .collect()
                            });
                            Some(Act::Pass(r.snap.clone(), r.pairs[r.pos..end].to_vec(), qi))
                        }
                        None if p.not_before <= now => Some(Act::StartPass),
                        None => {
                            soon(p.not_before - now);
                            None
                        }
                    },
                    None => None,
                };
                let act = match act {
                    Some(a) => a,
                    None if !w.queue.is_empty() => {
                        if let Some(t) = w.next_request
                            && t > now
                        {
                            soon(t - now);
                            continue;
                        }
                        let n = w.emb.batch_size.min(w.queue.len());
                        let taken: Vec<(Pair, u64)> = w.queue.drain(..n).collect();
                        for (p, _) in &taken {
                            w.queued.remove(p);
                        }
                        w.in_flight = taken.iter().map(|t| t.1).min();
                        Act::Batch(taken)
                    }
                    None => continue,
                };
                found = Some((
                    name.clone(),
                    w.epoch,
                    w.emb.clone(),
                    w.predicate.clone(),
                    w.dimension,
                    act,
                ));
                break;
            }
            match found {
                Some(f) => f,
                None => {
                    return match wait {
                        Some(d) => Prepared::Wait(d),
                        None => Prepared::Idle,
                    };
                }
            }
        };
        match act {
            Act::StartPass => {
                self.embed_start_pass(&name, epoch, &emb, &predicate);
                Prepared::Progress
            }
            Act::Pass(snap, pairs, qi) => {
                self.embed_pass_step(&name, epoch, &emb, &predicate, &snap, &pairs, qi);
                Prepared::Progress
            }
            Act::Batch(taken) => {
                match self.embed_batch(&name, epoch, emb, &predicate, dimension, env, taken) {
                    Some(b) => Prepared::Batch(Box::new(b)),
                    None => Prepared::Progress,
                }
            }
        }
    }

    /// List the pairs of a full pass of index `name`.
    fn embed_start_pass(&self, name: &str, epoch: u64, emb: &EmbeddingConfig, predicate: &str) {
        let snap = self.snapshot();
        let src = Source::of(&snap, emb, predicate);
        let listed = (|| -> Result<_> {
            let mut set: FxHashSet<(u64, u64)> = FxHashSet::default();
            let mut qi = None;
            if emb.query.is_some() {
                let rows = query_rows(&snap, emb, None)?;
                let mut m = FxHashMap::default();
                for (k, texts) in rows {
                    set.insert(k);
                    m.insert(k, make_inputs(emb, texts));
                }
                qi = Some(m);
            } else {
                for p in src.preds.iter().flatten() {
                    quads(&snap, Perm::Pso, &[p.0], |q| {
                        set.insert((q[0].0, q[3].0));
                    })?;
                }
            }
            if let Some(t) = src.target {
                quads(&snap, Perm::Pso, &[t.0], |q| {
                    set.insert((q[0].0, q[3].0));
                })?;
            }
            let mut pairs: Vec<(u64, u64)> = set.into_iter().collect();
            pairs.sort_unstable();
            Ok((pairs, qi))
        })();
        let mut works = self.embed.works.lock();
        let Some(w) = works.get_mut(name).filter(|w| w.epoch == epoch) else {
            return;
        };
        match listed {
            Ok((pairs, qi)) => {
                // pairs that failed are tried again by a pass
                w.failed.clear();
                if let Some(p) = &mut w.pass {
                    p.running = Some(RunningPass {
                        snap,
                        pairs,
                        pos: 0,
                        query_inputs: qi,
                    });
                }
                w.last_pass = Some(Instant::now());
            }
            Err(e) => {
                w.error(format!("full pass: {e}"), None);
                w.retry_at = Some(backoff_until(BACKOFF));
            }
        }
    }

    /// Look at a step of a running pass and schedule the pairs that need work.
    #[allow(clippy::too_many_arguments)]
    fn embed_pass_step(
        &self,
        name: &str,
        epoch: u64,
        emb: &EmbeddingConfig,
        predicate: &str,
        snap: &Arc<Snapshot>,
        pairs: &[(u64, u64)],
        qi: Option<FxHashMap<(u64, u64), Vec<String>>>,
    ) {
        let src = Source::of(snap, emb, predicate);
        // (pair, inputs hash or 0 without inputs, distinct inputs, vectors)
        let mut seen: Vec<(Pair, u64, usize, usize)> = Vec::with_capacity(pairs.len());
        let mut err = None;
        for &(s, g) in pairs {
            let (s, g) = (Id(s), Id(g));
            let inputs = match &qi {
                Some(m) => Ok(m.get(&(s.0, g.0)).cloned().unwrap_or_default()),
                None => predicate_inputs(snap, &src, s, g),
            };
            let r = inputs.and_then(|i| Ok((i, vectors_of(snap, src.target, s, g)?)));
            match r {
                Ok((inputs, vecs)) => {
                    if let (Some(ns), Some(ng)) = (Node::of(snap, s), Node::of(snap, g)) {
                        let h = if inputs.is_empty() {
                            0
                        } else {
                            inputs_hash(&inputs)
                        };
                        seen.push(((ns, ng), h, inputs.len(), vecs.len()));
                    }
                }
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        let mut works = self.embed.works.lock();
        let Some(w) = works.get_mut(name).filter(|w| w.epoch == epoch) else {
            return;
        };
        let Some(pass) = w.pass.as_mut() else { return };
        let Some(r) = pass.running.as_mut().filter(|r| Arc::ptr_eq(&r.snap, snap)) else {
            return;
        };
        if let Some(e) = err {
            w.error(format!("full pass: {e}"), None);
            w.pass = None;
            w.retry_at = Some(backoff_until(BACKOFF));
            return;
        }
        r.pos += pairs.len();
        let finished = r.pos >= r.pairs.len();
        let seq = pass.seq;
        for (pair, h, n_inputs, n_vectors) in seen {
            let ph = pair_hash(&pair);
            let ok = if h == 0 {
                n_vectors == 0
            } else {
                w.record.get(ph) == Some(h) && n_vectors == n_inputs
            };
            if h == 0 && n_vectors == 0 {
                w.record.set(ph, 0);
            }
            if !ok {
                w.push(pair, seq);
            }
        }
        if finished {
            w.pass = None;
            w.record.flush();
        }
    }

    /// Compute the inputs of the pairs taken from the queue, and drop those that need
    /// nothing. `None` when none needs a request.
    #[allow(clippy::too_many_arguments)]
    fn embed_batch(
        &self,
        name: &str,
        epoch: u64,
        emb: EmbeddingConfig,
        predicate: &str,
        dimension: usize,
        env: Arc<Environment>,
        taken: Vec<(Pair, u64)>,
    ) -> Option<Batch> {
        let snap = self.snapshot();
        let src = Source::of(&snap, &emb, predicate);
        let mut computed = Vec::with_capacity(taken.len());
        for (pair, seq) in taken {
            let ids = (pair.0.id(&snap), pair.1.id(&snap));
            let r = match ids {
                (Some(s), Some(g)) => pair_inputs(&snap, &emb, &src, s, g).and_then(|i| {
                    Ok((
                        i,
                        vectors_of(&snap, src.target, s, g)?.len(),
                        subject_label(&snap, s),
                    ))
                }),
                _ => Ok((Vec::new(), 0, String::new())),
            };
            computed.push((pair, seq, r));
        }
        let mut works = self.embed.works.lock();
        let w = works.get_mut(name).filter(|w| w.epoch == epoch)?;
        let mut items = Vec::new();
        for (pair, seq, r) in computed {
            let ph = pair_hash(&pair);
            match r {
                Err(e) => {
                    w.stats.failed += 1;
                    w.stats.fail(FailureKind::Read, 1);
                    w.error(format!("reading the text: {e}"), None);
                }
                Ok((inputs, n_vectors, subject)) => {
                    if inputs.is_empty() {
                        if n_vectors == 0 {
                            w.record.set(ph, 0);
                            continue;
                        }
                    } else {
                        let h = inputs_hash(&inputs);
                        if (w.record.get(ph) == Some(h) && n_vectors == inputs.len())
                            || w.failed.get(&ph) == Some(&h)
                        {
                            continue;
                        }
                    }
                    items.push(Item {
                        pair,
                        seq,
                        inputs,
                        subject,
                    });
                }
            }
        }
        if items.is_empty() {
            w.in_flight = None;
            w.record.flush();
            return None;
        }
        w.in_flight = items.iter().map(|i| i.seq).min();
        Some(Batch {
            index: name.to_string(),
            target: predicate.to_string(),
            prepared_at: snap.commit,
            epoch,
            emb,
            dimension,
            env,
            items,
            cache: self.embed.clone(),
        })
    }

    /// The worker's last step: write the vectors of a batch in one commit of kind
    /// `embed`, or keep the batch for later when its request failed.
    pub fn embed_apply(&self, done: Embedded) {
        let Embedded {
            batch,
            vectors,
            requests,
            ms,
            sent,
            sent_chars,
        } = done;
        let vectors = {
            let mut works = self.embed.works.lock();
            let Some(w) = works
                .get_mut(&batch.index)
                .filter(|w| w.epoch == batch.epoch)
            else {
                return;
            };
            w.stats.requests += requests;
            w.stats.inputs += sent;
            match vectors {
                Ok(v) => v,
                Err(e) => {
                    w.stats.fail(
                        match &e {
                            CallError::Transient(..) => FailureKind::Transient,
                            CallError::Auth(_) => FailureKind::Auth,
                            CallError::Refused(_) => FailureKind::Refused,
                            CallError::Fatal(_) => FailureKind::Fatal,
                            CallError::Rejected(_) => FailureKind::Rejected,
                        },
                        1,
                    );
                    let wait = match &e {
                        CallError::Auth(_) | CallError::Fatal(_) | CallError::Refused(_) => {
                            AUTH_BACKOFF
                        }
                        _ => BACKOFF,
                    };
                    w.error(e.to_string(), None);
                    w.retry_at = Some(backoff_until(wait));
                    w.in_flight = None;
                    // the batch goes back to the front, in its order
                    for it in batch.items.into_iter().rev() {
                        if w.queued.insert(it.pair.clone()) {
                            w.queue.push_front((it.pair, it.seq));
                        }
                    }
                    return;
                }
            }
        };
        let r = self.embed_write(&batch, &vectors);
        let mut works = self.embed.works.lock();
        let Some(w) = works
            .get_mut(&batch.index)
            .filter(|w| w.epoch == batch.epoch)
        else {
            return;
        };
        w.in_flight = None;
        if batch.emb.requests_per_minute > 0 {
            w.next_request = Some(
                Instant::now()
                    + Duration::from_secs_f64(60.0 / batch.emb.requests_per_minute as f64),
            );
        }
        if batch.emb.tokens_per_minute > 0 && sent_chars > 0 {
            // the tokens of the batch, estimated from its length, spread over a minute
            let tokens = sent_chars.div_ceil(crate::vector::embed::config::CHARS_PER_TOKEN as u64);
            let until = Instant::now()
                + Duration::from_secs_f64(
                    60.0 * tokens as f64 / batch.emb.tokens_per_minute as f64,
                );
            w.next_request = Some(w.next_request.map_or(until, |t| t.max(until)));
        }
        if sent > 0 {
            w.stats.last_batch = Some(crate::vector::embed::EmbeddingBatch {
                at: crate::commit::rfc3339_ms(crate::commit::now_ms()),
                inputs: sent,
                ms,
            });
        }
        match r {
            Ok(outcomes) => {
                for o in outcomes {
                    match o {
                        Outcome::Done(ph, h, n) => {
                            w.record.set(ph, h);
                            w.failed.remove(&ph);
                            w.stats.embedded += n;
                        }
                        Outcome::Failed(ph, h, n, msg, subject) => {
                            w.failed.insert(ph, h);
                            w.stats.failed += n;
                            w.stats.fail(FailureKind::Rejected, n);
                            w.error(msg, Some(subject));
                        }
                        Outcome::Skipped => {}
                    }
                }
                w.record.flush();
            }
            Err(e) => {
                let permanent = matches!(
                    e,
                    Error::Rejected(_) | Error::Invalid(_) | Error::NotPermitted(_)
                );
                w.error(format!("writing the vectors: {e}"), None);
                w.stats.fail(FailureKind::Write, 1);
                if permanent {
                    for it in &batch.items {
                        w.failed
                            .insert(pair_hash(&it.pair), inputs_hash(&it.inputs));
                        w.stats.failed += it.inputs.len().max(1) as u64;
                    }
                } else {
                    w.retry_at = Some(backoff_until(BACKOFF));
                    for it in batch.items.iter().rev() {
                        if w.queued.insert(it.pair.clone()) {
                            w.queue.push_front((it.pair.clone(), it.seq));
                        }
                    }
                }
            }
        }
    }

    /// The commit of a batch: per pair, the vectors of its inputs replace the ones it
    /// has. A pair whose inputs changed since the batch was prepared is left alone (the
    /// change scheduled it again).
    fn embed_write(
        &self,
        batch: &Batch,
        vectors: &FxHashMap<String, std::result::Result<Arc<[f32]>, String>>,
    ) -> Result<Vec<Outcome>> {
        let mut txn = self.write_with(
            CommitKind::Embed,
            crate::guard::WriteOptions {
                message: Some(format!("embeddings of vector index {}", batch.index).into()),
                ..Default::default()
            },
        );
        let view = Arc::new(txn.view());
        let src = Source::of(&view, &batch.emb, &batch.target);
        let target = txn.intern(&Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            batch.target.clone(),
        )))?;
        let mut out = Vec::with_capacity(batch.items.len());
        for it in &batch.items {
            let ph = pair_hash(&it.pair);
            let (Some(s), Some(g)) = (it.pair.0.id(&view), it.pair.1.id(&view)) else {
                // the subject or graph is gone: so are its text and vectors
                out.push(if it.inputs.is_empty() {
                    Outcome::Done(ph, 0, 0)
                } else {
                    Outcome::Skipped
                });
                continue;
            };
            // without a commit since the batch was prepared, its inputs still hold
            if view.commit != batch.prepared_at {
                let now = pair_inputs(&view, &batch.emb, &src, s, g)?;
                if now != it.inputs {
                    out.push(Outcome::Skipped);
                    continue;
                }
            }
            let mut desired = Vec::new();
            let mut failed: Option<String> = None;
            for i in &it.inputs {
                match vectors.get(i) {
                    Some(Ok(v)) => {
                        let id = txn.intern(&Term::Literal(crate::vector::literal(v)))?;
                        if !desired.contains(&id) {
                            desired.push(id);
                        }
                    }
                    Some(Err(m)) => failed = Some(m.clone()),
                    None => failed = Some("no vector came back for this input".into()),
                }
            }
            let existing = vectors_of(&txn.view(), Some(target), s, g)?;
            for o in &existing {
                if !desired.contains(o) {
                    txn.delete([s, target, *o, g])?;
                }
            }
            let mut added = 0;
            for o in &desired {
                if !existing.contains(o) {
                    txn.insert([s, target, *o, g])?;
                    added += 1;
                }
            }
            let h = if it.inputs.is_empty() {
                0
            } else {
                inputs_hash(&it.inputs)
            };
            out.push(match failed {
                Some(m) => Outcome::Failed(ph, h, 1, m, it.subject.clone()),
                None => Outcome::Done(ph, h, added),
            });
        }
        txn.commit()?;
        Ok(out)
    }

    /// Run the embedding work of this store on this thread until nothing is left, or
    /// `timeout` passes (then [`Error::Timeout`]). A request that keeps failing leaves
    /// the work waiting in `backoff`, which also ends the run with a timeout.
    pub fn embed_until_idle(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let _attached = self.embed.attach();
        loop {
            match self.embed_prepare() {
                Prepared::Batch(b) => {
                    let e = self.embed.clone();
                    let done = b.run(&move |d| {
                        if Instant::now() + d > deadline {
                            return false;
                        }
                        e.sleep(d)
                    });
                    self.embed_apply(done);
                }
                Prepared::Progress => {}
                Prepared::Idle => return Ok(()),
                Prepared::Wait(d) => {
                    if Instant::now() + d > deadline {
                        return Err(Error::Timeout);
                    }
                    self.embed.sleep(d);
                }
            }
            if Instant::now() > deadline {
                return Err(Error::Timeout);
            }
        }
    }

    /// Wait (without working) until index `name` has embedded every commit up to `seq`,
    /// at most `timeout`; its status then (`None`: no such embedding index).
    pub fn wait_embedded(
        &self,
        name: &str,
        seq: u64,
        timeout: Duration,
    ) -> Option<EmbeddingStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            let s = self.embedding_status(name)?;
            if s.applied_seq >= seq || Instant::now() >= deadline {
                return Some(s);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// What became of one pair of a batch.
enum Outcome {
    /// written: (pair hash, inputs hash, vectors added)
    Done(u64, u64, u64),
    /// some inputs failed: (pair hash, inputs hash, inputs failed, message, subject)
    Failed(u64, u64, u64, String, String),
    /// its inputs changed meanwhile
    Skipped,
}

fn backoff_until(d: Duration) -> (Instant, i64) {
    (
        Instant::now() + d,
        crate::commit::now_ms() + d.as_millis() as i64,
    )
}

/// The vector of a search's text for the index on predicate `iri` (`400` when it has
/// no embedding, `502` when the provider fails).
pub(crate) fn embed_query_text(snap: &Snapshot, iri: &str, text: &str) -> Result<Arc<[f32]>> {
    let mut v = embed_texts(snap, iri, &[text], true)?;
    v.pop()
        .ok_or_else(|| Error::Service("embedding the query text: no vector".into()))
}

/// The vectors of `texts` from the embedding provider of the vector index on predicate
/// `iri`, in order: as search texts (with the index's `queryPrefix`) when `query` is
/// true, else as stored inputs (with its `inputPrefix`). Vectors the dataset's cache
/// holds are not requested again, and the others go out in batches of the index's
/// `batchSize`. Fails with `Invalid` when the index has no provider or does not embed
/// query text, `NotPermitted` when the outbound policy refuses the endpoint, and
/// `Service` when the provider fails.
pub fn embed_texts(
    snap: &Snapshot,
    iri: &str,
    texts: &[&str],
    query: bool,
) -> Result<Vec<Arc<[f32]>>> {
    let configured = snap.generation.vectors.configured();
    let Some((_, cfg)) = configured.iter().find(|(_, c)| c.predicate == iri) else {
        return Err(Error::invalid(format!(
            "spk:vectorSearch: <{iri}> has no vector index with an embedding provider; pass an spk:vector literal"
        )));
    };
    let Some(emb) = &cfg.embedding else {
        return Err(Error::invalid(format!(
            "spk:vectorSearch: <{iri}> has no embedding provider; pass an spk:vector literal"
        )));
    };
    if !emb.query_text {
        return Err(Error::invalid(format!(
            "spk:vectorSearch: the index of <{iri}> does not embed query text (queryText is false)"
        )));
    }
    let embedder = snap.generation.vectors.embedder();
    let env = embedder
        .as_ref()
        .map_or_else(crate::vector::embed::environment, |e| e.env());
    if !env.enabled {
        return Err(Error::invalid(
            "spk:vectorSearch: embedding is disabled on this server; pass an spk:vector literal",
        ));
    }
    let identity = emb.identity(cfg.dimension);
    // a provider's model may prompt queries differently from documents (spec F12), so
    // the vector of a query text is cached apart from that of the same stored text
    let identity = if query && emb.provider.is_some() {
        identity ^ 0x7175_6572_7900_0000
    } else {
        identity
    };
    let inputs: Vec<String> = texts
        .iter()
        .map(|t| {
            if query {
                emb.query_input(t)
            } else {
                emb.input(t)
            }
        })
        .collect();
    let mut out: Vec<Option<Arc<[f32]>>> = inputs
        .iter()
        .map(|i| embedder.as_ref().and_then(|e| e.cache.get(identity, i)))
        .collect();
    let missing: Vec<usize> = (0..inputs.len()).filter(|&i| out[i].is_none()).collect();
    // a search waits for a short retry at most
    let sleep = |d: Duration| {
        if d > Duration::from_secs(2) {
            return false;
        }
        std::thread::sleep(d);
        true
    };
    let what = if query { "the query text" } else { "the texts" };
    for chunk in missing.chunks(emb.batch_size.max(1)) {
        let batch: Vec<String> = chunk.iter().map(|&i| inputs[i].clone()).collect();
        let mut n = 0;
        let r = client::embed(
            &env,
            emb,
            cfg.dimension,
            &batch,
            query,
            &Waits { sleep: &sleep },
            &mut n,
        );
        let vectors = match r {
            Ok(v) => v,
            Err(CallError::Refused(m)) => {
                return Err(Error::NotPermitted(format!("embedding {what}: {m}")));
            }
            Err(e) => return Err(Error::Service(format!("embedding {what}: {e}"))),
        };
        if vectors.len() != chunk.len() {
            return Err(Error::Service(format!("embedding {what}: no vector")));
        }
        for (&i, v) in chunk.iter().zip(vectors) {
            let v: Arc<[f32]> = v
                .map_err(|m| Error::Service(format!("embedding {what}: {m}")))?
                .into();
            if let Some(e) = &embedder {
                e.cache.insert(identity, inputs[i].clone(), v.clone());
            }
            out[i] = Some(v);
        }
    }
    Ok(out.into_iter().flatten().collect())
}
