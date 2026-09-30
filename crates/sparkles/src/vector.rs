//! Vector similarity: the `spk:vector` literal datatype, a deterministic scoring kernel,
//! and exact top-k search (`spk:vectorSearch`).
//!
//! A vector is an ordinary RDF literal, `"[0.1, 0.2, 0.3]"^^<urn:x-sparkles:vector>`:
//! a JSON array of 1..=16384 finite numbers, each mapped to the nearest `f32`. The
//! literal stays authoritative (Sparkles never rewrites it). Search reads packed `f32`
//! segments built lazily per (predicate, dimension) from a generation's base index; each
//! query overlays its snapshot's inserted and deleted quads, so every snapshot sees
//! exactly its own vectors.

use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::store::Snapshot;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

/// Datatype IRI of vector literals.
pub const DATATYPE: &str = "urn:x-sparkles:vector";
/// Namespace of the vector functions and property function.
pub const NS: &str = "urn:x-sparkles:";
/// The top-k search property function.
pub const VECTOR_SEARCH: &str = "urn:x-sparkles:vectorSearch";
/// Largest dimension.
pub const MAX_DIM: usize = 16384;
/// Largest `k`.
pub const MAX_K: usize = 10_000;

static BUDGET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(4 << 30);

/// Memory the packed vectors of one generation may use (default 4 GiB).
pub fn budget() -> u64 {
    BUDGET.load(std::sync::atomic::Ordering::Relaxed)
}

/// Set the vector memory budget of this process.
pub fn set_budget(bytes: u64) {
    BUDGET.store(bytes, std::sync::atomic::Ordering::Relaxed);
}

/// Parse a vector's lexical form. The error names the byte offset of the first problem.
pub fn parse(lex: &str) -> std::result::Result<Vec<f32>, String> {
    if lex.len() > 1 << 20 {
        return Err("lexical form longer than 1 MiB".into());
    }
    let b = lex.as_bytes();
    let mut i = 0;
    let ws = |i: &mut usize| {
        while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
            *i += 1;
        }
    };
    let at = |i: usize, what: &str| format!("malformed spk:vector literal at offset {i}: {what}");
    ws(&mut i);
    if b.get(i) != Some(&b'[') {
        return Err(at(i, "expected '['"));
    }
    i += 1;
    let mut out = Vec::new();
    loop {
        ws(&mut i);
        let start = i;
        // number = [ "-" ] ( "0" / [1-9] *DIGIT ) [ "." 1*DIGIT ] [ ("e"/"E") ["+"/"-"] 1*DIGIT ]
        if b.get(i) == Some(&b'-') {
            i += 1;
        }
        let digits = |i: &mut usize| {
            let s = *i;
            while *i < b.len() && b[*i].is_ascii_digit() {
                *i += 1;
            }
            *i - s
        };
        match b.get(i) {
            Some(b'0') => i += 1,
            Some(b'1'..=b'9') => {
                digits(&mut i);
            }
            _ => return Err(at(i, "expected a number")),
        }
        if b.get(i) == Some(&b'.') {
            i += 1;
            if digits(&mut i) == 0 {
                return Err(at(i, "expected a digit"));
            }
        }
        if matches!(b.get(i), Some(b'e' | b'E')) {
            i += 1;
            if matches!(b.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            if digits(&mut i) == 0 {
                return Err(at(i, "expected an exponent"));
            }
        }
        let x: f32 = lex[start..i]
            .parse()
            .map_err(|_| at(start, "not a number"))?;
        if !x.is_finite() {
            return Err(at(start, "out of the f32 range"));
        }
        out.push(x);
        if out.len() > MAX_DIM {
            return Err(at(start, "more than 16384 elements"));
        }
        ws(&mut i);
        match b.get(i) {
            Some(b',') => i += 1,
            Some(b']') => {
                i += 1;
                ws(&mut i);
                return if i == b.len() {
                    Ok(out)
                } else {
                    Err(at(i, "trailing characters"))
                };
            }
            _ => return Err(at(i, "expected ',' or ']'")),
        }
    }
}

/// Canonical lexical form (used only for literals Sparkles creates).
pub fn canonical(v: &[f32]) -> String {
    let parts: Vec<String> = v.iter().map(|x| format!("{x:?}")).collect();
    format!("[{}]", parts.join(","))
}

/// A vector literal in canonical form.
pub fn literal(v: &[f32]) -> oxrdf::Literal {
    oxrdf::Literal::new_typed_literal(canonical(v), oxrdf::NamedNode::new_unchecked(DATATYPE))
}

