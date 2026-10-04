//! Branches and merges over HTTP ([F09](../../../../docs/specs/F09-branches-and-merges.md)):
//! the branch a request works on (`?branch=NAME` or the path form `/{ds}@{branch}/…`),
//! the branch routes `/$/branches/{ds}[/{name}]`, and merges `/$/merge/{ds}`.

use super::*;
use crate::auth::{Endpoint, on_branch};
use sparkles::branch::{
    BranchInfo, BranchOptions, ConflictScope, MAIN, MergeOptions, MergeOutcome, MergeReport,
    NamedCommitRef, Resolution, Take,
};
use sparkles::history::At;

/// Merge counters by dataset label: results, conflicts, changes and time.
#[derive(Default)]
struct MergeCounts {
    results: std::collections::BTreeMap<&'static str, u64>,
    conflicts: u64,
    inserted: u64,
    deleted: u64,
    seconds: f64,
    count: u64,
}

static MERGES: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::BTreeMap<String, MergeCounts>>,
> = std::sync::LazyLock::new(Default::default);

fn count_merge(st: &AppState, ds: &str, result: &'static str, r: Option<&MergeReport>, secs: f64) {
    let mut m = MERGES.lock();
    let c = m.entry(st.metrics.dataset_label(Some(ds))).or_default();
    *c.results.entry(result).or_default() += 1;
    if let Some(r) = r {
        c.conflicts += r.conflicts_found;
        if r.merged {
            c.inserted += r.inserted;
            c.deleted += r.deleted;
        }
    }
    c.seconds += secs;
    c.count += 1;
}

/// Branch and merge metrics in the Prometheus text format.
pub(crate) fn metrics(st: &AppState, out: &mut String) {
    use std::fmt::Write;
    let label = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let family = |o: &mut String, name: &str, kind: &str, help: &str| {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} {kind}");
    };
    let datasets: Vec<Arc<Dataset>> = st.datasets.read().values().cloned().collect();
    let mut gauges: std::collections::BTreeMap<String, (u64, u64)> = Default::default();
    for d in datasets.iter().filter(|d| d.kind == DbType::Persistent) {
        let linked = d.store.branch_set().map_or(0, |s| {
            s.open_stores()
                .iter()
                .filter(|b| b.snapshot().generation.linked().is_some())
                .count() as u64
        });
        let g = gauges
            .entry(st.metrics.dataset_label(Some(&d.name)))
            .or_default();
        g.0 += d.store.branch_count() as u64;
        g.1 += linked;
    }
    if !gauges.is_empty() {
        family(
            out,
            "sparkles_branches",
            "gauge",
            "Branches per dataset, main included.",
        );
        for (ds, (n, _)) in &gauges {
            let _ = writeln!(out, "sparkles_branches{{dataset=\"{}\"}} {n}", label(ds));
        }
        family(
            out,
            "sparkles_branch_linked",
            "gauge",
            "Open branches that still read their upstream's index files.",
        );
        for (ds, (_, n)) in &gauges {
            let _ = writeln!(
                out,
                "sparkles_branch_linked{{dataset=\"{}\"}} {n}",
                label(ds)
            );
        }
    }
    let m = MERGES.lock();
    if m.is_empty() {
        return;
    }
    family(
        out,
        "sparkles_merges_total",
        "counter",
        "Merges by result: merged, fast-forward, up-to-date, conflict or refused.",
    );
    for (ds, c) in m.iter() {
        for (r, n) in &c.results {
            let _ = writeln!(
                out,
                "sparkles_merges_total{{dataset=\"{}\",result=\"{r}\"}} {n}",
                label(ds)
            );
        }
    }
    family(
        out,
        "sparkles_merge_conflicts_total",
        "counter",
        "Conflicting groups that merges found.",
    );
    for (ds, c) in m.iter() {
        let _ = writeln!(
            out,
            "sparkles_merge_conflicts_total{{dataset=\"{}\"}} {}",
            label(ds),
            c.conflicts
        );
    }
    family(
        out,
        "sparkles_merge_changes_total",
        "counter",
        "Quads that merges inserted and deleted.",
    );
    for (ds, c) in m.iter() {
        let _ = writeln!(
            out,
            "sparkles_merge_changes_total{{dataset=\"{}\",op=\"insert\"}} {}",
            label(ds),
            c.inserted
        );
        let _ = writeln!(
            out,
            "sparkles_merge_changes_total{{dataset=\"{}\",op=\"delete\"}} {}",
            label(ds),
            c.deleted
        );
    }
    family(
        out,
        "sparkles_merge_seconds",
        "summary",
        "Time spent in merges.",
    );
    for (ds, c) in m.iter() {
        let _ = writeln!(
            out,
            "sparkles_merge_seconds_sum{{dataset=\"{}\"}} {}",
            label(ds),
            c.seconds
        );
        let _ = writeln!(
            out,
            "sparkles_merge_seconds_count{{dataset=\"{}\"}} {}",
            label(ds),
            c.count
        );
    }
}

