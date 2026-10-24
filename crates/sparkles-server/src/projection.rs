//! Projections of vector indexes (spec U11 §3.3): `POST /$/vector/{ds}/{name}/projection`
//! computes one and keeps it, `GET` answers it, whole or by region, as JSON or as the
//! binary points format.
//!
//! * **Running.** PCA of an index of up to [`IN_REQUEST`] vectors runs in the request,
//!   under its `timeout`. A larger PCA and every neighbour layout run as a cancellable task
//!   of kind `vector-projection`, and the `POST` answers `202` with the task.
//! * **Keeping.** The server keeps at most [`KEEP`] projections, [`KEEP_BYTES`] in all,
//!   keyed by dataset, branch, index and method, and drops the least recently used first.
//!   Each holds a grant of the query memory pool for its bytes, when the server has a pool,
//!   and a projection the pool cannot hold is refused with `507`. A kept projection also
//!   holds the snapshot it read, whose dictionary names its points, so a compaction's old
//!   generation stays readable until the projection is replaced or dropped.
//! * **Staleness.** A projection read at an older commit than the head is answered with
//!   `stale: true` until a new one is computed.
//! * **Access.** The routes need `read`, and they are in the `whole_dataset` list of
//!   `auth/routes.rs`, so a caller whose access covers part of the dataset gets `403`: a
//!   point reveals that an entity exists and what it is like.

use crate::http::{ApiResult, blocking, dataset, err};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use parking_lot::Mutex;
use serde_json::{Value as J, json};
use sparkles::id::Id;
use sparkles::vector::projection::{GRID, MAX_POINTS, NO_CLASS};
use sparkles::vector::{Projection, ProjectionConfig, ProjectionMethod};
use std::sync::Arc;
use std::time::Instant;

type St = State<Arc<crate::state::AppState>>;

/// The task kind of projections.
pub const TASK: &str = "vector-projection";
/// The most vectors a PCA projects within the request.
pub const IN_REQUEST: usize = 1_000_000;
/// The most projections the server keeps.
pub const KEEP: usize = 4;
/// The most bytes the kept projections take.
pub const KEEP_BYTES: u64 = 1 << 30;
/// The media type of the binary points format.
pub const POINTS_TYPE: &str = "application/x-sparkles-points";
/// The binary format's magic and version: a reader refuses any other.
pub const POINTS_MAGIC: &[u8; 8] = b"SPKPTS\0\x01";

#[derive(Clone, PartialEq, Eq, Debug)]
struct Key {
    dataset: uuid::Uuid,
    branch: uuid::Uuid,
    name: String,
    index: String,
    method: ProjectionMethod,
}

struct Kept {
    key: Key,
    projection: Arc<Projection>,
    _grant: Option<sparkles::sparql::pool::PoolGrant>,
    used: Instant,
}

/// The projections a server keeps.
#[derive(Default)]
pub struct Projections(Mutex<Vec<Kept>>);

/// The kept projections and their bytes, for `/metrics`.
pub fn kept(st: &crate::state::AppState) -> (usize, u64) {
    let k = st.projections.0.lock();
    (k.len(), k.iter().map(|e| e.projection.bytes()).sum())
}

/// `sparkles_vector_projections` and `sparkles_vector_projection_bytes`.
pub fn metrics(st: &crate::state::AppState, out: &mut String) {
    use std::fmt::Write;
    let (n, bytes) = kept(st);
    let _ = writeln!(
        out,
        "# HELP sparkles_vector_projections Vector index projections the server keeps.\n# TYPE sparkles_vector_projections gauge\nsparkles_vector_projections {n}"
    );
    let _ = writeln!(
        out,
        "# HELP sparkles_vector_projection_bytes Bytes of the vector index projections the server keeps.\n# TYPE sparkles_vector_projection_bytes gauge\nsparkles_vector_projection_bytes {bytes}"
    );
}

fn key_of(ds: &crate::state::Dataset, index: &str, method: ProjectionMethod) -> Key {
    Key {
        dataset: ds.store.dataset_id(),
        branch: ds.store.branch_id(),
        name: ds.name.clone(),
        index: index.to_string(),
        method,
    }
}

