//! `GET /{ds}/diff`: the net quads added and removed between two readable commits.
//!
//! `from` and `to` take the selectors of `at` (`head`, `N`, `commit:N`,
//! `time:<RFC 3339>`, `snapshot:NAME`). `to` defaults to the head and `from` to the
//! commit before `to`, so `?to=commit:42` shows what commit 42 changed. `graph=IRI` or
//! `default` limits the diff to one graph.
//!
//! The body is JSON (`application/json`, counts, and the quads with `quads=true`),
//! N-Quads lines marked `+ ` or `- ` (`text/x-sparkles-diff`), removals first, or an RDF
//! Patch (`application/rdf-patch`, or `application/rdf-patch+thrift` in binary) whose
//! `id` and `prev` headers name the two commits. Each is streamed once it is large. A
//! diff between two `commit:` selectors never changes, so it gets a weak entity tag.

use super::*;
use sparkles::history::At;
use sparkles::store::{
    ClassGroup, ClassGroups, Diff, DiffOp, DiffOptions, MAX_GROUPS, ObjectGroup,
};
use std::io::Write;

/// The diff line format.
pub(super) const DIFF_MEDIA_TYPE: &str = "text/x-sparkles-diff";

/// The most terms one diff request may name.
const MAX_TERMS: usize = 1_000;

/// FNV-1a of a graph view's rule (part of a diff's entity tag).
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

/// The body formats of a diff.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DiffFormat {
    Json,
    Lines,
    /// RDF Patch text, under the media type asked for
    Patch(&'static str),
    PatchBinary,
}