/// Response header: the branch a response comes from (absent for `main`).
pub(crate) const SPARKLES_BRANCH: &str = "sparkles-branch";
/// Response header: that branch's id.
pub(crate) const SPARKLES_BRANCH_ID: &str = "sparkles-branch-id";

/// The branch a request chose (request extension, set before routing). The grant check
/// reads it, so a build without the `auth` feature never does.
#[derive(Clone, Debug)]
#[cfg_attr(not(feature = "auth"), allow(dead_code))]
pub(crate) struct SelectedBranch(pub String);

tokio::task_local! {
    /// The branch the request being handled chose (`None`: `main`).
    static BRANCH: Option<String>;
}

/// The branch of the request being handled (`None`: `main`).
pub(crate) fn current() -> Option<String> {
    BRANCH.try_with(|b| b.clone()).ok().flatten()
}

/// Admin routes that take `branch` (the others refuse one other than `main`).
const ADMIN_BRANCH_ROUTES: &[&str] = &[
    "commits",
    "snapshots",
    "history",
    "compaction",
    "compact",
    "stats",
    "schema",
    "reason",
    "validation",
    "prefixes",
    "describe",
    "text",
    "geo",
    "vector",
    "rdfs",
    "cache",
];

fn bad_branch(msg: impl Into<String>) -> Response {
    let body = json!({ "error": msg.into(), "code": "invalid-branch" });
    let mut r = (StatusCode::BAD_REQUEST, Json(body.clone())).into_response();
    r.extensions_mut().insert(ErrorJson(body));
    r
}

