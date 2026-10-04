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
    let mut gauges: std::collections::BTreeMap<String, (u64, u64, u64)> = Default::default();
    // per branch: quads, delta inserts and deletes, log bytes, own disk bytes
    type Per = std::collections::BTreeMap<(String, String), [u64; 5]>;
    let mut per: Per = Default::default();
    let cap = st.metrics.max_datasets().max(1);
    for d in datasets.iter().filter(|d| d.kind == DbType::Persistent) {
        let open = d
            .store
            .branch_set()
            .map(|s| s.open_stores())
            .unwrap_or_default();
        let linked = open
            .iter()
            .filter(|b| b.snapshot().generation.linked().is_some())
            .count() as u64;
        let ds = st.metrics.dataset_label(Some(&d.name));
        let g = gauges.entry(ds.clone()).or_default();
        g.0 += d.store.branch_count() as u64;
        g.1 += linked;
        g.2 += d.store.branch_held_bytes();
        // main, then the open branches by name; past the cap they add up under $other
        let mut stores: Vec<(String, &sparkles::store::Store)> =
            open.iter().map(|b| (b.branch_name(), b.as_ref())).collect();
        stores.sort_by(|a, b| a.0.cmp(&b.0));
        stores.insert(0, (MAIN.to_string(), &d.store));
        for (i, (name, s)) in stores.into_iter().enumerate() {
            let label = if i < cap { name } else { "$other".to_string() };
            let snap = s.snapshot();
            let e = per.entry((ds.clone(), label)).or_default();
            e[0] += snap.len();
            e[1] += snap.delta.inserts() as u64;
            e[2] += snap.delta.deletes() as u64;
            e[3] += s.wal_bytes();
            e[4] += s.branch_own_bytes();
        }
    }
    if !per.is_empty() {
        for (i, (name, help)) in [
            ("sparkles_branch_quads", "Quads on each branch."),
            (
                "sparkles_branch_delta_quads",
                "Inserted and deleted quads of each branch not yet compacted into its index.",
            ),
            (
                "sparkles_branch_wal_bytes",
                "Size of each branch's write-ahead log.",
            ),
            (
                "sparkles_branch_disk_bytes",
                "Size of each branch's own directory (main's: the dataset's, less its branches).",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            family(out, name, "gauge", help);
            for ((ds, b), v) in &per {
                let labels = format!("dataset=\"{}\",branch=\"{}\"", label(ds), label(b));
                match i {
                    0 => {
                        let _ = writeln!(out, "{name}{{{labels}}} {}", v[0]);
                    }
                    1 => {
                        let _ = writeln!(out, "{name}{{{labels},kind=\"insert\"}} {}", v[1]);
                        let _ = writeln!(out, "{name}{{{labels},kind=\"delete\"}} {}", v[2]);
                    }
                    n => {
                        let _ = writeln!(out, "{name}{{{labels}}} {}", v[n + 1]);
                    }
                }
            }
        }
    }
    if !gauges.is_empty() {
        family(
            out,
            "sparkles_branches",
            "gauge",
            "Branches per dataset, main included.",
        );
        for (ds, (n, _, _)) in &gauges {
            let _ = writeln!(out, "sparkles_branches{{dataset=\"{}\"}} {n}", label(ds));
        }
        family(
            out,
            "sparkles_branch_linked",
            "gauge",
            "Open branches that still read their upstream's index files.",
        );
        for (ds, (_, n, _)) in &gauges {
            let _ = writeln!(
                out,
                "sparkles_branch_linked{{dataset=\"{}\"}} {n}",
                label(ds)
            );
        }
        family(
            out,
            "sparkles_branch_held_bytes",
            "gauge",
            "Bytes kept on disk only for branches: upstream generations their links read, and retired branches.",
        );
        for (ds, (_, _, n)) in &gauges {
            let _ = writeln!(
                out,
                "sparkles_branch_held_bytes{{dataset=\"{}\"}} {n}",
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

/// Admin routes whose `branch` names the branch a commit-level merge writes to: their
/// handlers check it.
const PICK_ROUTES: &[&str] = &["revert", "cherry-pick"];

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
        // reverts and cherry-picks name their branch with `branch`, and check it
        if PICK_ROUTES.contains(&seg) {
            return next.run(req).await;
        }
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
pub(super) fn main_dataset(st: &AppState, name: &str) -> ApiResult<Arc<Dataset>> {
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
pub(super) fn check(
    p: &Principal,
    ds: &str,
    name: &str,
    lvl: Level,
    e: Option<Endpoint>,
) -> ApiResult<()> {
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

pub(super) fn commit_ref(c: &NamedCommitRef) -> J {
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
    let exempt: Vec<String> = ds
        .store
        .merge_exempt()?
        .into_iter()
        .map(|p| p.into_string())
        .collect();
    Ok(Json(json!({
        "dataset": name,
        "datasetId": ds.store.dataset_id(),
        "branches": branches,
        "exemptPredicates": exempt,
    }))
    .into_response())
}

/// IRIs of predicates, as `<iri>` or plain.
fn predicates(v: &J, what: &str) -> ApiResult<Vec<oxrdf::NamedNode>> {
    let bad = || {
        invalid(
            "invalid-merge",
            format!("{what}: an array of predicate IRIs"),
        )
    };
    v.as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|p| {
            let s = p.as_str().ok_or_else(bad)?;
            let s = s
                .strip_prefix('<')
                .and_then(|s| s.strip_suffix('>'))
                .unwrap_or(s);
            oxrdf::NamedNode::new(s)
                .map_err(|_| invalid("invalid-merge", format!("{what}: not an IRI: {s}")))
        })
        .collect()
}

/// `PATCH /$/branches/{ds}`: the dataset's branch settings, `exemptPredicates`, the
/// predicates whose cells never conflict in its merges. Needs admin on `main`.
pub(crate) async fn patch_settings(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    body: Bytes,
) -> ApiResult {
    let ds = main_dataset(&st, &name)?;
    check(&p, &name, MAIN, Level::Admin, None)?;
    let v: J = serde_json::from_slice(&body)
        .map_err(|e| invalid("invalid-branch", format!("invalid request body: {e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| invalid("invalid-branch", "the body is a JSON object"))?;
    if let Some(k) = obj.keys().find(|k| *k != "exemptPredicates") {
        return Err(invalid(
            "invalid-branch",
            format!("unknown field {k}: exemptPredicates can change"),
        ));
    }
    let preds = match obj.get("exemptPredicates") {
        Some(x) => Some(predicates(x, "exemptPredicates")?),
        None => None,
    };
    let exempt = blocking(move || {
        Ok(match preds {
            Some(ps) => ds.store.set_merge_exempt(&ps)?,
            None => ds.store.merge_exempt()?,
        })
    })
    .await?;
    let exempt: Vec<String> = exempt.into_iter().map(|p| p.into_string()).collect();
    Ok(Json(json!({ "dataset": name, "exemptPredicates": exempt })).into_response())
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
        .find(|k| !matches!(k.as_str(), "protected" | "note" | "name"))
    {
        return Err(invalid(
            "invalid-branch",
            format!("unknown field {k}: name, protected and note can change"),
        ));
    }
    let rename = match obj.get("name") {
        None => None,
        Some(J::String(n)) if *n == branch => None,
        Some(J::String(n)) => Some(n.clone()),
        Some(_) => return Err(invalid("invalid-branch", "name: a string")),
    };
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
    if let Some(new) = &rename {
        // a rename moves the branch out of the grants of its old name and into those of
        // the new one: write on both, admin for a protected branch, whole datasets
        if ds
            .store
            .branch_info(&branch)
            .map(|i| i.protected)
            .unwrap_or(false)
        {
            check(&p, &name, &branch, Level::Admin, None)?;
        }
        let q = on_branch(&name, new);
        if p.level_at(&q, Endpoint::Branches)
            .is_none_or(|l| l < Level::Write)
        {
            return Err(forbidden(format!(
                "write access to branch {new} of /{name} required"
            )));
        }
        whole(&p, &name, &branch)?;
        whole(&p, &name, new)?;
    }
    let d = ds.clone();
    let (b, r) = (branch.clone(), rename.clone());
    let info = blocking(move || {
        let b = match &r {
            Some(new) => d.store.rename_branch(&b, new)?.name,
            None => b,
        };
        if let Some(on) = protected {
            d.store.set_branch_protected(&b, on)?;
        }
        if let Some(note) = note {
            d.store.set_branch_note(&b, note)?;
        }
        Ok(d.store.branch_info(&b)?)
    })
    .await?;
    let mut j = branch_json(&info);
    let Some(new) = rename else {
        return Ok(Json(j).into_response());
    };
    st.renamed_branch(&ds, &branch, &new);
    // grants name branches in the configuration, which a rename leaves as written
    let changing = grants_changing(&st, &name, &branch, &new);
    if !changing.is_empty() {
        tracing::warn!(
            "/{name}: renaming branch {branch} to {new} changes what {} grant{} cover: {}",
            changing.len(),
            if changing.len() == 1 { "" } else { "s" },
            changing.join("; ")
        );
    }
    j["grantsChanged"] = json!(changing.len());
    let loc = format!("/$/branches/{name}/{new}");
    Ok(([(header::LOCATION, loc)], Json(j)).into_response())
}

/// The configured grants that cover one of branches `old` and `new` of `ds` but not
/// the other.
fn grants_changing(st: &AppState, ds: &str, old: &str, new: &str) -> Vec<String> {
    #[cfg(feature = "auth")]
    if let Some(a) = &st.auth {
        return a.policy().branch_grants_changing(ds, old, new);
    }
    let _ = (st, ds, old, new);
    Vec::new()
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
    let flag = |k: &str| matches!(params.get(k), Some("true" | "1" | ""));
    let o = sparkles::branch::DeleteOptions {
        force: flag("force"),
        reparent: flag("reparent"),
    };
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
    blocking(move || Ok(d.store.delete_branch_with(&b, &o)?)).await?;
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
        Some("replay") => o.replay = true,
        Some(x) => {
            return Err(invalid(
                "invalid-merge",
                format!("ff: auto, only or replay, not {x}"),
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
    if let Some(x) = v.get("exempt") {
        o.exempt = predicates(x, "exempt")?;
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
        // a merge that records its second parent
        if rc.commit.kind == sparkles::commit::CommitKind::Merge && !r.squashed {
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
        "replayed": (!r.replayed.is_empty()).then(|| {
            r.replayed
                .iter()
                .map(|c| json!({
                    "from": commit_ref(&c.from),
                    "commit": c.receipt.as_ref().map(|rc| rc.commit.seq),
                }))
                .collect::<Vec<J>>()
        }),
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

/// A merge of some kind: what it merges, and where.
#[derive(Clone, Debug)]
pub(crate) enum Op {
    Merge {
        source: String,
        target: String,
    },
    /// undo commit `commit` of `branch`'s history on that branch
    Revert {
        branch: String,
        commit: u64,
    },
    /// apply commit `commit` of `source`'s history to `branch`
    CherryPick {
        source: String,
        commit: u64,
        branch: String,
    },
}

impl Op {
    /// The branch the operation writes to.
    fn target(&self) -> &str {
        match self {
            Op::Merge { target, .. } => target,
            Op::Revert { branch, .. } | Op::CherryPick { branch, .. } => branch,
        }
    }

    fn run(&self, s: &sparkles::store::Store, o: &MergeOptions) -> sparkles::Result<MergeOutcome> {
        match self {
            Op::Merge { source, target } => s.merge(source, target, o),
            Op::Revert { branch, commit } => s.revert(branch, *commit, o),
            Op::CherryPick {
                source,
                commit,
                branch,
            } => s.cherry_pick(source, *commit, branch, o),
        }
    }

    fn preview(
        &self,
        s: &sparkles::store::Store,
        o: &MergeOptions,
    ) -> sparkles::Result<MergeReport> {
        match self {
            Op::Merge { source, target } => s.preview_merge(source, target, o),
            Op::Revert { branch, commit } => s.preview_revert(branch, *commit, o),
            Op::CherryPick {
                source,
                commit,
                branch,
            } => s.preview_cherry_pick(source, *commit, branch, o),
        }
    }

    /// The caller's access: read on what is merged, write on the target, from grants
    /// without graph restrictions.
    fn access(&self, p: &Principal, ds: &str) -> ApiResult<()> {
        let source = match self {
            Op::Merge { source, .. } => source,
            Op::Revert { branch, .. } => branch,
            Op::CherryPick { source, .. } => source,
        };
        check(p, ds, source, Level::Read, Some(Endpoint::Merge))?;
        check(p, ds, self.target(), Level::Write, Some(Endpoint::Merge))?;
        whole(p, ds, source)?;
        whole(p, ds, self.target())
    }

    /// The result's JSON: the merge fields, and what a revert undid.
    fn json(&self, r: &MergeReport, stale: Option<bool>) -> J {
        let mut j = merge_json(r, stale);
        if let Op::Revert { branch, commit } = self {
            j["reverted"] = json!({ "branch": branch, "seq": commit });
        }
        if let Op::CherryPick { source, commit, .. } = self {
            j["picked"] = json!({ "branch": source, "seq": commit });
        }
        j
    }
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
    let v = query_options(&params, &["source", "target", "ff"])?;
    let ask = merge_options(&st, &v, true)?;
    let op = Op::Merge {
        source: ask.source,
        target: ask.target,
    };
    run_preview(&p, &name, ds, op, ask.o).await
}

/// Merge options given as query parameters (the previews), as the JSON body has them.
fn query_options(params: &Params, own: &[&str]) -> ApiResult<J> {
    let mut v = serde_json::Map::new();
    for k in own
        .iter()
        .copied()
        .chain(["conflicts", "onConflict", "inferences"])
    {
        if let Some(x) = params.get(k) {
            v.insert(k.into(), x.into());
        }
    }
    if let Some(x) = params.get("squash") {
        v.insert("squash".into(), matches!(x, "true" | "1" | "").into());
    }
    let exempt = params.all("exempt");
    if !exempt.is_empty() {
        v.insert("exempt".into(), exempt.into());
    }
    if let Some(l) = params.get("limit") {
        v.insert(
            "limit".into(),
            l.parse::<u64>()
                .map_err(|_| invalid("invalid-merge", "limit: 1 to 10000"))?
                .into(),
        );
    }
    Ok(J::Object(v))
}

async fn run_preview(
    p: &Principal,
    name: &str,
    ds: Arc<Dataset>,
    op: Op,
    o: MergeOptions,
) -> ApiResult {
    op.access(p, name)?;
    let op2 = op.clone();
    let r = blocking(move || Ok(op2.preview(&ds.store, &o)?)).await?;
    Ok(Json(op.json(&r, None)).into_response())
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
    let ask = merge_options(&st, &v, false)?;
    let op = Op::Merge {
        source: ask.source,
        target: ask.target,
    };
    execute(&st, &p, &name, ds, &headers, &v, op, ask.o, ask.dry_run).await
}

/// The options of a revert or a cherry-pick: the JSON body's merge options, which may
/// not choose how fast-forwards are made or squash.
fn pick_options(st: &AppState, body: &Bytes, preview: bool) -> ApiResult<(J, MergeAsk)> {
    let mut v: J = if body.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        serde_json::from_slice(body)
            .map_err(|e| invalid("invalid-merge", format!("invalid request body: {e}")))?
    };
    let obj = v
        .as_object_mut()
        .ok_or_else(|| invalid("invalid-merge", "the body is a JSON object"))?;
    for k in ["source", "target", "ff", "squash", "base"] {
        if obj.contains_key(k) {
            return Err(invalid(
                "invalid-merge",
                format!("{k} does not apply here: the query names the branches and the commit"),
            ));
        }
    }
    if let Some(e) = obj.get("expect")
        && e.get("source").is_some()
    {
        return Err(invalid(
            "invalid-merge",
            "expect takes target here: a commit does not move",
        ));
    }
    obj.insert("source".into(), MAIN.into());
    let ask = merge_options(st, &v, preview)?;
    Ok((v, ask))
}

/// `commit=N` of a revert or a cherry-pick.
fn commit_param(params: &Params) -> ApiResult<u64> {
    let c = params
        .get("commit")
        .ok_or_else(|| invalid("invalid-merge", "commit is required"))?;
    c.strip_prefix("commit:")
        .unwrap_or(c)
        .parse()
        .map_err(|_| invalid("invalid-merge", format!("commit: a commit number, not {c}")))
}

/// The branch `branch=` names (default `main`).
fn branch_param(params: &Params) -> String {
    params.get("branch").unwrap_or(MAIN).to_string()
}

/// `GET /$/revert/{ds}?branch=&commit=`: what reverting the commit would do.
pub(crate) async fn preview_revert(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    let ds = main_dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let mut v = query_options(&params, &[])?;
    v["source"] = MAIN.into();
    let ask = merge_options(&st, &v, true)?;
    let op = Op::Revert {
        branch: branch_param(&params),
        commit: commit_param(&params)?,
    };
    run_preview(&p, &name, ds, op, ask.o).await
}

/// `POST /$/revert/{ds}?branch=&commit=`: revert a commit of the branch's history on the
/// branch, with the merge options of the JSON body, if any.
pub(crate) async fn revert(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let ds = main_dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let (v, ask) = pick_options(&st, &body, false)?;
    let op = Op::Revert {
        branch: branch_param(&params),
        commit: commit_param(&params)?,
    };
    execute(&st, &p, &name, ds, &headers, &v, op, ask.o, ask.dry_run).await
}

/// Run a merge of some kind and answer its result: `200` with the result, `409` with
/// the conflicts, or for a dry run the write preview with the merge fields.
#[allow(clippy::too_many_arguments)]
async fn execute(
    st: &Arc<AppState>,
    p: &Principal,
    name: &str,
    ds: Arc<Dataset>,
    headers: &HeaderMap,
    v: &J,
    op: Op,
    mut o: MergeOptions,
    dry_run: bool,
) -> ApiResult {
    op.access(p, name)?;
    if let Some(a) = p.log_name() {
        o.write.author = Some(a.into());
    }
    let dr = dry_run.then(|| sparkles::preview::DryRun {
        changes: v
            .get("changes")
            .and_then(J::as_u64)
            .unwrap_or(0)
            .min(sparkles::preview::MAX_LISTED_CHANGES as u64) as usize,
        all_changes: false,
        max_changes: st.limits.max_rows as u64,
    });
    o.write.dry_run = dr.clone();
    let tds = st.branch_dataset(&ds, op.target())?;
    if dr.is_none() && respond_async(headers) {
        return start_task(st, name, ds, op, o);
    }
    let reasoned = tds.reasoning.read().is_some();
    let d = ds.clone();
    let (op2, o2) = (op.clone(), o.clone());
    let t0 = std::time::Instant::now();
    let out = blocking(move || {
        Ok(match op2.run(&d.store, &o2) {
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
            count_merge(st, name, "refused", None, secs);
            return Err(e);
        }
    };
    if let Ok(o) = &out {
        count_outcome(st, name, o, secs);
    }
    match out {
        Err(preview) => {
            // a dry run: the C15 preview of the merge commit, with the merge fields
            let dr = dr.expect("only a dry run previews");
            let req = dry_run::Request::new(st, headers, &dr, false);
            let mut doc = req.json(&tds, &preview);
            let d = ds.clone();
            let mut po = o;
            po.write.dry_run = None;
            let op2 = op.clone();
            let fields = blocking(move || Ok(op2.preview(&d.store, &po)?)).await?;
            if let Some(obj) = doc.as_object_mut() {
                obj.insert("merge".into(), op.json(&fields, None));
            }
            Ok(Json(doc).into_response())
        }
        Ok(MergeOutcome::Conflicts(c)) => Err(ApiError(
            StatusCode::CONFLICT,
            serde_json::to_value(c.as_ref()).unwrap_or_default(),
        )),
        Ok(MergeOutcome::UpToDate(r)) => Ok(Json(op.json(&r, None)).into_response()),
        Ok(MergeOutcome::Merged(r)) => {
            let stale = reasoned && (r.inserted + r.deleted) > 0;
            let seq = r.commit.as_ref().map_or(r.target.seq, |c| c.commit.seq);
            let resp = Json(op.json(&r, Some(stale))).into_response();
            Ok(with_commit(resp, &tds, seq))
        }
    }
}

/// Whether the request asks for an asynchronous answer (RFC 7240 `Prefer:
/// respond-async`).
fn respond_async(h: &HeaderMap) -> bool {
    h.get_all("prefer").iter().any(|v| {
        v.to_str().is_ok_and(|s| {
            s.split(',')
                .any(|p| p.trim().eq_ignore_ascii_case("respond-async"))
        })
    })
}

/// Run a merge as a cancellable task: `202` with the task and `Location: /$/tasks/{id}`.
/// The task's `detail` is the result, or the conflict report of a merge that conflicts
/// (the task then fails).
fn start_task(
    st: &Arc<AppState>,
    name: &str,
    ds: Arc<Dataset>,
    op: Op,
    o: MergeOptions,
) -> ApiResult {
    task_start_check(st, None, name)?;
    let kind = match &op {
        Op::Merge { .. } => "merge",
        Op::Revert { .. } => "revert",
        Op::CherryPick { .. } => "cherry-pick",
    };
    let (st2, n) = (st.clone(), name.to_string());
    let task = st.start_task_opts(st.next_task_id(), kind, name, None, true, move |h| {
        let ctl = h.control();
        // a task has no request deadline: it runs until it ends or is cancelled
        let mut o = o.with_control(&ctl);
        o.deadline = None;
        o.write.deadline = None;
        let t0 = std::time::Instant::now();
        let out = op.run(&ds.store, &o);
        let secs = t0.elapsed().as_secs_f64();
        match out {
            Err(e) => {
                count_merge(&st2, &n, "refused", None, secs);
                Err(e.into())
            }
            Ok(out) => {
                count_outcome(&st2, &n, &out, secs);
                match out {
                    MergeOutcome::Conflicts(c) => {
                        h.set_detail(serde_json::to_value(c.as_ref()).unwrap_or_default());
                        Err(anyhow::anyhow!("{}", c.error))
                    }
                    MergeOutcome::UpToDate(r) => {
                        h.set_detail(op.json(&r, None));
                        Ok("up to date: nothing to change".into())
                    }
                    MergeOutcome::Merged(r) => {
                        h.set_detail(op.json(&r, None));
                        Ok(match r.commit.as_ref() {
                            Some(c) => format!(
                                "commit {} on {}: +{} -{}",
                                c.commit.seq,
                                op.target(),
                                r.inserted,
                                r.deleted
                            ),
                            None => format!("+{} -{}", r.inserted, r.deleted),
                        })
                    }
                }
            }
        }
    });
    let loc = format!("/$/tasks/{}", task.id);
    Ok((
        StatusCode::ACCEPTED,
        [
            (header::LOCATION, loc),
            (
                header::HeaderName::from_static("preference-applied"),
                "respond-async".to_string(),
            ),
        ],
        Json(task),
    )
        .into_response())
}

/// Count a merge's outcome in the merge metrics.
fn count_outcome(st: &AppState, name: &str, out: &MergeOutcome, secs: f64) {
    match out {
        MergeOutcome::Conflicts(c) => {
            let mut m = MERGES.lock();
            let e = m.entry(st.metrics.dataset_label(Some(name))).or_default();
            e.conflicts += c.conflicts;
            *e.results.entry("conflict").or_default() += 1;
            e.seconds += secs;
            e.count += 1;
        }
        MergeOutcome::UpToDate(r) => count_merge(st, name, "up-to-date", Some(r), secs),
        MergeOutcome::Merged(r) => count_merge(
            st,
            name,
            if r.fast_forward && !r.squashed {
                "fast-forward"
            } else {
                "merged"
            },
            Some(r),
            secs,
        ),
    }
}

/// The cherry-pick a request names: `source=` and `commit=` the commit, `branch=` the
/// branch it is applied to (default `main`).
fn cherry_pick_op(params: &Params) -> ApiResult<Op> {
    let source = params
        .get("source")
        .ok_or_else(|| invalid("invalid-merge", "source is required"))?
        .to_string();
    if source != MAIN && !sparkles::branch::valid_name(&source) {
        return Err(invalid(
            "invalid-branch",
            format!("invalid branch name '{source}'"),
        ));
    }
    Ok(Op::CherryPick {
        source,
        commit: commit_param(params)?,
        branch: branch_param(params),
    })
}

/// `GET /$/cherry-pick/{ds}?source=&commit=&branch=`: what applying the commit would do.
pub(crate) async fn preview_cherry_pick(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    let ds = main_dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let mut v = query_options(&params, &[])?;
    v["source"] = MAIN.into();
    let ask = merge_options(&st, &v, true)?;
    let op = cherry_pick_op(&params)?;
    run_preview(&p, &name, ds, op, ask.o).await
}

/// `POST /$/cherry-pick/{ds}?source=&commit=&branch=`: apply a commit of the source
/// branch's history to the branch, with the merge options of the JSON body, if any.
pub(crate) async fn cherry_pick(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let ds = main_dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let (v, ask) = pick_options(&st, &body, false)?;
    let op = cherry_pick_op(&params)?;
    execute(&st, &p, &name, ds, &headers, &v, op, ask.o, ask.dry_run).await
}