/// Drop the kept projections of datasets that are gone or were replaced.
fn sweep(st: &crate::state::AppState, kept: &mut Vec<Kept>) {
    kept.retain(|e| {
        st.datasets().get(&e.key.name).is_some_and(|d| {
            d.store.dataset_id() == e.key.dataset && d.store.branch_id() == e.key.branch
        })
    });
}

/// Keep `p` under `key`, replacing the one there and dropping the least recently used
/// ones past the limits.
fn keep(st: &crate::state::AppState, key: Key, p: Arc<Projection>) -> ApiResult<()> {
    let bytes = p.bytes();
    if bytes > KEEP_BYTES {
        return Err(too_big(bytes));
    }
    let mut kept = st.projections.0.lock();
    sweep(st, &mut kept);
    kept.retain(|e| e.key != key);
    while kept.len() >= KEEP
        || kept.iter().map(|e| e.projection.bytes()).sum::<u64>() + bytes > KEEP_BYTES
    {
        let Some(oldest) = kept
            .iter()
            .enumerate()
            .min_by_key(|(_, e)| e.used)
            .map(|(i, _)| i)
        else {
            break;
        };
        kept.remove(oldest);
    }
    let grant = match st.catalog.pool() {
        None => None,
        Some(pool) => Some(pool.try_grant(bytes).ok_or_else(|| {
            err(
                StatusCode::INSUFFICIENT_STORAGE,
                format!(
                    "the query memory pool cannot hold the projection's {bytes} bytes now; try again when fewer queries run"
                ),
            )
        })?),
    };
    kept.push(Kept {
        key,
        projection: p,
        _grant: grant,
        used: Instant::now(),
    });
    Ok(())
}

fn too_big(bytes: u64) -> crate::http::ApiError {
    err(
        StatusCode::INSUFFICIENT_STORAGE,
        format!(
            "a projection of this index takes about {bytes} bytes, more than the {KEEP_BYTES} the server keeps for projections"
        ),
    )
}

/// The kept projection of `key`'s index: of `method`, or the most recent of any method.
fn find(
    st: &crate::state::AppState,
    ds: &crate::state::Dataset,
    index: &str,
    method: Option<ProjectionMethod>,
) -> Option<Arc<Projection>> {
    let mut kept = st.projections.0.lock();
    sweep(st, &mut kept);
    let want = |k: &Key| {
        k.dataset == ds.store.dataset_id()
            && k.branch == ds.store.branch_id()
            && k.index == index
            && method.is_none_or(|m| k.method == m)
    };
    let e = kept
        .iter_mut()
        .filter(|e| want(&e.key))
        .max_by_key(|e| e.used)?;
    e.used = Instant::now();
    Some(e.projection.clone())
}

fn params(uri: &Uri) -> Vec<(String, String)> {
    uri.query()
        .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
        .unwrap_or_default()
}

fn param<'a>(p: &'a [(String, String)], k: &str) -> Option<&'a str> {
    p.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
}

fn method_param(p: &[(String, String)]) -> ApiResult<Option<ProjectionMethod>> {
    match param(p, "method") {
        None => Ok(None),
        Some(m) => ProjectionMethod::parse(m).map(Some).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                format!("method: unknown method {m:?}: \"pca\" or \"neighbours\""),
            )
        }),
    }
}

fn unknown_index(st: &crate::state::Dataset, name: &str) -> ApiResult<usize> {
    let s = st
        .dataset
        .indexes()
        .vector()
        .get(name)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("no vector index {name}")))?;
    Ok(s.rows as usize)
}