/// Before routing: take the branch from the path form `/{ds}@{branch}/…` (rewriting the
/// path to `/{ds}/…`), from `branch=` in the query string, or from `branch=` in a form
/// body, check that they agree, and run the request with that branch. Responses from a
/// branch other than `main` name it in `Sparkles-Branch` and `Sparkles-Branch-Id`.
pub(crate) async fn select(
    State(st): St,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_string();
    let mut chosen: Vec<String> = Vec::new();
    let admin = path.starts_with("/$/");
    // the path form
    if !admin && !path.starts_with("/ui") {
        let rest = &path[1..];
        let (first, tail) = rest.split_once('/').map_or((rest, ""), |(a, b)| (a, b));
        let decoded = percent_encoding::percent_decode_str(first).decode_utf8_lossy();
        if let Some((ds, b)) = decoded.split_once('@') {
            if ds.is_empty() || b.is_empty() {
                return bad_branch(format!("invalid dataset and branch '{decoded}'"));
            }
            chosen.push(b.to_string());
            let new_path = if tail.is_empty() && !rest.contains('/') {
                format!("/{ds}")
            } else {
                format!("/{ds}/{tail}")
            };
            let pq = match req.uri().query() {
                Some(q) => format!("{new_path}?{q}"),
                None => new_path,
            };
            let mut parts = req.uri().clone().into_parts();
            match pq.parse() {
                Ok(p) => parts.path_and_query = Some(p),
                Err(_) => return bad_branch("invalid path"),
            }
            match Uri::from_parts(parts) {
                Ok(u) => *req.uri_mut() = u,
                Err(_) => return bad_branch("invalid path"),
            }
        }
    }
    // the parameter, in the query string
    // (parsed only when it names a branch, so other requests pay for a substring check)
    if req.uri().query().is_some_and(|q| q.contains("branch=")) {
        chosen.extend(Params::from_query(req.uri()).all("branch"));
    }
    // and in a form body (not compressed: the decompression layer runs later)
    let form = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("application/x-www-form-urlencoded"));
    if form && req.method() == Method::POST && !req.headers().contains_key(header::CONTENT_ENCODING)
    {
        let limit = st
            .limits
            .max_admin_body_bytes
            .map_or(64 << 20, |b| b as usize)
            .max(64 << 20);
        let (parts, body) = req.into_parts();
        let bytes = match axum::body::to_bytes(body, limit).await {
            Ok(b) => b,
            Err(e) => return err(StatusCode::PAYLOAD_TOO_LARGE, e.to_string()).into_response(),
        };
        if bytes.windows(7).any(|w| w == b"branch=") {
            let mut p = Params::default();
            p.extend_form(&bytes);
            chosen.extend(p.all("branch"));
        }
        req = axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes));
    }
    chosen.dedup();
    let mut distinct = chosen.clone();
    distinct.sort();
    distinct.dedup();
    if distinct.len() > 1 {
        return bad_branch(format!(
            "the request names more than one branch: {}",
            distinct.join(", ")
        ));
    }
    let Some(branch) = distinct.pop() else {
        return next.run(req).await;
    };
    if branch != MAIN && !sparkles::branch::valid_name(&branch) {
        return bad_branch(format!("invalid branch name '{branch}'"));
    }
    if admin {
        let seg = path.split('/').nth(2).unwrap_or("");
        // `/$/datasets/{ds}/clone` copies a branch too
        let clone = seg == "datasets" && path.ends_with("/clone");
        if branch != MAIN && !clone && !ADMIN_BRANCH_ROUTES.contains(&seg) {
            return bad_branch(format!("{path} does not take a branch"));
        }
        if seg == "branches" || seg == "merge" {
            return next.run(req).await;
        }
    }
    req.extensions_mut().insert(SelectedBranch(branch.clone()));
    let ds_name = req
        .uri()
        .path()
        .split('/')
        .nth(if admin { 3 } else { 1 })
        .map(|s| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        });
    let mut resp = BRANCH.scope(Some(branch.clone()), next.run(req)).await;
    if branch != MAIN
        && let Some(ds) = ds_name.and_then(|n| st.get(&n))
        && let Ok(id) = ds.store.branch_id_of(&branch)
    {
        let h = resp.headers_mut();
        if let Ok(v) = header::HeaderValue::from_str(&branch) {
            h.insert(SPARKLES_BRANCH, v);
        }
        if let Ok(v) = header::HeaderValue::from_str(&id.to_string()) {
            h.insert(SPARKLES_BRANCH_ID, v);
        }
    }
    resp
}

/// The JSON body of a branch error.
pub(crate) fn error_body(b: &sparkles::branch::BranchError) -> J {
    if let Some(c) = &b.conflicts {
        return serde_json::to_value(c.as_ref()).unwrap_or_else(|_| json!({}));
    }
    let mut body = json!({ "error": b.message, "code": b.code });
    if !b.candidates.is_empty() {
        body["candidates"] = b
            .candidates
            .iter()
            .map(|c| json!({ "branch": c.branch, "branchId": c.branch_id, "seq": c.seq }))
            .collect();
    }
    if let Some(c) = &b.inherited {
        body["commit"] = json!({ "branchId": c.branch_id, "seq": c.seq });
    }
    body
}

fn not_found(name: &str) -> ApiError {
    ApiError(
        StatusCode::NOT_FOUND,
        json!({ "error": format!("no such branch: {name}"), "code": "no-such-branch" }),
    )
}

fn forbidden(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, json!({ "error": msg.into() }))
}

fn invalid(code: &str, msg: impl Into<String>) -> ApiError {
    ApiError(
        StatusCode::BAD_REQUEST,
        json!({ "error": msg.into(), "code": code }),
    )
}