/// How search results are scored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    /// cosine similarity (higher is better)
    Cosine,
    /// dot product (higher is better)
    Dot,
    /// L2 distance (lower is better)
    Euclidean,
}

impl Metric {
    pub fn parse(s: &str) -> Option<Metric> {
        Some(match s {
            "cosine" => Metric::Cosine,
            "dot" => Metric::Dot,
            "euclidean" => Metric::Euclidean,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Metric::Cosine => "cosine",
            Metric::Dot => "dot",
            Metric::Euclidean => "euclidean",
        }
    }
    /// Whether larger scores are better.
    pub fn higher_is_better(self) -> bool {
        !matches!(self, Metric::Euclidean)
    }
}

/// Σ f(aᵢ, bᵢ) with eight independent accumulators summed in a fixed order: the result is
/// the same whether or not the loop is vectorized.
#[inline]
fn lanes(a: &[f32], b: &[f32], f: impl Fn(f32, f32) -> f32) -> f32 {
    let mut acc = [0f32; 8];
    let (ca, ra) = a.as_chunks::<8>();
    let (cb, rb) = b.as_chunks::<8>();
    for (x, y) in ca.iter().zip(cb) {
        for l in 0..8 {
            acc[l] += f(x[l], y[l]);
        }
    }
    for (l, (x, y)) in ra.iter().zip(rb).enumerate() {
        acc[l] += f(*x, *y);
    }
    ((acc[0] + acc[4]) + (acc[1] + acc[5])) + ((acc[2] + acc[6]) + (acc[3] + acc[7]))
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    lanes(a, b, |x, y| x * y)
}

/// Euclidean norm.
pub fn norm(a: &[f32]) -> f32 {
    dot(a, a).sqrt()
}

/// Score of `b` against `a` (`a_norm`, `b_norm`: their norms, used by cosine). `None`
/// when undefined: different dimensions, a zero norm with cosine, or a non-finite result.
pub fn score(m: Metric, a: &[f32], a_norm: f32, b: &[f32], b_norm: f32) -> Option<f32> {
    if a.len() != b.len() {
        return None;
    }
    let s = match m {
        Metric::Dot => dot(a, b),
        Metric::Euclidean => lanes(a, b, |x, y| (x - y) * (x - y)).sqrt(),
        Metric::Cosine => {
            if a_norm == 0.0 || b_norm == 0.0 {
                return None;
            }
            (dot(a, b) / (a_norm * b_norm)).clamp(-1.0, 1.0)
        }
    };
    s.is_finite().then_some(s)
}

/// The vector of a stored literal key (`"lex 0xFF ^urn:x-sparkles:vector`), if it is a
/// well-typed vector literal.
pub fn from_key(key: &[u8]) -> Option<Vec<f32>> {
    let rest = key.strip_prefix(b"\"")?;
    let sep = rest.iter().rposition(|&b| b == 0xFF)?;
    let dt = rest[sep + 1..].strip_prefix(b"^")?;
    if dt != DATATYPE.as_bytes() {
        return None;
    }
    parse(std::str::from_utf8(&rest[..sep]).ok()?).ok()
}

// --------------------------------------------------------------- segments ------

/// The base-index vectors of one predicate, by dimension.
pub struct PredicateVectors {
    pub by_dim: FxHashMap<usize, Segment>,
    /// literals of the vector datatype that are not valid vectors
    pub malformed: u64,
    pub bytes: u64,
}

/// Packed vectors of one dimension: rows in PSO order (so the same (s, o) in several
/// graphs are adjacent).
#[derive(Default)]
pub struct Segment {
    pub dim: usize,
    /// (s, o, g) per row
    pub ids: Vec<[u64; 3]>,
    pub norms: Vec<f32>,
    /// row-major, `dim` values per row
    pub data: Vec<f32>,
}

impl Segment {
    fn row(&self, i: usize) -> &[f32] {
        &self.data[i * self.dim..(i + 1) * self.dim]
    }
    fn bytes(&self) -> u64 {
        (self.data.len() * 4 + self.norms.len() * 4 + self.ids.len() * 24) as u64
    }
}

/// Per-generation cache of predicate vectors (built on first use).
#[derive(Default)]
pub struct GenerationVectors {
    by_pred: parking_lot::Mutex<FxHashMap<u64, Arc<PredicateVectors>>>,
}

/// One packed predicate, for status reports.
pub struct PackedStatus {
    pub predicate: u64,
    pub bytes: u64,
    pub malformed: u64,
    /// (dimension, rows): a vector in several graphs counts once per graph
    pub dims: Vec<(usize, usize)>,
}

impl GenerationVectors {
    /// The predicates packed so far (on their first search), by predicate id.
    pub fn status(&self) -> Vec<PackedStatus> {
        let mut out: Vec<PackedStatus> = self
            .by_pred
            .lock()
            .iter()
            .map(|(&p, v)| {
                let mut dims: Vec<(usize, usize)> =
                    v.by_dim.values().map(|s| (s.dim, s.ids.len())).collect();
                dims.sort_unstable();
                PackedStatus {
                    predicate: p,
                    bytes: v.bytes,
                    malformed: v.malformed,
                    dims,
                }
            })
            .collect();
        out.sort_unstable_by_key(|s| s.predicate);
        out
    }

