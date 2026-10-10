//! Top-k search over a snapshot: the base rows of a segment (scanned exactly, through
//! the HNSW graph of a configured index, or only those of candidate subjects), minus the
//! snapshot's deleted quads, plus its inserted ones scored exactly.
//!
//! Every path scores with the same kernel ([`super::score`]), so a row's score does not
//! depend on the path. The graph only chooses which base rows are scored: with it, the
//! result is the best rows among the graph's candidates and the inserted rows.

use super::config::SearchMode;
use super::index::{Built, Segment};
use super::{Metric, budget, graph_dist, norm, score};
use crate::error::{Error, Result};
use crate::index::Perm;
use crate::store::Snapshot;
use rustc_hash::FxHashSet;
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
    pub inserted: u64,
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
    let deleted: FxHashSet<[u64; 3]> = snap.delta.del[pi]
        .range(lo..=hi)
        .map(|k| [k[1], k[2], k[3]])
        .collect();
    info.deleted = deleted.len() as u64;
    let mut inserted: Vec<Hit> = Vec::new();
    let mut ins_dims: FxHashSet<usize> = FxHashSet::default();
    for k in snap.delta.ins[pi].range(lo..=hi) {
        if q.subjects.is_some_and(|s| s.binary_search(&k[1]).is_err()) {
            continue;
        }
        let Some(v) = gv.overlay_vector(snap, k[2]) else {
            continue;
        };
        ins_dims.insert(v.len());
        if v.len() != dim || !(q.graph)(k[3]) {
            continue;
        }
        if let Some(sc) = score(q.metric, q.query, qnorm, &v, norm(&v)) {
            inserted.push(Hit {
                s: k[1],
                o: k[2],
                g: k[3],
                score: sc,
            });
        }
    }
    info.inserted = inserted.len() as u64;
    let seg = base.segment(dim);
    if seg.is_none() && inserted.is_empty() {
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
        return Ok((Vec::new(), info));
    }
    info.rows = seg.map_or(0, |s| s.rows() as u64);
    let ctx = Ctx {
        q,
        qnorm,
        deleted: &deleted,
    };
    let mut cand: Vec<Hit> = match seg {
        None => Vec::new(),
        Some(seg) => match (q.subjects, &base) {
            (Some(subjects), _) => {
                info.method = "candidates";
                ctx.subjects(seg, subjects, &mut info, check)?
            }
            (None, Base::Index(b)) => match ann_plan(snap, b, q, &deleted) {
                Ok(ef) => {
                    info.method = "hnsw";
                    info.ef = ef;
                    match ctx.ann(b, ef, &mut info, check)? {
                        Some(h) => h,
                        None => {
                            // the graph found too few rows: fall back
                            info.method = "exact";
                            info.reason = Some("the graph found fewer than k rows");
                            ctx.exact(seg, &mut info, check)?
                        }
                    }
                }
                Err(reason) => {
                    info.reason = reason;
                    ctx.exact(seg, &mut info, check)?
                }
            },
            (None, _) => ctx.exact(seg, &mut info, check)?,
        },
    };
    cand.extend(inserted);
    Ok((finish(q, cand), info))
}

/// Sort, deduplicate and cut the candidates.
fn finish(q: &Search<'_>, mut cand: Vec<Hit>) -> Vec<Hit> {
    cand.sort_by(|a, b| better(q.metric, a, b));
    if q.distinct_subject {
        let mut seen = FxHashSet::default();
        cand.retain(|h| seen.insert(h.s));
    } else if q.dedup {
        let mut seen = FxHashSet::default();
        cand.retain(|h| seen.insert((h.s, h.o)));
    }
    cand.truncate(q.k);
    cand
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
    if accepted as usize <= b.config.exact_threshold.max(q.k) {
        return Err(Some("few rows"));
    }
    // a graph filter that rejects most nodes makes the graph walk most of them
    if accepted.saturating_mul(20) < g.nodes as u64 {
        return Err(Some("the active graph holds few of the rows"));
    }
    Ok(q.mode.ef.unwrap_or(h.ef_search).max(q.k))
}

struct Ctx<'a> {
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
    ) -> Result<Vec<Hit>> {
        use rayon::prelude::*;
        let q = self.q;
        let keep = q.k + 2;
        let n = seg.rows();
        let chunk = 8192;
        let parts: Vec<Result<(Vec<Hit>, u64)>> = (0..n.div_ceil(chunk))
            .into_par_iter()
            .map(|c| {
                check()?;
                let (s, e) = (c * chunk, ((c + 1) * chunk).min(n));
                let mut best: Vec<Hit> = Vec::with_capacity(keep * 2);
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
        let mut all = Vec::new();
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
                out.extend(self.hit(seg, i));
            }
        }
        Ok(out)
    }

    /// The graph's candidates (`None`: too few, while more rows are accepted).
    fn ann(
        &self,
        b: &Built,
        ef: usize,
        info: &mut SearchInfo,
        check: &(dyn Fn() -> Result<()> + Sync),
    ) -> Result<Option<Vec<Hit>>> {
        let q = self.q;
        let seg = &*b.segment;
        let g = b.graph.as_ref().expect("planned with a graph");
        let nodes = &b.node_rows;
        let dist = |x: u32| {
            let r = nodes[x as usize] as usize;
            graph_dist(q.metric, q.query, self.qnorm, seg.row(r), seg.norms[r])
        };
        let accept = |x: u32| {
            let r = nodes[x as usize] as usize;
            (r..seg.run_end(r)).any(|i| self.accepted(seg, i))
        };
        let accepted = b.rows_in(q.graph).saturating_sub(self.deleted.len() as u64);
        let mut ef = ef;
        loop {
            check()?;
            let found = g.search(&dist, ef, &accept);
            let mut hits = Vec::with_capacity(found.len());
            for (_, x) in &found {
                let r = nodes[*x as usize] as usize;
                for i in r..seg.run_end(r) {
                    if !self.accepted(seg, i) {
                        continue;
                    }
                    info.scored += 1;
                    hits.extend(self.hit(seg, i));
                    if q.dedup {
                        break;
                    }
                }
            }
            let distinct = if q.distinct_subject {
                hits.iter().map(|h| h.s).collect::<FxHashSet<_>>().len()
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