/// The dataset's own object, for the branch routes (which name branches themselves).
fn main_dataset(st: &AppState, name: &str) -> ApiResult<Arc<Dataset>> {
    let ds = st
        .get(name)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("no such dataset: /{name}")))?;
    if ds.kind != DbType::Persistent {
        return Err(ApiError(
            StatusCode::NOT_IMPLEMENTED,
            json!({
                "error": "branches need a persistent dataset",
                "code": "branches-unsupported",
            }),
        ));
    }
    Ok(ds)
}

/// A branch the caller may see at `lvl` (through endpoint `e`, when given), or the
/// answer for one that does not exist.
fn check(p: &Principal, ds: &str, name: &str, lvl: Level, e: Option<Endpoint>) -> ApiResult<()> {
    let q = on_branch(ds, name);
    let have = match e {
        Some(e) => p.level_at(&q, e),
        None => p.level(&q),
    };
    if p.level(&q).is_none() && name != MAIN {
        return Err(not_found(name));
    }
    if have.is_none_or(|h| h < lvl) {
        return Err(forbidden(format!(
            "{} access to branch {name} of /{ds} required",
            lvl.as_str()
        )));
    }
    Ok(())
}

/// Branch creation and merges act on whole datasets: grants limited to some graphs do
/// not allow them.
fn whole(p: &Principal, ds: &str, name: &str) -> ApiResult<()> {
    if p.restricted(&on_branch(ds, name)) {
        return Err(forbidden(format!(
            "your access to branch {name} of /{ds} is limited to some graphs or triples; branches and merges act on whole datasets"
        )));
    }
    Ok(())
}

fn commit_ref(c: &NamedCommitRef) -> J {
    json!({ "branch": c.branch, "branchId": c.branch_id, "seq": c.seq })
}

/// A branch as JSON (the `Branch` type of the API).
pub(crate) fn branch_json(b: &BranchInfo) -> J {
    json!({
        "name": b.name,
        "id": b.id,
        "ordinal": b.ordinal,
        "head": b.head.map(|h| h.seq),
        "modified": b.head.map(|h| h.timestamp()),
        "from": b.from.as_ref().map(commit_ref),
        "upstream": b.upstream,
        "mergeBase": b.merge_base.as_ref().map(|m| json!({ "branch": m.branch, "seq": m.seq })),
        "ahead": b.ahead,
        "behind": b.behind,
        "protected": b.protected,
        "note": b.note,
        "created": sparkles::commit::rfc3339_ms(b.created_ms),
        "storage": {
            "linked": b.storage.linked,
            "ownBytes": b.storage.own_bytes,
            "heldBytes": b.storage.held_bytes,
            "generation": b.storage.generation,
        },
        "broken": b.broken,
    })
}

/// `GET /$/branches/{ds}`: the branches the caller may see, `main` first.
pub(crate) async fn list(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
) -> ApiResult {
    let ds = main_dataset(&st, &name)?;
    let n = name.clone();
    let v = blocking(move || Ok(ds.store.branches()?)).await?;
    let branches: Vec<J> = v
        .iter()
        .filter(|b| p.level(&on_branch(&n, &b.name)).is_some())
        .map(branch_json)
        .collect();
    let ds = main_dataset(&st, &name)?;
    Ok(Json(json!({
        "dataset": name,
        "datasetId": ds.store.dataset_id(),
        "branches": branches,
    }))
    .into_response())
}

