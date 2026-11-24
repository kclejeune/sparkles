//! Top-k search over a snapshot: the base rows of a segment (scanned exactly, through
//! the HNSW graph of a configured index, or only those of candidate subjects), minus the
//! snapshot's deleted quads, plus its inserted ones scored exactly.
//!
//! Every path scores with the same kernel ([`super::score`]), so a row's score does not
//! depend on the path. The graph only chooses which base rows are scored: with it, the
//! result is the best rows among the graph's candidates and the inserted rows.

use super::config::SearchMode;
use super::hnsw;
use super::index::{Built, Segment};
use super::live::{LiveState, View};
use super::{Metric, budget, graph_dist, norm, score};
use crate::error::{Error, Result};
use crate::index::Perm;
use crate::store::Snapshot;
use rustc_hash::FxHashSet;
use sparkles_budget::sizes::{table_bytes, table_capacity, table_grown, vec_grown};
use std::sync::Arc;

/// One search hit.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub s: u64,
    pub o: u64,
    pub g: u64,
    pub score: f32,
}

/// Total order: best first, ties by ascending (s, o, g).
fn better(m: Metric, a: &Hit, b: &Hit) -> std::cmp::Ordering {
    let by = if m.higher_is_better() {
        b.score.total_cmp(&a.score)
    } else {
        a.score.total_cmp(&b.score)
    };
    by.then_with(|| (a.s, a.o, a.g).cmp(&(b.s, b.o, b.g)))
}

/// Parameters of one search.
pub struct Search<'a> {
    pub pred: u64,
    pub query: &'a [f32],
    pub k: usize,
    pub metric: Metric,
    /// accepts a graph id
    pub graph: &'a (dyn Fn(u64) -> bool + Sync),
    /// merge rows with the same (s, o) from different graphs
    pub dedup: bool,
    /// at most one row (the best) per subject
    pub distinct_subject: bool,
    /// only rows of these subjects (sorted ids): `candidates:join`
    pub subjects: Option<&'a [u64]>,
    pub mode: SearchMode,
}

/// How a search was answered (for EXPLAIN).
#[derive(Clone, Debug, Default)]
pub struct SearchInfo {
    /// `exact`, `hnsw` or `candidates`
    pub method: &'static str,
    /// why the graph was not used, when an index has one
    pub reason: Option<&'static str>,
    /// the configured index that answered
    pub index: Option<String>,
    pub ef: usize,
    /// base rows of the predicate and dimension
    pub rows: u64,
    /// base rows scored
    pub scored: u64,
    /// rows the snapshot inserted since the generation's base, in scope. When the graph's
    /// live layer has caught up with the snapshot, this is every inserted row of the
    /// predicate, not only those in the search's graph scope, because counting in scope
    /// would cost the walk over the rows that the caught-up layer lets the search skip.
    pub inserted: u64,
    /// of those, the rows scored exactly because the graph does not hold them yet (all
    /// of them unless the graph answered)
    pub pending: u64,
    pub deleted: u64,
}

/// Where the base rows come from.
enum Base {
    Implicit(Arc<super::PredicateVectors>),
    Index(Arc<Built>),
}

impl Base {
    fn segment(&self, dim: usize) -> Option<&Segment> {
        match self {
            Base::Implicit(p) => p.by_dim.get(&dim),
            Base::Index(b) => (b.segment.dim == dim).then_some(&*b.segment),
        }
    }
}

/// The `k` best rows of predicate `q.pred` whose vectors have the query's dimension, in
/// `snap` (its base rows minus deleted quads, plus inserted ones), best first.
pub fn search(
    snap: &Snapshot,
    q: &Search<'_>,
    check: &(dyn Fn() -> Result<()> + Sync),
) -> Result<(Vec<Hit>, SearchInfo)> {
    search_with_admit(snap, q, check, &mut |_| Ok(()))
}