/// `POST /$/vector/{ds}/{name}/projection?method=pca|neighbours&timeout=`: compute the
/// projection and keep it. The body, optional, is `{"sample": n, "seed": n,
/// "iterations": n}`.
pub async fn compute(
    State(st): St,
    Path((ds_name, name)): Path<(String, String)>,
    uri: Uri,
    body: axum::body::Bytes,
) -> ApiResult {
    let ds = dataset(&st, &ds_name)?;
    let p = params(&uri);
    let method = method_param(&p)?.unwrap_or(ProjectionMethod::Pca);
    let mut cfg = ProjectionConfig {
        method,
        ..Default::default()
    };
    if !body.is_empty() {
        let j: J = serde_json::from_slice(&body)
            .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
        let o = j
            .as_object()
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "the body must be a JSON object"))?;
        for (k, v) in o {
            let n = v.as_u64().ok_or_else(|| {
                err(
                    StatusCode::BAD_REQUEST,
                    format!("{k} must be a whole number"),
                )
            })?;
            match k.as_str() {
                "sample" if (1..=sparkles::vector::projection::MAX_SAMPLE as u64).contains(&n) => {
                    cfg.sample = n as usize
                }
                "sample" => {
                    return Err(err(
                        StatusCode::BAD_REQUEST,
                        format!("sample: 1 to {}", sparkles::vector::projection::MAX_SAMPLE),
                    ));
                }
                "seed" => cfg.seed = n,
                "iterations" if (1..=1000).contains(&n) => cfg.iterations = n as usize,
                "iterations" => {
                    return Err(err(StatusCode::BAD_REQUEST, "iterations: 1 to 1000"));
                }
                _ => {
                    return Err(err(
                        StatusCode::BAD_REQUEST,
                        format!("unknown member {k:?}: sample, seed, iterations"),
                    ));
                }
            }
        }
    }
    let rows = unknown_index(&ds, &name)?;
    let estimate = rows as u64 * sparkles::vector::projection::BYTES_PER_POINT;
    if estimate > KEEP_BYTES {
        return Err(too_big(estimate));
    }
    let key = key_of(&ds, &name, method);
    if method == ProjectionMethod::Pca && rows <= IN_REQUEST {
        let params = crate::http::Params::from_query(&uri);
        let timeout = crate::http::timeout_param(&st, &params);
        let (cancel, _guard) = crate::http::cancel_on_drop();
        let st2 = st.clone();
        let ds2 = ds.clone();
        let p = blocking(move || {
            let ctl = sparkles::task::Control {
                cancel: sparkles::task::Cancel::from_flag(cancel),
                deadline: Some(Instant::now() + timeout),
                ..Default::default()
            };
            Ok(ds2
                .dataset
                .indexes()
                .vector()
                .projection(&name, &cfg, &ctl)?)
        })
        .await
        .map_err(|e| crate::http::with_timeout(e, Some(timeout)))?;
        let p = Arc::new(p);
        keep(&st2, key, p.clone())?;
        return Ok(Json(summary(&p, &ds)).into_response());
    }
    crate::http::task_start_check(&st, Some(TASK), &ds_name)?;
    let st2 = st.clone();
    let ds2 = ds.clone();
    let task = st.start_task_opts(st.next_task_id(), TASK, &ds_name, None, true, move |h| {
        let progress = {
            let h = h.clone();
            sparkles::task::Progress::new(move |f, m| h.progress(f, m))
        };
        let ctl = sparkles::task::Control {
            cancel: sparkles::task::Cancel::from_flag(h.cancel_flag()),
            progress,
            deadline: None,
        };
        let p = Arc::new(
            ds2.dataset
                .indexes()
                .vector()
                .projection(&name, &cfg, &ctl)?,
        );
        let n = p.len();
        keep(&st2, key, p).map_err(|e| anyhow::anyhow!("{}", e.message()))?;
        Ok(format!(
            "vector index {name}: {} projection of {n} entities",
            method.name()
        ))
    });
    let location = format!("/$/tasks/{}", task.id);
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        Json(task),
    )
        .into_response())
}

/// The projection's facts, without its points.
fn summary(p: &Projection, ds: &crate::state::Dataset) -> J {
    let head = ds.store.snapshot();
    json!({
        "index": p.index,
        "method": p.method.name(),
        "commit": p.commit(),
        "generation": p.generation(),
        "stale": head.commit != p.commit() || head.generation.name != p.generation(),
        "vectors": p.vectors,
        "dimension": p.dimension,
        "entities": p.len(),
        "bounds": p.bounds,
        "explained": p.explained,
        "builtMs": p.built_ms,
        "bytes": p.bytes(),
    })
}

fn term_text(p: &Projection, id: u64) -> String {
    match p.snapshot.term(Id(id)) {
        Some(oxrdf::Term::NamedNode(n)) => n.into_string(),
        Some(oxrdf::Term::BlankNode(b)) => format!("_:{}", b.as_str()),
        Some(t) => t.to_string(),
        None => String::new(),
    }
}