/// `GET /$/branches/{ds}/{name}`.
pub(crate) async fn get_branch(
    State(st): St,
    Path((name, branch)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
) -> ApiResult {
    let ds = main_dataset(&st, &name)?;
    check(&p, &name, &branch, Level::Read, None)?;
    let info = blocking(move || Ok(ds.store.branch_info(&branch)?)).await?;
    Ok(Json(branch_json(&info)).into_response())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBody {
    name: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    protected: bool,
    #[serde(default)]
    note: Option<String>,
}

/// `POST /$/branches/{ds}`: create a branch.
pub(crate) async fn create(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    body: Bytes,
) -> ApiResult {
    let ds = main_dataset(&st, &name)?;
    let b: CreateBody = serde_json::from_slice(&body)
        .map_err(|e| invalid("invalid-branch", format!("invalid request body: {e}")))?;
    let from = b.from.clone().unwrap_or_else(|| MAIN.to_string());
    check(&p, &name, &from, Level::Read, None)?;
    // the new name: write on it, through the branches endpoint
    let q = on_branch(&name, &b.name);
    if p.level_at(&q, Endpoint::Branches)
        .is_none_or(|l| l < Level::Write)
    {
        return Err(forbidden(format!(
            "write access to branch {} of /{name} required",
            b.name
        )));
    }
    whole(&p, &name, &from)?;
    whole(&p, &name, &b.name)?;
    let at = match &b.at {
        Some(a) => a.parse::<At>()?,
        None => At::Head,
    };
    let o = BranchOptions {
        from,
        at,
        protected: b.protected,
        note: b.note,
    };
    let new = b.name.clone();
    let info = blocking(move || Ok(ds.store.create_branch(&new, &o)?)).await?;
    let loc = format!("/$/branches/{name}/{}", info.name);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, loc)],
        Json(branch_json(&info)),
    )
        .into_response())
}