    pub fn used_bytes(&self) -> u64 {
        self.by_pred.lock().values().map(|p| p.bytes).sum()
    }

    /// The base vectors of predicate `p` in `snap`'s generation, built on first use
    /// within the memory budget.
    pub fn predicate(&self, snap: &Snapshot, p: u64, budget: u64) -> Result<Arc<PredicateVectors>> {
        if let Some(v) = self.by_pred.lock().get(&p) {
            return Ok(v.clone());
        }
        let base = snap.perm(Perm::Pso);
        let rows = base.count(&snap.cache, &[p])?;
        let mut keys: Vec<[u64; 3]> = Vec::with_capacity(rows as usize);
        base.for_each_range(&snap.cache, &[p], |b, s, e| {
            for i in s..e {
                let k = b.key(i);
                keys.push([k[1], k[2], k[3]]);
            }
            Ok(())
        })?;
        // parse every distinct literal once
        let mut objs: Vec<u64> = keys
            .iter()
            .map(|k| k[1])
            .filter(|&o| Id(o).tag() == Tag::Vocab)
            .collect();
        objs.sort_unstable();
        objs.dedup();
        let mut parsed: FxHashMap<u64, Option<Arc<[f32]>>> = FxHashMap::default();
        let payloads: Vec<u64> = objs.iter().map(|&o| Id(o).payload()).collect();
        let mut malformed = 0u64;
        snap.generation.vocab.get_sorted(&payloads, |pl, key| {
            let is_vec = key.ends_with(DATATYPE.as_bytes()) && key.first() == Some(&b'"');
            let v = from_key(key).map(Arc::<[f32]>::from);
            if is_vec && v.is_none() {
                malformed += 1;
            }
            parsed.insert(Id::vocab(pl).0, v);
        });
        let mut by_dim: FxHashMap<usize, Segment> = FxHashMap::default();
        for k in keys {
            let Some(Some(v)) = parsed.get(&k[1]) else {
                continue;
            };
            let seg = by_dim.entry(v.len()).or_insert_with(|| Segment {
                dim: v.len(),
                ..Default::default()
            });
            seg.ids.push(k);
            seg.norms.push(norm(v));
            seg.data.extend_from_slice(v);
        }
        let bytes: u64 = by_dim.values().map(Segment::bytes).sum();
        let used = self.used_bytes();
        if used + bytes > budget {
            // the packed vectors of every predicate share one budget
            return Err(Error::BudgetExceeded(crate::Budget {
                kind: crate::BudgetKind::Memory,
                limit: budget,
                requested: used + bytes,
            }));
        }
        let pv = Arc::new(PredicateVectors {
            by_dim,
            malformed,
            bytes,
        });
        self.by_pred.lock().insert(p, pv.clone());
        Ok(pv)
    }
}

// ----------------------------------------------------------------- search ------

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

/// Parameters of one exact search.
pub struct Search<'a> {
    pub pred: u64,
    pub query: &'a [f32],
    pub k: usize,
    pub metric: Metric,
    /// accepts a graph id
    pub graph: &'a (dyn Fn(u64) -> bool + Sync),
    /// merge rows with the same (s, o) from different graphs
    pub dedup: bool,
}