fn term_json(t: &oxrdf::Term) -> J {
    match t {
        oxrdf::Term::NamedNode(n) => json!({ "type": "uri", "value": n.as_str() }),
        oxrdf::Term::BlankNode(b) => json!({ "type": "bnode", "value": b.as_str() }),
        oxrdf::Term::Literal(l) => {
            let mut o = json!({ "type": "literal", "value": l.value() });
            if let Some(lang) = l.language() {
                o["xml:lang"] = lang.into();
            } else if l.datatype().as_str() != "http://www.w3.org/2001/XMLSchema#string" {
                o["datatype"] = l.datatype().as_str().into();
            }
            o
        }
        #[allow(unreachable_patterns)]
        _ => json!({ "type": "literal", "value": t.to_string() }),
    }
}

fn parse_bbox(s: &str) -> ApiResult<[f32; 4]> {
    let v: Vec<f32> = s
        .split(',')
        .map(|x| x.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                "bbox: four numbers, minX,minY,maxX,maxY",
            )
        })?;
    match v.as_slice() {
        [a, b, c, d]
            if a.is_finite()
                && b.is_finite()
                && c.is_finite()
                && d.is_finite()
                && a < c
                && b < d =>
        {
            Ok([*a, *b, *c, *d])
        }
        _ => Err(err(
            StatusCode::BAD_REQUEST,
            "bbox: four finite numbers, minX,minY,maxX,maxY, with each minimum below its maximum",
        )),
    }
}

/// The colour value of each point in `points`: the first object of `colour` for its
/// subject, as an index into the returned dictionary.
fn colours(p: &Projection, points: &[usize], colour: u64) -> (Vec<u32>, Vec<(oxrdf::Term, u64)>) {
    use sparkles::store::Chunk;
    let mut dict: Vec<(oxrdf::Term, u64)> = Vec::new();
    let mut index: std::collections::HashMap<u64, u32> = std::collections::HashMap::new();
    let mut out = vec![NO_CLASS; points.len()];
    for (k, &i) in points.iter().enumerate() {
        let s = p.subjects[i];
        let mut first: Option<u64> = None;
        let _ = p
            .snapshot
            .scan(sparkles::index::Perm::Spo, &[s, colour], |c| {
                first = match c {
                    Chunk::Block(b, lo, hi) if lo < hi => Some(b.key(lo)[2]),
                    Chunk::Row(key) => Some(key[2]),
                    _ => None,
                };
                Ok(first.is_none())
            });
        if let Some(o) = first {
            let j = match index.get(&o) {
                Some(j) => *j,
                None => {
                    let Some(t) = p.snapshot.term(Id(o)) else {
                        continue;
                    };
                    dict.push((t, 0));
                    let j = (dict.len() - 1) as u32;
                    index.insert(o, j);
                    j
                }
            };
            dict[j as usize].1 += 1;
            out[k] = j;
        }
    }
    (out, dict)
}