impl DiffFormat {
    /// From `format=` (json, diff, patch, patch-binary), else from `Accept`.
    pub(super) fn of(params: &Params, headers: &HeaderMap) -> ApiResult<DiffFormat> {
        match params.get("format") {
            Some("diff" | "text") => Ok(DiffFormat::Lines),
            Some("json") => Ok(DiffFormat::Json),
            Some("patch") => Ok(DiffFormat::Patch(sparkles::patch::MEDIA_TYPE)),
            Some("patch-binary") => Ok(DiffFormat::PatchBinary),
            Some(f) => Err(err(
                StatusCode::BAD_REQUEST,
                format!("unknown format '{f}': use json, diff, patch or patch-binary"),
            )),
            None => Ok(headers
                .get(header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .and_then(|a| {
                    negotiate(
                        a,
                        &[
                            "application/json",
                            DIFF_MEDIA_TYPE,
                            "text/plain",
                            sparkles::patch::MEDIA_TYPE,
                            "text/rdf-patch",
                            sparkles::patch::MEDIA_TYPE_BINARY,
                        ],
                    )
                })
                .map_or(DiffFormat::Json, |i| match i {
                    1 | 2 => DiffFormat::Lines,
                    3 => DiffFormat::Patch(sparkles::patch::MEDIA_TYPE),
                    4 => DiffFormat::Patch("text/rdf-patch"),
                    5 => DiffFormat::PatchBinary,
                    _ => DiffFormat::Json,
                })),
        }
    }

    /// The `Content-Type` of the body.
    pub(super) fn content_type(self) -> String {
        match self {
            DiffFormat::Json => "application/json".into(),
            DiffFormat::Lines => format!("{DIFF_MEDIA_TYPE}; charset=utf-8"),
            DiffFormat::Patch(t) => format!("{t}; charset=utf-8"),
            DiffFormat::PatchBinary => sparkles::patch::MEDIA_TYPE_BINARY.into(),
        }
    }

    pub(super) fn is_patch(self) -> bool {
        matches!(self, DiffFormat::Patch(_) | DiffFormat::PatchBinary)
    }
}

fn code_err(status: StatusCode, code: &str, msg: impl Into<String>) -> ApiError {
    ApiError(status, json!({ "error": msg.into(), "code": code }))
}

/// One selector parameter (`None` when absent); given twice with different values is an
/// error.
pub(super) fn selector(params: &Params, key: &str) -> ApiResult<Option<At>> {
    let vals = params.all(key);
    let Some(first) = vals.first() else {
        return Ok(None);
    };
    if vals.iter().any(|v| v != first) {
        return Err(code_err(
            StatusCode::BAD_REQUEST,
            "invalid-at",
            format!("{key} is given more than once with different values"),
        ));
    }
    first
        .parse::<At>()
        .map(Some)
        .map_err(|e| code_err(StatusCode::BAD_REQUEST, "invalid-at", format!("{key}: {e}")))
}

/// The graph a diff is limited to: `graph=IRI`, or `default` (also `graph=default`).
fn graph_param(params: &Params) -> ApiResult<Option<oxrdf::GraphName>> {
    if params.has("default") || params.get("graph") == Some("default") {
        return Ok(Some(oxrdf::GraphName::DefaultGraph));
    }
    match params.get("graph") {
        None => Ok(None),
        Some(iri) => oxrdf::NamedNode::new(iri)
            .map(|n| Some(oxrdf::GraphName::NamedNode(n)))
            .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid graph IRI: {e}"))),
    }
}

fn side(r: &sparkles::history::Resolved, restricted: bool) -> J {
    let mut commit = json!(r.commit);
    if restricted {
        super::redact_commit_json(&mut commit);
    }
    json!({
        "selector": r.at.to_string(),
        "commit": commit,
    })
}

pub(super) fn quad_json(op: DiffOp, q: &oxrdf::Quad) -> J {
    let graph = match &q.graph_name {
        oxrdf::GraphName::DefaultGraph => J::Null,
        g => json!(g.to_string()),
    };
    json!({
        "op": op.sign().to_string(),
        "subject": q.subject.to_string(),
        "predicate": q.predicate.to_string(),
        "object": q.object.to_string(),
        "graph": graph,
    })
}

/// The JSON members of a diff besides its quads. For a caller limited to some graphs,
/// without the work counts and the commits' quad counts, which cover every graph.
fn summary(name: &str, ds: &Dataset, d: &Diff, restricted: bool) -> J {
    let mut j = json!({
        "dataset": name,
        "datasetId": ds.store.owner_dataset_id(),
        "from": side(&d.from, restricted),
        "to": side(&d.to, restricted),
        "added": d.added,
        "removed": d.removed,
        "method": d.method.as_str(),
        "logChanges": d.log_changes,
        "compared": d.compared,
    });
    if restricted && let Some(m) = j.as_object_mut() {
        m.remove("logChanges");
        m.remove("compared");
    }
    j
}

pub(super) async fn diff(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    // the changes of the graphs the caller may read
    let view = p.view(&ds.name, crate::auth::Endpoint::Diff);
    let restricted = view.is_some();
    let params = Params::from_query(&uri);
    let to = selector(&params, "to")?.unwrap_or(At::Head);
    let from = selector(&params, "from")?;
    let graph = graph_param(&params)?;
    let fmt = DiffFormat::of(&params, &headers)?;
    let text = fmt == DiffFormat::Lines;
    if fmt.is_patch() && params.get("limit").is_some() {
        // a patch without some of its changes would apply to a wrong state
        return Err(err(
            StatusCode::BAD_REQUEST,
            "limit does not apply to RDF Patch: a patch lists every change",
        ));
    }
    // the changes that touch these resources as subject or object
    let term_params = params.all("term");
    if term_params.len() > MAX_TERMS {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("term: at most {MAX_TERMS} terms"),
        ));
    }
    if fmt.is_patch() && !term_params.is_empty() {
        // a patch without some of its changes would apply to a wrong state
        return Err(err(
            StatusCode::BAD_REQUEST,
            "term does not apply to RDF Patch: a patch lists every change",
        ));
    }
    let terms = term_params
        .iter()
        .map(|v| super::hist::term_param("term", v))
        .collect::<ApiResult<Vec<_>>>()?;
    let with_quads = fmt != DiffFormat::Json || params.get("quads").is_some_and(truthy);
    // the counts grouped by class instead of the quads
    let group = match params.get("group") {
        None => false,
        Some("class") => true,
        Some(g) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!("unknown group '{g}': use class"),
            ));
        }
    };
    if group && (fmt != DiffFormat::Json || with_quads) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "group=class answers JSON counts: it does not combine with quads=true, the diff lines or RDF Patch",
        ));
    }
    let limit = match params.get("limit") {
        None if group => MAX_GROUPS,
        None => usize::MAX,
        Some(v) => match v.parse::<usize>() {
            Ok(n) if group && !(1..=MAX_GROUPS).contains(&n) => {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    format!("limit: with group=class, from 1 to {MAX_GROUPS} groups"),
                ));
            }
            Ok(n) => n,
            Err(_) => {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "limit must be a non-negative integer",
                ));
            }
        },
    };
    let mut opts = query_options(&st, &ds, &params);
    if group {
        // a grouping holds memory of its own, under the budgets the request asks for
        super::budgets::Overrides::parse(&params)?.apply(&mut opts);
    }
    // a client that goes away stops the diff and the class lookups
    let (cancel, _cancel_on_drop) = cancel_on_drop();
    let rows = opts.max_rows.unwrap_or(st.limits.max_rows) as u64;
    // a grouping holds the diff it groups, so the memory budget also caps the diff's size
    // before it is built
    let memory_cap = match opts.max_memory_bytes {
        Some(m) if group => Some((m / sparkles::store::CHANGE_BYTES).max(1)),
        _ => None,
    }
    .filter(|&c| rows == 0 || c < rows);
    let o = DiffOptions {
        graph,
        max_quads: memory_cap.unwrap_or(rows),
        cancel: opts.cancel.clone().or(Some(cancel)),
        deadline: opts.timeout.map(|t| std::time::Instant::now() + t),
        graphs: view.clone(),
        terms: None,
    };
    let o = if terms.is_empty() {
        o
    } else {
        o.only_terms(&terms)
    };
    // a diff across branches: `fromBranch` and `toBranch`, each the request's branch
    // by default; without `from`, from the head of `fromBranch`
    let here = ds.branch_name().to_string();
    let (fb, tb) = (params.get("fromBranch"), params.get("toBranch"));
    let cross = fb.is_some() || tb.is_some();
    let (fb, tb) = (
        fb.map_or(here.clone(), str::to_string),
        tb.map_or(here.clone(), str::to_string),
    );
    for b in [&fb, &tb] {
        let q = crate::auth::on_branch(&name, b);
        if p.level_at(&q, crate::auth::Endpoint::Diff).is_none() {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                json!({ "error": format!("no such branch: {b}"), "code": "no-such-branch" }),
            ));
        }
    }
    let main = ds.main().unwrap_or_else(|| ds.clone());
    let (from_id, to_id) = if ds.kind == DbType::Persistent {
        (main.store.branch_id_of(&fb)?, main.store.branch_id_of(&tb)?)
    } else {
        (ds.store.dataset_id(), ds.store.dataset_id())
    };
    let (d, grouped) = blocking({
        let ds = ds.clone();
        let (fb, tb) = (fb.clone(), tb.clone());
        move || {
            let d =
                diff_of(&ds, &main, &here, cross, (&fb, from), (&tb, to), &o).map_err(
                    |e| match e {
                        sparkles::Error::BudgetExceeded(b)
                            if b.kind == sparkles::BudgetKind::Rows
                                && Some(b.limit) == memory_cap =>
                        {
                            sparkles::Error::BudgetExceeded(sparkles::Budget {
                                kind: sparkles::BudgetKind::Memory,
                                limit: opts.max_memory_bytes.unwrap_or_default(),
                                requested: b
                                    .requested
                                    .saturating_mul(sparkles::store::CHANGE_BYTES),
                            })
                        }
                        e => e,
                    },
                )?;
            if !group {
                return Ok((d, None));
            }
            let g = group_by_class(&ds, &main, &here, (&fb, &tb), &d, &o, &opts, limit)?;
            Ok((d, Some(g)))
        }
    })
    .await?;
    let id = ds.store.dataset_id();
    let fixed = matches!(d.from.at, At::Commit(_)) && matches!(d.to.at, At::Commit(_));
    // everything that shapes the body: the commits, the format, the quads listed and the
    // graph
    let tag = format!(
        "W/\"{id}:{}..{}:{}{}{}{}\"",
        d.from.commit.seq,
        d.to.commit.seq,
        match (fmt, with_quads) {
            _ if group => "json+groups",
            (DiffFormat::Lines, _) => "diff",
            (DiffFormat::Patch(_), _) => "patch",
            (DiffFormat::PatchBinary, _) => "patch-binary",
            (DiffFormat::Json, true) => "json+quads",
            (DiffFormat::Json, false) => "json",
        },
        match params.get("limit") {
            Some(_) if with_quads || group => format!(":limit={limit}"),
            _ => String::new(),
        },
        if term_params.is_empty() {
            String::new()
        } else {
            let mut ts = term_params.clone();
            ts.sort();
            format!(":terms={:016x}", fnv(&ts.join("\n")))
        },
        match (&params.get("graph"), params.has("default")) {
            (Some(g), _) => format!(
                ":graph={}",
                form_urlencoded::byte_serialize(g.as_bytes()).collect::<String>()
            ),
            (None, true) => ":graph=default".to_string(),
            _ => String::new(),
        }
    );
    // across branches, the branches are part of what the tag names
    let tag = if from_id != to_id {
        format!("{}:{from_id}..{to_id}\"", tag.trim_end_matches('"'))
    } else {
        tag
    };
    // a view's body differs from the full one, and from a view with other protections
    // of triples lifted
    let tag = match &view {
        Some(v) => format!(
            "{}:view={:016x}\"",
            tag.trim_end_matches('"'),
            fnv(&super::graph::cache::view_key(Some(v)))
        ),
        None => tag,
    };
    let summary = summary(&name, &ds, &d, restricted);
    let counts = [
        ("sparkles-diff-from", d.from.commit.seq),
        ("sparkles-diff-to", d.to.commit.seq),
        ("sparkles-diff-added", d.added),
        ("sparkles-diff-removed", d.removed),
    ];
    if fixed
        && headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(',').any(|t| t.trim() == tag || t.trim() == "*"))
    {
        let mut r = StatusCode::NOT_MODIFIED.into_response();
        if let Ok(v) = header::HeaderValue::from_str(&tag) {
            r.headers_mut().insert(header::ETAG, v);
        }
        return Ok(r);
    }
    let d = Arc::new(d);
    let patch_ids = (
        sparkles::patch::commit_iri(to_id, d.to.commit.seq),
        sparkles::patch::commit_iri(from_id, d.from.commit.seq),
    );
    let write = move |w: &mut LimitedWriter<stream::SwitchWriter>| -> sparkles::Result<()> {
        if fmt.is_patch() {
            let mut pw = sparkles::patch::PatchWriter::new(w, fmt == DiffFormat::PatchBinary);
            pw.header("id", &patch_ids.0)?;
            pw.header("prev", &patch_ids.1)?;
            pw.begin()?;
            for (op, q) in d.iter() {
                pw.change(op, &q)?;
            }
            pw.commit()?;
            return Ok(());
        }
        if text {
            for (op, q) in d.slice(0, limit) {
                writeln!(
                    w,
                    "{} {}",
                    op.sign(),
                    sparkles::annotations::nquads_line(&q)
                )?;
            }
            return Ok(());
        }
        // the summary, then the quads or the groups one by one
        let mut head = serde_json::to_vec(&summary).map_err(std::io::Error::other)?;
        if let Some((g, _held)) = &grouped {
            head.pop(); // the closing brace
            w.write_all(&head)?;
            write!(
                w,
                ",\"group\":\"class\",\"groupCount\":{},\"truncated\":{},\"groups\":[",
                g.total, g.truncated
            )?;
            for (i, x) in g.groups.iter().enumerate() {
                if i > 0 {
                    w.write_all(b",")?;
                }
                serde_json::to_writer(&mut *w, &group_json(x)).map_err(std::io::Error::other)?;
            }
            w.write_all(b"]}")?;
            return Ok(());
        }
        if !with_quads {
            w.write_all(&head)?;
            return Ok(());
        }
        head.pop(); // the closing brace
        w.write_all(&head)?;
        w.write_all(b",\"quads\":[")?;
        for (i, (op, q)) in d.slice(0, limit).enumerate() {
            if i > 0 {
                w.write_all(b",")?;
            }
            serde_json::to_writer(&mut *w, &quad_json(op, &q)).map_err(std::io::Error::other)?;
        }
        w.write_all(b"]}")?;
        Ok(())
    };
    let ds_name = ds.name.clone();
    let st_weak = Arc::downgrade(&st);
    let body = stream::serialize(st.limits.max_export_bytes, write, move |end| {
        if let Some(st) = st_weak.upgrade() {
            st.metrics
                .add_response_bytes(Some(&ds_name), Op::Other, end.bytes);
        }
        stream_end_log("diff", &end);
    })
    .await?;
    let body = match body {
        stream::Serialized::Whole { body, .. } => axum::body::Body::from(body),
        stream::Serialized::Streamed(b) => b,
    };
    let mut resp = ([(header::CONTENT_TYPE, fmt.content_type())], body).into_response();
    let h = resp.headers_mut();
    for (k, v) in counts {
        h.insert(k, v.into());
    }
    if let Ok(v) = header::HeaderValue::from_str(&id.to_string()) {
        h.insert(SPARKLES_DATASET_ID, v);
    }
    if params.get("format").is_none() {
        h.append(header::VARY, header::HeaderValue::from_static("accept"));
    }
    if fixed && let Ok(v) = header::HeaderValue::from_str(&tag) {
        h.insert(header::ETAG, v);
    }
    Ok(resp)
}