/// [`search`] with pre-admission of query-owned scratch (`deleted`, pending `inserted`
/// rows, and candidate buffers) through `admit` before allocation and growth.
pub(crate) fn search_with_admit(
    snap: &Snapshot,
    q: &Search<'_>,
    check: &(dyn Fn() -> Result<()> + Sync),
    admit: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<(Vec<Hit>, SearchInfo)> {
    let dim = q.query.len();
    let qnorm = norm(q.query);
    if q.metric == Metric::Cosine && qnorm == 0.0 {
        return Err(Error::invalid(
            "cosine is undefined for a zero query vector",
        ));
    }
    let gv = &snap.generation.vectors;
    let mut info = SearchInfo {
        method: "exact",
        ..Default::default()
    };
    // a configured index fixes the predicate's dimension
    let configured = gv.configured_for(snap, q.pred);
    if let Some((name, c)) = &configured {
        if c.dimension != dim {
            return Err(Error::invalid(format!(
                "dimension mismatch: <{}> is indexed with dimension {} (index {name}); query has {dim}",
                c.predicate, c.dimension
            )));
        }
        info.index = Some(name.clone());
    }
    let base = match configured.as_ref().and_then(|(n, _)| gv.built(n)) {
        Some(b) if b.segment.dim == dim => Base::Index(b),
        _ => Base::Implicit(gv.predicate(snap, q.pred, budget())?),
    };
    // the snapshot's changes to this predicate
    let pi = Perm::Pso.index();
    let lo = [q.pred, 0, 0, 0];
    let hi = [q.pred, u64::MAX, u64::MAX, u64::MAX];
    let del_count = snap.delta.del[pi].count_between(&lo, &hi);
    let del_est_bytes = table_bytes(table_capacity(del_count), size_of::<[u64; 3]>()) as u64;
    admit(del_est_bytes)?;
    let mut deleted: FxHashSet<[u64; 3]> = FxHashSet::default();
    if del_count > 0 {
        deleted.reserve(del_count);
        for (i, k) in snap.delta.del[pi].range(lo..=hi).enumerate() {
            if i.is_multiple_of(4096) {
                check()?;
            }
            deleted.insert([k[1], k[2], k[3]]);
        }
    }
    let del_bytes = table_bytes(deleted.capacity(), size_of::<[u64; 3]>()) as u64;
    admit(del_bytes)?;
    info.deleted = deleted.len() as u64;
    let seg = base.segment(dim);
    // whether the graph answers, decided before the inserted rows are read: the rows its
    // live layer holds are then found through it and not scored here
    let plan = match (&base, q.subjects, seg) {
        (Base::Index(b), None, Some(_)) => Some(ann_plan(snap, b, q, &deleted)),
        _ => None,
    };
    let live = match (&base, &plan) {
        (Base::Index(b), Some(Ok(_))) => Some(b.live.read()),
        _ => None,
    };
    let mut ins_dims: FxHashSet<usize> = FxHashSet::default();
    let (mut inserted, covered) = inserted_rows_charged(
        snap,
        q,
        qnorm,
        live.as_deref(),
        &mut ins_dims,
        check,
        del_bytes,
        admit,
    )?;
    info.inserted = inserted.len() as u64 + covered;
    info.pending = inserted.len() as u64;
    if seg.is_none() && inserted.is_empty() {
        let dims_cap = ins_dims.len().saturating_add(match &base {
            Base::Implicit(p) => p.by_dim.len(),
            Base::Index(_) => 0,
        });
        admit(
            del_bytes
                .saturating_add(dimension_bytes(&ins_dims))
                .saturating_add(dims_cap as u64 * size_of::<usize>() as u64),
        )?;
        let mut dims: Vec<usize> = match &base {
            Base::Implicit(p) => p.by_dim.keys().copied().chain(ins_dims).collect(),
            Base::Index(_) => ins_dims.into_iter().collect(),
        };
        dims.sort_unstable();
        dims.dedup();
        if !dims.is_empty() && configured.is_none() {
            return Err(Error::invalid(format!(
                "dimension mismatch: the predicate has vectors of dimension {}; query has {dim}",
                dims.iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        drop(dims);
        drop(deleted);
        admit(0)?;
        return Ok((Vec::new(), info));
    }
    info.rows = seg.map_or(0, |s| s.rows() as u64);
    let ctx = Ctx {
        snap,
        q,
        qnorm,
        deleted: &deleted,
    };
    let overlay_bytes = |inserted: &Vec<Hit>, dims: &FxHashSet<usize>| {
        del_bytes
            .saturating_add(hit_bytes(inserted))
            .saturating_add(dimension_bytes(dims))
    };
    let mut cand: Vec<Hit> = match seg {
        None => Vec::new(),
        Some(seg) => match (q.subjects, &base, plan) {
            (Some(subjects), _, _) => {
                info.method = "candidates";
                ctx.subjects(seg, subjects, &mut info, check, &mut |b| {
                    admit(overlay_bytes(&inserted, &ins_dims).saturating_add(b))
                })?
            }
            (None, Base::Index(b), Some(Ok(ef))) => {
                info.method = "hnsw";
                info.ef = ef;
                let live = live.as_deref().expect("read with the plan");
                match ctx.ann(b, live, covered, ef, &mut info, check, &mut |b| {
                    admit(overlay_bytes(&inserted, &ins_dims).saturating_add(b))
                })? {
                    Some(h) => h,
                    None => {
                        // the graph found too few rows: fall back, scoring every
                        // inserted row
                        info.method = "exact";
                        info.reason = Some("the graph found fewer than k rows");
                        drop(inserted);
                        inserted = inserted_rows_charged(
                            snap,
                            q,
                            qnorm,
                            None,
                            &mut ins_dims,
                            check,
                            del_bytes,
                            admit,
                        )?
                        .0;
                        info.pending = inserted.len() as u64;
                        ctx.exact(seg, &mut info, check, &mut |b| {
                            admit(overlay_bytes(&inserted, &ins_dims).saturating_add(b))
                        })?
                    }
                }
            }
            (None, Base::Index(_), Some(Err(reason))) => {
                info.reason = reason;
                ctx.exact(seg, &mut info, check, &mut |b| {
                    admit(overlay_bytes(&inserted, &ins_dims).saturating_add(b))
                })?
            }
            (None, _, _) => ctx.exact(seg, &mut info, check, &mut |b| {
                admit(overlay_bytes(&inserted, &ins_dims).saturating_add(b))
            })?,
        },
    };
    let overlay = overlay_bytes(&inserted, &ins_dims);
    let hit_size = size_of::<Hit>();
    let next = vec_grown(cand.capacity(), cand.len(), inserted.len(), hit_size);
    let old = if next > cand.capacity() {
        cand.capacity()
    } else {
        0
    };
    admit(overlay.saturating_add((next.saturating_add(old)) as u64 * hit_size as u64))?;
    cand.extend(inserted);
    drop(deleted);
    drop(ins_dims);
    let mut out = finish(q, cand, admit)?;
    if out.len() < out.capacity() {
        admit((out.capacity().saturating_add(out.len())) as u64 * hit_size as u64)?;
        out.shrink_to_fit();
    }
    admit(hit_bytes(&out))?;
    Ok((out, info))
}

fn hit_bytes(hits: &Vec<Hit>) -> u64 {
    hits.capacity() as u64 * size_of::<Hit>() as u64
}

fn dimension_bytes(dims: &FxHashSet<usize>) -> u64 {
    table_bytes(dims.capacity(), size_of::<usize>()) as u64
}

fn push_hit(hits: &mut Vec<Hit>, hit: Hit, admit: &mut dyn FnMut(u64) -> Result<()>) -> Result<()> {
    if hits.len() == hits.capacity() {
        let elem = size_of::<Hit>();
        let next = vec_grown(hits.capacity(), hits.len(), 1, elem);
        admit(hit_bytes(hits).saturating_add(next as u64 * elem as u64))?;
    }
    hits.push(hit);
    Ok(())
}

/// The snapshot's inserted rows of the predicate in scope, scored exactly, and the
/// number of rows in scope whose pair the graph's live layer `live` holds, which are not
/// read here because the graph finds them. When `live` has caught up with a snapshot of
/// the same delta, it holds every row it can score, so the rows are counted without
/// reading them, and the count is every inserted row of the predicate. `ins_dims`
/// collects the dimensions of the rows read.
#[allow(clippy::too_many_arguments)]
fn inserted_rows_charged(
    snap: &Snapshot,
    q: &Search<'_>,
    qnorm: f32,
    live: Option<&LiveState>,
    ins_dims: &mut FxHashSet<usize>,
    check: &(dyn Fn() -> Result<()> + Sync),
    base_bytes: u64,
    admit: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<(Vec<Hit>, u64)> {
    let gv = &snap.generation.vectors;
    let pi = Perm::Pso.index();
    let lo = [q.pred, 0, 0, 0];
    let hi = [q.pred, u64::MAX, u64::MAX, u64::MAX];
    if let Some(l) = live {
        let rows = snap.delta.ins[pi].count_between(&lo, &hi) as u64;
        if l.is_caught_up(snap, rows) {
            return Ok((Vec::new(), rows));
        }
    }
    let mut out = Vec::new();
    let mut covered = 0;
    for (idx, k) in snap.delta.ins[pi].range(lo..=hi).enumerate() {
        if idx.is_multiple_of(4096) {
            check()?;
        }
        if q.subjects.is_some_and(|s| s.binary_search(&k[1]).is_err()) {
            continue;
        }
        if let Some(l) = live
            && l.node_of(k[1], k[2]).is_some()
        {
            covered += (q.graph)(k[3]) as u64;
            continue;
        }
        let Some(v) = gv.overlay_vector(snap, k[2]) else {
            continue;
        };
        if !ins_dims.contains(&v.len()) {
            let next = table_grown(ins_dims.capacity(), ins_dims.len(), 1);
            if next != ins_dims.capacity() {
                admit(
                    base_bytes
                        .saturating_add(hit_bytes(&out))
                        .saturating_add(dimension_bytes(ins_dims))
                        .saturating_add(table_bytes(next, size_of::<usize>()) as u64),
                )?;
            }
            ins_dims.insert(v.len());
        }
        if v.len() != q.query.len() || !(q.graph)(k[3]) {
            continue;
        }
        if let Some(sc) = score(q.metric, q.query, qnorm, &v, norm(&v)) {
            push_hit(
                &mut out,
                Hit {
                    s: k[1],
                    o: k[2],
                    g: k[3],
                    score: sc,
                },
                &mut |b| {
                    admit(
                        base_bytes
                            .saturating_add(dimension_bytes(ins_dims))
                            .saturating_add(b),
                    )
                },
            )?;
        }
    }
    Ok((out, covered))
}

/// Sort, deduplicate and cut the candidates.
fn finish(
    q: &Search<'_>,
    mut cand: Vec<Hit>,
    admit: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<Vec<Hit>> {
    // Equal sort keys have identical Hit fields.
    cand.sort_unstable_by(|a, b| better(q.metric, a, b));
    let seen = cand.len().min(q.k);
    if q.distinct_subject {
        admit(
            hit_bytes(&cand)
                .saturating_add(table_bytes(table_capacity(seen), size_of::<u64>()) as u64),
        )?;
        let mut subjects = FxHashSet::default();
        subjects.reserve(seen);
        cand.retain(|h| subjects.len() < seen && subjects.insert(h.s));
    } else if q.dedup {
        admit(
            hit_bytes(&cand)
                .saturating_add(table_bytes(table_capacity(seen), size_of::<(u64, u64)>()) as u64),
        )?;
        let mut pairs = FxHashSet::default();
        pairs.reserve(seen);
        cand.retain(|h| pairs.len() < seen && pairs.insert((h.s, h.o)));
    }
    cand.truncate(q.k);
    Ok(cand)
}

/// Whether the graph answers, with the `ef` it uses; else why not (`None`: the index has
/// no graph).
fn ann_plan(
    snap: &Snapshot,
    b: &Built,
    q: &Search<'_>,
    deleted: &FxHashSet<[u64; 3]>,
) -> std::result::Result<usize, Option<&'static str>> {
    let Some(h) = b.config.hnsw else {
        return Err(None);
    };
    if q.mode.exact {
        return Err(Some("exact:true"));
    }
    if snap.historical {
        return Err(Some("a past state is searched exactly"));
    }
    let Some(g) = &b.graph else {
        return Err(Some("the graph is being built"));
    };
    if q.metric != b.config.metric {
        return Err(Some("the query's metric is not the index's"));
    }
    let accepted = b.rows_in(q.graph).saturating_sub(deleted.len() as u64);
    // the inserted rows count too, as the graph's live layer finds those it holds (a
    // range count, with no walk); an index whose base holds few rows still uses its
    // graph once inserts make it large
    let pi = Perm::Pso.index();
    let inserted = snap.delta.ins[pi]
        .count_between(&[q.pred, 0, 0, 0], &[q.pred, u64::MAX, u64::MAX, u64::MAX])
        as u64;
    if (accepted + inserted) as usize <= b.config.exact_threshold.max(q.k) {
        return Err(Some("few rows"));
    }
    // a graph filter that rejects most nodes makes the graph walk most of them
    if accepted.saturating_mul(20) < g.nodes as u64 {
        return Err(Some("the active graph holds few of the rows"));
    }
    Ok(q.mode.ef.unwrap_or(h.ef_search).max(q.k))
}

struct Ctx<'a> {
    snap: &'a Snapshot,
    q: &'a Search<'a>,
    qnorm: f32,
    deleted: &'a FxHashSet<[u64; 3]>,
}

impl Ctx<'_> {
    fn accepted(&self, seg: &Segment, i: usize) -> bool {
        (self.q.graph)(seg.ids[i][2]) && !self.deleted.contains(&seg.ids[i])
    }

    fn hit(&self, seg: &Segment, i: usize) -> Option<Hit> {
        let [s, o, g] = seg.ids[i];
        score(
            self.q.metric,
            self.q.query,
            self.qnorm,
            seg.row(i),
            seg.norms[i],
        )
        .map(|score| Hit { s, o, g, score })
    }

    /// Every row, scored in chunks; each chunk keeps its best `k + 2` (a chunk shares at
    /// most one run of equal (s, o), or of one subject, with each neighbour, so merging
    /// cannot starve the result).
    fn exact(
        &self,
        seg: &Segment,
        info: &mut SearchInfo,
        check: &(dyn Fn() -> Result<()> + Sync),
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Vec<Hit>> {
        use rayon::prelude::*;
        let q = self.q;
        let keep = q.k + 2;
        let n = seg.rows();
        let chunk = 8192;
        let count = n.div_ceil(chunk);
        let part_cap = (keep * 2).min(chunk);
        let parts_bytes = count as u64
            * (size_of::<Result<(Vec<Hit>, u64)>>() as u64
                + part_cap as u64 * size_of::<Hit>() as u64);
        admit(parts_bytes)?;
        let parts: Vec<Result<(Vec<Hit>, u64)>> = (0..count)
            .into_par_iter()
            .map(|c| {
                check()?;
                let (s, e) = (c * chunk, ((c + 1) * chunk).min(n));
                let mut best: Vec<Hit> = Vec::with_capacity(part_cap);
                let mut prev: Option<(u64, u64)> = None;
                // the best row of the current subject (distinct:subject)
                let mut run: Option<Hit> = None;
                let mut scored = 0u64;
                let push = |best: &mut Vec<Hit>, h: Hit| {
                    best.push(h);
                    if best.len() >= keep * 2 {
                        best.select_nth_unstable_by(keep - 1, |a, b| better(q.metric, a, b));
                        best.truncate(keep);
                    }
                };
                for i in s..e {
                    if !self.accepted(seg, i) {
                        continue;
                    }
                    let [rs, ro, _] = seg.ids[i];
                    if q.dedup {
                        if prev == Some((rs, ro)) {
                            continue;
                        }
                        prev = Some((rs, ro));
                    }
                    scored += 1;
                    let Some(h) = self.hit(seg, i) else {
                        continue;
                    };
                    if q.distinct_subject {
                        match run {
                            Some(r) if r.s == h.s => {
                                if better(q.metric, &h, &r).is_lt() {
                                    run = Some(h);
                                }
                            }
                            Some(r) => {
                                push(&mut best, r);
                                run = Some(h);
                            }
                            None => run = Some(h),
                        }
                    } else {
                        push(&mut best, h);
                    }
                }
                if let Some(r) = run {
                    push(&mut best, r);
                }
                Ok((best, scored))
            })
            .collect();
        let rows = parts
            .iter()
            .map(|p| p.as_ref().map_or(0, |(h, _)| h.len()))
            .sum::<usize>();
        admit(parts_bytes.saturating_add(rows as u64 * size_of::<Hit>() as u64))?;
        let mut all = Vec::with_capacity(rows);
        for p in parts {
            let (h, n) = p?;
            all.extend(h);
            info.scored += n;
        }
        Ok(all)
    }

    /// The rows of the candidate subjects only.
    fn subjects(
        &self,
        seg: &Segment,
        subjects: &[u64],
        info: &mut SearchInfo,
        check: &(dyn Fn() -> Result<()> + Sync),
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Vec<Hit>> {
        let mut out = Vec::new();
        for (j, &s) in subjects.iter().enumerate() {
            if j % 4096 == 4095 {
                check()?;
            }
            let mut prev: Option<u64> = None;
            for i in seg.subject_rows(s) {
                if !self.accepted(seg, i) {
                    continue;
                }
                let o = seg.ids[i][1];
                if self.q.dedup && prev == Some(o) {
                    continue;
                }
                prev = Some(o);
                info.scored += 1;
                if let Some(hit) = self.hit(seg, i) {
                    push_hit(&mut out, hit, admit)?;
                }
            }
        }
        Ok(out)
    }

    /// The graphs of the snapshot's inserted rows of live node `x`'s pair that the
    /// search accepts.
    fn live_rows<'s>(&'s self, live: &LiveState, x: u32) -> impl Iterator<Item = u64> + 's {
        let (s, o) = live.key(x);
        let p = self.q.pred;
        self.snap.delta.ins[Perm::Pso.index()]
            .range([p, s, o, 0]..=[p, s, o, u64::MAX])
            .map(|k| k[3])
            .filter(|&g| (self.q.graph)(g))
    }

    /// The graph's candidates (`None`: too few, while more rows are accepted). `live` is
    /// the graph's live layer, which holds `covered` of the snapshot's inserted rows in
    /// scope.
    #[allow(clippy::too_many_arguments)]
    fn ann(
        &self,
        b: &Built,
        live: &LiveState,
        covered: u64,
        ef: usize,
        info: &mut SearchInfo,
        check: &(dyn Fn() -> Result<()> + Sync),
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Option<Vec<Hit>>> {
        let q = self.q;
        let seg = &*b.segment;
        let g = b.graph.as_ref().expect("planned with a graph");
        let view = View { graph: g, live };
        let base = live.base();
        let nodes = &b.node_rows;
        let dist = |x: u32| {
            if x < base {
                let r = nodes[x as usize] as usize;
                graph_dist(q.metric, q.query, self.qnorm, seg.row(r), seg.norms[r])
            } else {
                let (v, n) = live.vector(x);
                graph_dist(q.metric, q.query, self.qnorm, v, n)
            }
        };
        let accept = |x: u32| {
            if x < base {
                let r = nodes[x as usize] as usize;
                (r..seg.run_end(r)).any(|i| self.accepted(seg, i))
            } else {
                self.live_rows(live, x).next().is_some()
            }
        };
        let accepted = b.rows_in(q.graph).saturating_sub(self.deleted.len() as u64) + covered;
        let mut ef = ef;
        loop {
            check()?;
            let found =
                hnsw::search_with_admit(&view, &dist, ef, &accept, (2 * g.m).max(64), admit)?;
            let found_bytes = found.capacity() as u64 * size_of::<(f32, u32)>() as u64;
            let mut stage_admit = |b: u64| admit(found_bytes.saturating_add(b));
            stage_admit(found.len() as u64 * size_of::<Hit>() as u64)?;
            let mut hits = Vec::with_capacity(found.len());
            for (_, x) in &found {
                if *x >= base {
                    let (s, o) = live.key(*x);
                    let (v, n) = live.vector(*x);
                    for g in self.live_rows(live, *x) {
                        info.scored += 1;
                        if let Some(score) = score(q.metric, q.query, self.qnorm, v, n) {
                            push_hit(&mut hits, Hit { s, o, g, score }, &mut stage_admit)?;
                        }
                        if q.dedup {
                            break;
                        }
                    }
                    continue;
                }
                let r = nodes[*x as usize] as usize;
                for i in r..seg.run_end(r) {
                    if !self.accepted(seg, i) {
                        continue;
                    }
                    info.scored += 1;
                    if let Some(hit) = self.hit(seg, i) {
                        push_hit(&mut hits, hit, &mut stage_admit)?;
                    }
                    if q.dedup {
                        break;
                    }
                }
            }
            let distinct = if q.distinct_subject {
                let count = hits.len().min(q.k);
                stage_admit(hit_bytes(&hits)
                    .saturating_add(table_bytes(table_capacity(count), size_of::<u64>()) as u64))?;
                let mut subjects = FxHashSet::default();
                subjects.reserve(count);
                for hit in &hits {
                    subjects.insert(hit.s);
                    if subjects.len() == count {
                        break;
                    }
                }
                subjects.len()
            } else {
                hits.len()
            };
            if distinct >= q.k || (hits.len() as u64) >= accepted {
                info.ef = ef;
                return Ok(Some(hits));
            }
            if ef >= super::config::MAX_EF.max(q.k * 8) {
                return Ok(None);
            }
            ef = (ef * 4).min(super::config::MAX_EF.max(q.k * 8));
        }
    }
}

/// The overlay counts of predicate `p` in `snap` (status reports).
pub(crate) fn overlay_counts(snap: &Snapshot, p: Option<u64>) -> (u64, u64) {
    let Some(p) = p else {
        return (0, 0);
    };
    let pi = Perm::Pso.index();
    let lo = [p, 0, 0, 0];
    let hi = [p, u64::MAX, u64::MAX, u64::MAX];
    (
        snap.delta.ins[pi].count_between(&lo, &hi) as u64,
        snap.delta.del[pi].count_between(&lo, &hi) as u64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::Source;
    use crate::store::{Store, StoreOptions};
    use oxrdfio::RdfFormat;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn r2_exact_partitions_are_admitted_before_scoring() {
        let store = Store::in_memory(StoreOptions::default());
        let data = (0..9000)
            .map(|i| format!("<urn:s:{i}> <urn:emb> \"[1,0,0]\"^^<urn:x-sparkles:vector> .\n"))
            .collect::<String>();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let q = Search {
            pred: snap.lookup_iri("urn:emb").unwrap().0,
            query: &[1.0, 0.0, 0.0],
            k: 2,
            metric: Metric::Cosine,
            graph: &|_| true,
            dedup: false,
            distinct_subject: false,
            subjects: None,
            mode: SearchMode::default(),
        };
        let (hits, info) = search(&snap, &q, &|| Ok(())).unwrap();
        assert_eq!((hits.len(), info.method, info.scored), (2, "exact", 9000));

        let checks = AtomicUsize::new(0);
        let err = search_with_admit(
            &snap,
            &q,
            &|| {
                checks.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            &mut |bytes| {
                if bytes == 0 {
                    return Ok(());
                }
                assert!(bytes >= 2 * 8 * size_of::<Hit>() as u64);
                Err(Error::Cancelled)
            },
        );
        assert!(matches!(err, Err(Error::Cancelled)));
        assert_eq!(checks.load(Ordering::Relaxed), 0);
    }
}