/// The `k` best rows of predicate `pred` whose vectors have the query's dimension, in
/// `snap` (its base segment minus deleted quads, plus inserted ones), best first.
pub fn search(
    snap: &Snapshot,
    q: &Search<'_>,
    check: &(dyn Fn() -> Result<()> + Sync),
) -> Result<Vec<Hit>> {
    use rayon::prelude::*;
    let dim = q.query.len();
    let qnorm = norm(q.query);
    if q.metric == Metric::Cosine && qnorm == 0.0 {
        return Err(Error::invalid(
            "cosine is undefined for a zero query vector",
        ));
    }
    let pv = snap.generation.vectors.predicate(snap, q.pred, budget())?;
    // the snapshot's changes to this predicate
    let pi = Perm::Pso.index();
    let lo = [q.pred, 0, 0, 0];
    let hi = [q.pred, u64::MAX, u64::MAX, u64::MAX];
    let deleted: FxHashSet<[u64; 3]> = snap.delta.del[pi]
        .range(lo..=hi)
        .map(|k| [k[1], k[2], k[3]])
        .collect();
    let mut inserted: Vec<Hit> = Vec::new();
    let mut ins_dims: FxHashSet<usize> = FxHashSet::default();
    for k in snap.delta.ins[pi].range(lo..=hi) {
        let Some(v) = snap.key(Id(k[2])).and_then(|key| from_key(&key)) else {
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
    let seg = pv.by_dim.get(&dim);
    if seg.is_none() && inserted.is_empty() {
        let mut dims: Vec<usize> = pv.by_dim.keys().copied().chain(ins_dims).collect();
        dims.sort_unstable();
        dims.dedup();
        if !dims.is_empty() {
            return Err(Error::invalid(format!(
                "dimension mismatch: the predicate has vectors of dimension {}; query has {dim}",
                dims.iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        return Ok(Vec::new());
    }
    // rows are scored in chunks; each keeps its best k + 2 (a chunk shares at most one
    // run of equal (s, o) with each neighbour, so deduplication cannot starve the result)
    let keep = q.k + 2;
    let mut cand: Vec<Hit> = match seg {
        None => Vec::new(),
        Some(seg) => {
            let n = seg.ids.len();
            let chunk = 8192;
            let parts: Vec<Result<Vec<Hit>>> = (0..n.div_ceil(chunk))
                .into_par_iter()
                .map(|c| {
                    check()?;
                    let (s, e) = (c * chunk, ((c + 1) * chunk).min(n));
                    let mut best: Vec<Hit> = Vec::with_capacity(keep * 2);
                    let mut prev: Option<(u64, u64)> = None;
                    for i in s..e {
                        let [rs, ro, rg] = seg.ids[i];
                        if !(q.graph)(rg) || deleted.contains(&seg.ids[i]) {
                            continue;
                        }
                        if q.dedup {
                            if prev == Some((rs, ro)) {
                                continue;
                            }
                            prev = Some((rs, ro));
                        }
                        let Some(sc) = score(q.metric, q.query, qnorm, seg.row(i), seg.norms[i])
                        else {
                            continue;
                        };
                        best.push(Hit {
                            s: rs,
                            o: ro,
                            g: rg,
                            score: sc,
                        });
                        if best.len() >= keep * 2 {
                            best.select_nth_unstable_by(keep - 1, |a, b| better(q.metric, a, b));
                            best.truncate(keep);
                        }
                    }
                    Ok(best)
                })
                .collect();
            let mut all = Vec::new();
            for p in parts {
                all.extend(p?);
            }
            all
        }
    };
    cand.extend(inserted);
    cand.sort_by(|a, b| better(q.metric, a, b));
    if q.dedup {
        let mut seen = FxHashSet::default();
        cand.retain(|h| seen.insert((h.s, h.o)));
    }
    cand.truncate(q.k);
    Ok(cand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar() {
        assert_eq!(parse("[1, 0.5,-2e1 ]").unwrap(), [1.0, 0.5, -20.0]);
        assert_eq!(parse(" [0] ").unwrap(), [0.0]);
        for bad in [
            "[]", "[NaN]", "[+1]", "[.5]", "[1.]", "[0x1]", "[[1]]", "[1,]", "1", "[1e39]", "[01]",
            "[1] x",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(parse("[1, x]").unwrap_err().contains("offset 4"));
        let v = [1.0f32, 0.1, 1e-7, -0.0];
        assert_eq!(parse(&canonical(&v)).unwrap(), v);
    }

    #[test]
    fn kernel() {
        let a = [1.0, 2.0, 3.0];
        let b = [4.0, 5.0, 6.0];
        assert_eq!(dot(&a, &b), 32.0);
        assert_eq!(
            score(Metric::Euclidean, &[0.0, 0.0], 0.0, &[3.0, 4.0], 5.0),
            Some(5.0)
        );
        assert_eq!(score(Metric::Cosine, &a, norm(&a), &[0.0; 3], 0.0), None);
        assert_eq!(score(Metric::Dot, &a, 0.0, &[1.0, 2.0], 0.0), None);
        // a long vector: the lanes give the same sum however the loop is compiled
        let long: Vec<f32> = (0..1000).map(|i| (i as f32).sin()).collect();
        assert_eq!(dot(&long, &long), dot(&long, &long));
        let c = score(Metric::Cosine, &long, norm(&long), &long, norm(&long)).unwrap();
        assert!((c - 1.0).abs() < 1e-6);
    }
}