/// `PATCH /$/branches/{ds}/{name}`: change `protected` or `note` (`null` removes it).
pub(crate) async fn patch_branch(
    State(st): St,
    Path((name, branch)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    body: Bytes,
) -> ApiResult {
    let ds = main_dataset(&st, &name)?;
    let v: J = serde_json::from_slice(&body)
        .map_err(|e| invalid("invalid-branch", format!("invalid request body: {e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| invalid("invalid-branch", "the body is a JSON object"))?;
    if let Some(k) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "protected" | "note"))
    {
        return Err(invalid(
            "invalid-branch",
            format!("unknown field {k}: protected and note can change"),
        ));
    }
    let protected = match obj.get("protected") {
        None => None,
        Some(J::Bool(b)) => Some(*b),
        Some(_) => return Err(invalid("invalid-branch", "protected: true or false")),
    };
    let note = match obj.get("note") {
        None => None,
        Some(J::Null) => Some(None),
        Some(J::String(s)) => Some(Some(s.clone())),
        Some(_) => return Err(invalid("invalid-branch", "note: a string or null")),
    };
    if protected.is_some() {
        check(&p, &name, &branch, Level::Admin, None)?;
    } else {
        check(&p, &name, &branch, Level::Write, Some(Endpoint::Branches))?;
    }
    let info = blocking(move || {
        if let Some(on) = protected {
            ds.store.set_branch_protected(&branch, on)?;
        }
        if let Some(note) = note {
            ds.store.set_branch_note(&branch, note)?;
        }
        Ok(ds.store.branch_info(&branch)?)
    })
    .await?;
    Ok(Json(branch_json(&info)).into_response())
}

/// `DELETE /$/branches/{ds}/{name}`: delete a branch (`?force=true` with unmerged
/// commits).
pub(crate) async fn delete_branch(
    State(st): St,
    Path((name, branch)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    let ds = main_dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let force = matches!(params.get("force"), Some("true" | "1" | ""));
    check(&p, &name, &branch, Level::Write, Some(Endpoint::Branches))?;
    if branch != MAIN {
        let protected = ds
            .store
            .branch_info(&branch)
            .map(|i| i.protected)
            .unwrap_or(false);
        if protected {
            check(&p, &name, &branch, Level::Admin, None)?;
        }
    }
    let d = ds.clone();
    let b = branch.clone();
    blocking(move || Ok(d.store.delete_branch(&b, force)?)).await?;
    ds.branches.lock().remove(&branch);
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// A term of a resolution, in N-Triples (Turtle literals such as `33` too).
fn parse_term(s: &str, what: &str) -> ApiResult<oxrdf::Term> {
    let doc = format!("<urn:x-s> <urn:x-p> {s} .");
    let bad = || invalid("invalid-merge", format!("{what}: not a term: {s}"));
    let q = oxrdfio::RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(doc.as_bytes())
        .next()
        .ok_or_else(bad)?
        .map_err(|_| bad())?;
    Ok(q.object)
}

fn parse_resolution(v: &J) -> ApiResult<Resolution> {
    let graph = match v.get("graph") {
        None | Some(J::Null) => oxrdf::GraphName::DefaultGraph,
        Some(J::String(s)) => match parse_term(s, "graph")? {
            oxrdf::Term::NamedNode(n) => oxrdf::GraphName::NamedNode(n),
            oxrdf::Term::BlankNode(b) => oxrdf::GraphName::BlankNode(b),
            _ => return Err(invalid("invalid-merge", "graph: an IRI or blank node")),
        },
        Some(_) => return Err(invalid("invalid-merge", "graph: a string or null")),
    };
    let subject = match v.get("subject").and_then(J::as_str) {
        Some(s) => Some(parse_term(s, "subject")?),
        None => None,
    };
    let predicate = match v.get("predicate").and_then(J::as_str) {
        Some(s) => match parse_term(s, "predicate")? {
            oxrdf::Term::NamedNode(n) => Some(n),
            _ => return Err(invalid("invalid-merge", "predicate: an IRI")),
        },
        None => None,
    };
    let take = match v.get("take").and_then(J::as_str) {
        Some("objects") => {
            let objs = v
                .get("objects")
                .and_then(J::as_array)
                .ok_or_else(|| invalid("invalid-merge", "take objects needs objects"))?;
            let mut out = Vec::new();
            for o in objs {
                let s = o
                    .as_str()
                    .ok_or_else(|| invalid("invalid-merge", "objects: strings"))?;
                out.push(parse_term(s, "objects")?);
            }
            Take::Objects(out)
        }
        Some(t) => Take::parse(t).ok_or_else(|| {
            invalid(
                "invalid-merge",
                format!("take: ours, theirs, base, union or objects, not {t}"),
            )
        })?,
        None => return Err(invalid("invalid-merge", "a resolution needs take")),
    };
    Ok(Resolution {
        graph,
        subject,
        predicate,
        take,
    })
}

/// What a merge request asks for: source, target and options.
struct MergeAsk {
    source: String,
    target: String,
    o: MergeOptions,
    dry_run: bool,
}

fn merge_options(st: &AppState, v: &J, preview: bool) -> ApiResult<MergeAsk> {
    let s = |k: &str| v.get(k).and_then(J::as_str).map(str::to_string);
    let source = s("source").ok_or_else(|| invalid("invalid-merge", "source is required"))?;
    let target = s("target").unwrap_or_else(|| MAIN.to_string());
    let mut o = MergeOptions {
        max_quads: st.limits.max_rows as u64,
        deadline: Some(std::time::Instant::now() + st.default_timeout.max(Duration::from_secs(60))),
        ..Default::default()
    };
    match s("ff").as_deref() {
        None | Some("auto") => {}
        Some("only") => o.ff_only = true,
        Some(x) => {
            return Err(invalid(
                "invalid-merge",
                format!("ff: auto or only, not {x}"),
            ));
        }
    }
    match v.get("squash") {
        None | Some(J::Null) => {}
        Some(J::Bool(b)) => o.squash = *b,
        Some(_) => return Err(invalid("invalid-merge", "squash: true or false")),
    }
    if let Some(c) = s("conflicts") {
        o.scope = ConflictScope::parse(&c).ok_or_else(|| {
            invalid(
                "invalid-merge",
                format!("conflicts: cell, subject or quad, not {c}"),
            )
        })?;
    }
    match s("onConflict").as_deref() {
        None | Some("fail") => {}
        Some(t) => {
            o.on_conflict = Some(Take::parse(t).filter(|t| *t != Take::Base).ok_or_else(|| {
                invalid(
                    "invalid-merge",
                    format!("onConflict: fail, ours, theirs or union, not {t}"),
                )
            })?)
        }
    }
    if let Some(rs) = v.get("resolutions") {
        let rs = rs
            .as_array()
            .ok_or_else(|| invalid("invalid-merge", "resolutions: an array"))?;
        for r in rs {
            o.resolutions.push(parse_resolution(r)?);
        }
    }
    if let Some(e) = v.get("expect") {
        o.expect_source = e.get("source").and_then(J::as_u64);
        o.expect_target = e.get("target").and_then(J::as_u64);
    }
    match s("inferences").as_deref() {
        None | Some("exclude") => {}
        Some("include") => o.include_inferences = true,
        Some(x) => {
            return Err(invalid(
                "invalid-merge",
                format!("inferences: exclude or include, not {x}"),
            ));
        }
    }
    if let Some(l) = v.get("limit") {
        o.limit =
            l.as_u64()
                .filter(|n| (1..=10_000).contains(n))
                .ok_or_else(|| invalid("invalid-merge", "limit: 1 to 10000"))? as usize;
    }
    if let Some(m) = s("message") {
        o.write.message = Some(m.into());
    }
    let dry_run = v.get("dryRun").and_then(J::as_bool).unwrap_or(false) && !preview;
    Ok(MergeAsk {
        source,
        target,
        o,
        dry_run,
    })
}

fn merge_json(r: &MergeReport, stale: Option<bool>) -> J {
    let side = |c: &NamedCommitRef| json!({ "branch": c.branch, "seq": c.seq });
    let commit = r.commit.as_ref().filter(|rc| rc.committed).map(|rc| {
        let mut j = json!(sparkles::commit::AnnotatedCommit {
            commit: &rc.commit,
            annotation: Some(&rc.annotation),
        });
        j["branch"] = json!(r.target.branch);
        j["branchId"] = json!(r.target.branch_id);
        if !r.squashed {
            j["mergedFrom"] = commit_ref(&r.source);
        }
        j
    });
    let mut j = json!({
        "merged": r.merged,
        "upToDate": r.up_to_date,
        "fastForward": r.fast_forward,
        "squashed": r.squashed,
        "source": side(&r.source),
        "target": side(&r.target),
        "base": r.base.as_ref().map(side),
        "changes": { "inserted": r.inserted, "deleted": r.deleted },
        "conflicts": { "found": r.conflicts_found, "resolved": r.conflicts_resolved },
        "commit": commit,
        "inferences": r.inferences_excluded.map(|n| json!({
            "excluded": n,
            "stale": stale.unwrap_or(false),
        })),
    });
    if let Some(v) = r.commit.as_ref().and_then(|rc| rc.validation.as_ref()) {
        j["validation"] = json!(v.as_ref());
    }
    if let Some(c) = &r.conflicts
        && let (Some(obj), Ok(J::Object(cj))) = (j.as_object_mut(), serde_json::to_value(c))
    {
        for (k, v) in cj {
            if k != "conflicts" {
                obj.insert(k, v);
            }
        }
        obj.insert("conflictCount".into(), json!(c.conflicts));
    }
    j
}

fn merge_access(p: &Principal, ds: &str, ask: &MergeAsk) -> ApiResult<()> {
    check(p, ds, &ask.source, Level::Read, Some(Endpoint::Merge))?;
    check(p, ds, &ask.target, Level::Write, Some(Endpoint::Merge))?;
    whole(p, ds, &ask.source)?;
    whole(p, ds, &ask.target)
}

/// `GET /$/merge/{ds}?source=&target=`: what a merge would do, without writing.
pub(crate) async fn preview(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    let ds = main_dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let mut v = serde_json::Map::new();
    for k in [
        "source",
        "target",
        "ff",
        "conflicts",
        "onConflict",
        "inferences",
    ] {
        if let Some(x) = params.get(k) {
            v.insert(k.into(), x.into());
        }
    }
    if let Some(x) = params.get("squash") {
        v.insert("squash".into(), matches!(x, "true" | "1" | "").into());
    }
    if let Some(l) = params.get("limit") {
        v.insert(
            "limit".into(),
            l.parse::<u64>()
                .map_err(|_| invalid("invalid-merge", "limit: 1 to 10000"))?
                .into(),
        );
    }
    let ask = merge_options(&st, &J::Object(v), true)?;
    merge_access(&p, &name, &ask)?;
    let r = blocking(move || Ok(ds.store.preview_merge(&ask.source, &ask.target, &ask.o)?)).await?;
    Ok(Json(merge_json(&r, None)).into_response())
}

/// `POST /$/merge/{ds}`: merge, or (`dryRun`) preview the merge commit with the
/// target's write-time validation and quota.
pub(crate) async fn merge(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let ds = main_dataset(&st, &name)?;
    let v: J = serde_json::from_slice(&body)
        .map_err(|e| invalid("invalid-merge", format!("invalid request body: {e}")))?;
    let mut ask = merge_options(&st, &v, false)?;
    merge_access(&p, &name, &ask)?;
    if let Some(a) = p.log_name() {
        ask.o.write.author = Some(a.into());
    }
    let dr = ask.dry_run.then(|| sparkles::preview::DryRun {
        changes: v
            .get("changes")
            .and_then(J::as_u64)
            .unwrap_or(0)
            .min(sparkles::preview::MAX_LISTED_CHANGES as u64) as usize,
        all_changes: false,
        max_changes: st.limits.max_rows as u64,
    });
    ask.o.write.dry_run = dr.clone();
    let (source, target) = (ask.source.clone(), ask.target.clone());
    let tds = st.branch_dataset(&ds, &target)?;
    let reasoned = tds.reasoning.read().is_some();
    let d = ds.clone();
    let o = ask.o.clone();
    let t0 = std::time::Instant::now();
    let out = blocking(move || {
        Ok(match d.store.merge(&ask.source, &ask.target, &ask.o) {
            Err(Error::DryRun(p)) => Err(*p),
            Ok(o) => Ok(o),
            Err(e) => return Err(e.into()),
        })
    })
    .await;
    let secs = t0.elapsed().as_secs_f64();
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            count_merge(&st, &name, "refused", None, secs);
            return Err(e);
        }
    };
    match &out {
        Ok(MergeOutcome::Conflicts(c)) => {
            let mut m = MERGES.lock();
            let e = m.entry(st.metrics.dataset_label(Some(&name))).or_default();
            e.conflicts += c.conflicts;
            *e.results.entry("conflict").or_default() += 1;
            e.seconds += secs;
            e.count += 1;
        }
        Ok(MergeOutcome::UpToDate(r)) => count_merge(&st, &name, "up-to-date", Some(r), secs),
        Ok(MergeOutcome::Merged(r)) => count_merge(
            &st,
            &name,
            if r.fast_forward {
                "fast-forward"
            } else {
                "merged"
            },
            Some(r),
            secs,
        ),
        Err(_) => {}
    }
    match out {
        Err(preview) => {
            // a dry run: the C15 preview of the merge commit, with the merge fields
            let dr = dr.expect("only a dry run previews");
            let req = dry_run::Request::new(&st, &headers, &dr, false);
            let mut doc = req.json(&tds, &preview);
            let d = ds.clone();
            let mut po = o;
            po.write.dry_run = None;
            let fields =
                blocking(move || Ok(d.store.preview_merge(&source, &target, &po)?)).await?;
            if let Some(obj) = doc.as_object_mut() {
                obj.insert("merge".into(), merge_json(&fields, None));
            }
            Ok(Json(doc).into_response())
        }
        Ok(MergeOutcome::Conflicts(c)) => Err(ApiError(
            StatusCode::CONFLICT,
            serde_json::to_value(c.as_ref()).unwrap_or_default(),
        )),
        Ok(MergeOutcome::UpToDate(r)) => Ok(Json(merge_json(&r, None)).into_response()),
        Ok(MergeOutcome::Merged(r)) => {
            let stale = reasoned && (r.inserted + r.deleted) > 0;
            let seq = r.commit.as_ref().map_or(r.target.seq, |c| c.commit.seq);
            let resp = Json(merge_json(&r, Some(stale))).into_response();
            Ok(with_commit(resp, &tds, seq))
        }
    }
}