/// The diff the request names: across branches, or on the request's branch, `from`
/// defaulting to the commit before `to`.
fn diff_of(
    ds: &Dataset,
    main: &Dataset,
    here: &str,
    cross: bool,
    (fb, from): (&str, Option<At>),
    (tb, to): (&str, At),
    o: &DiffOptions,
) -> sparkles::Result<Diff> {
    if cross {
        let from = from.unwrap_or(At::Head);
        return Ok(main.store.branch_diff(fb, &from, tb, &to, o)?);
    }
    // without `from`: the commit before `to`
    let from = match from {
        Some(f) => f,
        None => {
            let r = match ds.store.resolve(&to) {
                Err(e) if sparkles::branch::inherited_commit(&e).is_some() => {
                    main.store.branch_resolve(here, &to)?.0
                }
                r => r?,
            };
            At::Commit(r.commit.seq.saturating_sub(1))
        }
    };
    match ds.dataset.history().diff(&from, &to, o) {
        // a commit the branch shares with its upstream
        Err(e) if sparkles::branch::inherited_commit(&e).is_some() => {
            main.store.branch_diff(here, &from, here, &to, o)
        }
        r => r,
    }
}

/// `group=class`: the diff's counts by the classes of its resources, which are read at
/// each side's state under the caller's view (`o.graphs`). What the grouping holds, and
/// the diff it groups, are charged to the request's memory budget and to its grant in the
/// server memory pool, so a grouping past them answers `507`. The charge is returned with
/// the groups and held until the response is written.
#[allow(clippy::too_many_arguments)]
fn group_by_class(
    ds: &Dataset,
    main: &Dataset,
    here: &str,
    (fb, tb): (&str, &str),
    d: &Diff,
    o: &DiffOptions,
    opts: &sparkles::sparql::QueryOptions,
    limit: usize,
) -> ApiResult<(ClassGroups, super::sparql_diff::Charge)> {
    let mut charge = super::sparql_diff::Charge::new(opts);
    charge.charge((d.len() as u64).saturating_mul(sparkles::store::CHANGE_BYTES))?;
    let ho = sparkles::history::HistoryOptions {
        cancel: o.cancel.clone(),
        deadline: o.deadline,
    };
    let mut state = |op: DiffOp| -> sparkles::Result<Arc<sparkles::store::Snapshot>> {
        let (branch, seq) = match op {
            DiffOp::Add => (tb, d.to.commit.seq),
            DiffOp::Remove => (fb, d.from.commit.seq),
        };
        let at = At::Commit(seq);
        if branch == here {
            let q = sparkles::sparql::QueryOptions {
                cancel: o.cancel.clone(),
                timeout: o
                    .deadline
                    .map(|t| t.saturating_duration_since(std::time::Instant::now())),
                ..Default::default()
            };
            Ok(super::history::snapshot_for(ds, Some(&at), &q)?.0)
        } else {
            Ok(main.store.branch_snapshot_at(branch, &at, &ho)?.0)
        }
    };
    let g = d.group_by_class(&mut state, o, limit, &mut |n| charge.charge(n))?;
    // the charge stays with the groups until the body that lists them is dropped
    Ok((g, charge))
}

/// A group of `group=class` as JSON: IRIs without angle brackets.
fn group_json(g: &ClassGroup) -> J {
    let object = match &g.object {
        ObjectGroup::Class(c) => json!({ "class": c }),
        ObjectGroup::Literal(dt) => json!({ "literal": dt }),
        ObjectGroup::Iri => json!({ "iri": true }),
    };
    json!({
        "subjectClass": g.subject_class,
        "predicate": g.predicate,
        "object": object,
        "added": g.added,
        "removed": g.removed,
    })
}