/// `GET /$/vector/{ds}/{name}/projection?method=&bbox=&limit=&colour=`
pub async fn answer(
    State(st): St,
    Path((ds_name, name)): Path<(String, String)>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ds = dataset(&st, &ds_name)?;
    let p = params(&uri);
    let method = method_param(&p)?;
    unknown_index(&ds, &name)?;
    let bbox = param(&p, "bbox").map(parse_bbox).transpose()?;
    let limit = match param(&p, "limit") {
        None => MAX_POINTS,
        Some(l) => l
            .parse::<usize>()
            .ok()
            .filter(|n| *n <= MAX_POINTS)
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("limit: 0 to {MAX_POINTS}")))?,
    };
    let colour = param(&p, "colour").map(str::to_string);
    let binary = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains(POINTS_TYPE));
    let Some(proj) = find(&st, &ds, &name, method) else {
        return Err(err(
            StatusCode::NOT_FOUND,
            format!(
                "vector index {name} has no {}projection yet: POST to /$/vector/{ds_name}/{name}/projection computes one",
                method.map(|m| format!("{} ", m.name())).unwrap_or_default()
            ),
        ));
    };
    let ds2 = ds.clone();
    blocking(move || {
        let colour_id = match &colour {
            None => None,
            Some(c) => {
                let n = oxrdf::NamedNode::new(c.as_str()).map_err(|e| {
                    sparkles::Error::invalid(format!("colour: invalid IRI {c:?}: {e}"))
                })?;
                Some(proj.snapshot.lookup_iri(n.as_str()))
            }
        };
        let (points, total) = proj.points_in(bbox, limit);
        let (colour_of, dict) = match colour_id {
            Some(Some(id)) => {
                let (o, d) = colours(&proj, &points, id.0);
                (Some(o), d)
            }
            Some(None) => (Some(vec![NO_CLASS; points.len()]), Vec::new()),
            None => (None, Vec::new()),
        };
        let mut head = summary(&proj, &ds2);
        let grid = proj.density(bbox);
        let max = grid.iter().copied().max().unwrap_or(0);
        let bytes: Vec<u8> = grid.iter().flat_map(|c| c.to_le_bytes()).collect();
        head["region"] = json!({
            "bbox": bbox.unwrap_or(proj.bounds),
            "total": total,
            "shown": points.len(),
            "sampled": points.len() < total,
        });
        head["density"] = json!({
            "size": GRID,
            "bbox": bbox.unwrap_or(proj.bounds),
            "max": max,
            "counts": base64::engine::general_purpose::STANDARD.encode(bytes),
        });
        head["classes"] = proj
            .class_ids
            .iter()
            .zip(&proj.class_counts)
            .map(|(c, n)| json!({ "iri": term_text(&proj, *c), "count": n }))
            .collect::<Vec<_>>()
            .into();
        if colour_of.is_some() {
            head["colour"] = json!({
                "predicate": colour,
                "values": dict.iter().map(|(t, n)| json!({ "value": term_json(t), "count": n })).collect::<Vec<_>>(),
            });
        }
        if binary {
            return Ok(points_binary(&proj, head, &points, colour_of.as_deref()));
        }
        let list: Vec<J> = points
            .iter()
            .enumerate()
            .map(|(k, &i)| {
                let mut o = json!({
                    "entity": term_text(&proj, proj.subjects[i]),
                    "x": proj.xy[i][0],
                    "y": proj.xy[i][1],
                });
                if proj.classes[i] != NO_CLASS {
                    o["class"] = proj.classes[i].into();
                }
                if let Some(c) = &colour_of
                    && c[k] != NO_CLASS
                {
                    o["colour"] = c[k].into();
                }
                o
            })
            .collect();
        head["points"] = list.into();
        Ok(Json(head).into_response())
    })
    .await
}

/// The binary points format: the magic and version, the header's length as a `u32`, the
/// header as JSON, the entities as UTF-8 joined by line feeds (`termsBytes` of them),
/// padding to a multiple of four bytes, then `Float32` x and y per point, then a `Uint32`
/// class index per point (`0xFFFFFFFF` for none) and, with `colour=`, a `Uint32` colour
/// index per point. Numbers are little-endian.
fn points_binary(
    p: &Projection,
    mut head: J,
    points: &[usize],
    colour: Option<&[u32]>,
) -> Response {
    let terms: Vec<String> = points
        .iter()
        .map(|&i| term_text(p, p.subjects[i]))
        .collect();
    let joined = terms.join("\n");
    head["count"] = points.len().into();
    head["termsBytes"] = joined.len().into();
    head["hasColour"] = colour.is_some().into();
    let header = serde_json::to_vec(&head).unwrap_or_default();
    let mut out = Vec::with_capacity(16 + header.len() + joined.len() + points.len() * 16);
    out.extend_from_slice(POINTS_MAGIC);
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(joined.as_bytes());
    while out.len() % 4 != 0 {
        out.push(0);
    }
    for &i in points {
        out.extend_from_slice(&p.xy[i][0].to_le_bytes());
        out.extend_from_slice(&p.xy[i][1].to_le_bytes());
    }
    for &i in points {
        out.extend_from_slice(&p.classes[i].to_le_bytes());
    }
    if let Some(c) = colour {
        for v in c {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    ([(header::CONTENT_TYPE, POINTS_TYPE)], out).into_response()
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
