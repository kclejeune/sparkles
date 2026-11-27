//! `POST /{ds}/claims/review` and `POST /{ds}/claims/withdraw` (spec C25 §5 and §6):
//! review decisions with their accepted projections, and claim withdrawals.
//!
//! Authorization: `write` on `{ds}` (or the branch) through the update endpoint (the
//! route table in `auth/routes.rs`). A review needs update and read grants on its
//! policy's decisions, supports and output graphs and read access to each target and,
//! for an acceptance, to the target's evidence, decided as `POST /{ds}/claims/resolve`
//! decides it. A withdrawal needs update grants on each claim's graph and on the output
//! graph, and read grants on the decisions, supports and output graphs, of every review
//! policy. A review under a matching policy decides identity proposals and needs the
//! same grants on the policy's decisions, maps and output graphs. `POST
//! /{ds}/claims/identities` reads a resolution map and needs `read` through the query
//! endpoint and read grants on the policy's graphs. A record the caller cannot read
//! answers
//! as a missing one. The caller's credential is checked again right before the commit
//! publishes, as for `POST /{ds}/claims`.

use super::claims::{CallerFence, digest, principal_iri};
use super::{bad, named, policy};
use crate::auth::Principal;
use crate::http::{AdminBody, ApiResult, err};
use crate::state::AppState;
use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{Value as J, json};
use sparkles::evidence::{Changed, DecisionInput, Disposition, ReviewOptions, WithdrawalInput};
use sparkles::task::Control;
use std::sync::Arc;

type St = State<Arc<AppState>>;

/// The most bytes a review or withdrawal body holds: 1000 entries of a few IRIs each.
const MAX_BYTES: usize = 4 << 20;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewRequest {
    policy: String,
    policy_version: u32,
    if_head: u64,
    idempotency_key: Option<String>,
    #[serde(default)]
    dry_run: bool,
    message: Option<String>,
    decisions: Vec<DecisionJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DecisionJson {
    key: String,
    target: String,
    target_digest: String,
    disposition: String,
    supersedes: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WithdrawRequest {
    if_head: Option<u64>,
    idempotency_key: Option<String>,
    #[serde(default)]
    dry_run: bool,
    message: Option<String>,
    withdrawals: Vec<WithdrawalJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WithdrawalJson {
    key: String,
    target: String,
    target_digest: String,
}

fn invalid(what: &str, e: impl std::fmt::Display) -> crate::http::ApiError {
    crate::http::err_code(
        StatusCode::BAD_REQUEST,
        "invalid-proposal",
        format!("invalid {what}: {e}"),
    )
}

fn key_of(field: String, key: &str) -> ApiResult<String> {
    if key.is_empty() || key.len() > 256 {
        return Err(bad(field, "a key has 1 to 256 bytes"));
    }
    Ok(key.to_string())
}

fn decisions(r: &[DecisionJson]) -> ApiResult<Vec<DecisionInput>> {
    let mut out = Vec::with_capacity(r.len());
    for (i, d) in r.iter().enumerate() {
        let at = format!("decisions[{i}]");
        out.push(DecisionInput {
            key: key_of(format!("{at}.key"), &d.key)?,
            target: named(&format!("{at}.target"), &d.target)?,
            target_digest: digest(&format!("{at}.targetDigest"), &d.target_digest)?,
            disposition: Disposition::from_name(&d.disposition).ok_or_else(|| {
                bad(
                    format!("{at}.disposition"),
                    "use accepted, rejected or needsEvidence",
                )
            })?,
            supersedes: d
                .supersedes
                .as_deref()
                .map(|s| named(&format!("{at}.supersedes"), s))
                .transpose()?,
        });
    }
    Ok(out)
}

fn withdrawals(r: &[WithdrawalJson]) -> ApiResult<Vec<WithdrawalInput>> {
    let mut out = Vec::with_capacity(r.len());
    for (i, w) in r.iter().enumerate() {
        let at = format!("withdrawals[{i}]");
        out.push(WithdrawalInput {
            key: key_of(format!("{at}.key"), &w.key)?,
            target: named(&format!("{at}.target"), &w.target)?,
            target_digest: digest(&format!("{at}.targetDigest"), &w.target_digest)?,
        });
    }
    Ok(out)
}

fn changed_json(c: &Changed) -> J {
    let change = |v: &[sparkles::evidence::ProjectionChange]| -> J {
        v.iter()
            .map(|c| json!({"output": c.output.as_str(), "triple": c.triple.to_string()}))
            .collect::<Vec<_>>()
            .into()
    };
    let names = |v: &[oxrdf::NamedNode]| -> J {
        v.iter()
            .map(|n| J::from(n.as_str()))
            .collect::<Vec<_>>()
            .into()
    };
    let mut out = json!({
        "supportsAdded": names(&c.supports_added),
        "supportsEnded": names(&c.supports_ended),
        "inserted": change(&c.inserted),
        "removed": change(&c.removed),
    });
    if let Some(m) = &c.map {
        out["map"] = json!({
            "policy": m.policy.as_str(),
            "revision": m.revision.as_str(),
            "number": m.number,
            "previous": m.previous.as_ref().map(|p| p.as_str()),
            "decisionsAdded": names(&m.decisions_added),
            "decisionsEnded": names(&m.decisions_ended),
            "entities": names(&m.entities),
        });
    }
    out
}

fn preview_json(out: &mut J, preview: Option<&sparkles::preview::Preview>) {
    if let Some(preview) = preview {
        let commit = preview.receipt_commit();
        out["dryRun"] = json!({
            "wouldCommit": matches!(preview.outcome(), sparkles::preview::Outcome::Commit),
            "inserted": commit.inserted,
            "deleted": commit.deleted,
            "error": preview.error(),
        });
    }
}

/// The datasets, options and controls a typed operation on `{ds}` runs with.
struct Prepared {
    main: Arc<crate::state::Dataset>,
    logical: Arc<crate::state::Dataset>,
    options: ReviewOptions,
    control: Control,
    fence: Arc<CallerFence>,
}

#[allow(clippy::too_many_arguments)]
fn prepare(
    st: &Arc<AppState>,
    name: &str,
    p: &Principal,
    branch: &str,
    execution: &super::Execution,
    idempotency_key: Option<String>,
    if_head: Option<u64>,
    dry_run: bool,
    message: Option<String>,
) -> ApiResult<Prepared> {
    use crate::auth::{Endpoint, on_branch};
    let logical = execution.dataset(st, name)?;
    let main = if branch == sparkles::branch::MAIN {
        logical.clone()
    } else {
        crate::http::main_dataset(st, name)?
    };
    let here = on_branch(&main.name, branch);
    let caller = crate::http::caller::CallerContext::for_caller(st, &logical, p);
    let opts = caller.options_on(&crate::http::Params::empty(), &here, Endpoint::Update);
    let control = execution.control(&opts);
    let mut options = ReviewOptions::new(principal_iri(p));
    options.idempotency_key = idempotency_key;
    options.if_head = if_head;
    options.dry_run = dry_run;
    options.message = message.map(Arc::from);
    options.author = crate::http::author(p);
    options.write = opts.graphs.clone();
    let drift = crate::settings::evidence::DriftFence::capture(st, &main)?;
    let fence = Arc::new(CallerFence {
        st: st.clone(),
        p: p.clone(),
        drift,
    });
    options.authority = Some(fence.clone());
    Ok(Prepared {
        main,
        logical,
        options,
        control,
        fence,
    })
}

fn body_of<T: serde::de::DeserializeOwned>(
    st: &AppState,
    bytes: &[u8],
    what: &str,
) -> ApiResult<T> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    if bytes.len() > MAX_BYTES {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("a {what} holds at most {MAX_BYTES} bytes"),
        ));
    }
    serde_json::from_slice(bytes).map_err(|e| invalid(what, e))
}

/// `POST /{ds}/claims/review`: append decisions under one review policy. `200` with the
/// operation, its decisions and what changed, committed, replayed or previewed. `400`
/// for a malformed request or an unknown policy, `403` for a graph the caller may not
/// update or read, `409` for `policy-changed`, `head-changed`, `target-changed`,
/// `decision-superseded`, `target-withdrawn`, `evidence-conflict` and
/// `idempotency-conflict`, `422` for an unavailable target or evidence, and `503` for
/// `projection-unavailable` and `authority-changed`.
pub(super) async fn review(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(bytes): AdminBody,
) -> ApiResult {
    super::run_http("review", st, name, p, bytes).await
}

pub(super) fn review_now(
    st: Arc<AppState>,
    name: String,
    p: Principal,
    bytes: axum::body::Bytes,
    execution: super::Execution,
) -> ApiResult<J> {
    let r: ReviewRequest = body_of(&st, &bytes, "review")?;
    let input = decisions(&r.decisions)?;
    let branch = execution.branch.clone();
    let prepared = prepare(
        &st,
        &name,
        &p,
        &branch,
        &execution,
        r.idempotency_key,
        Some(r.if_head),
        r.dry_run,
        r.message,
    )?;
    let mut cache = ReadCache::default();
    let mut read = |ds: &sparkles::Dataset, b: &str| cache.get(&st, &p, ds, b);
    let reviewed = sparkles::evidence::review(
        Some(&st.catalog),
        &prepared.main.dataset,
        &prepared.logical.dataset,
        &r.policy,
        r.policy_version,
        &input,
        &prepared.options,
        &mut read,
        &prepared.control,
    )?;
    let mut out = json!({
        "operation": reviewed.operation.as_str(),
        "committed": reviewed.committed,
        "replayed": reviewed.replayed,
        "commit": reviewed.commit,
        "branchId": reviewed.branch,
        "decisions": reviewed.decisions.iter().map(|d| json!({
            "key": d.key,
            "iri": d.iri.as_str(),
            "target": d.target.as_str(),
            "disposition": d.disposition.name(),
            "supersedes": d.supersedes.as_ref().map(|s| s.as_str()),
            "support": d.support.as_ref().map(|s| s.as_str()),
            "notProjected": d.not_projected,
            "conflicts": d.conflicts.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "changed": changed_json(&reviewed.changed),
        "mapRevision": reviewed.map_revision.as_ref().map(|m| m.as_str()),
    });
    preview_json(&mut out, reviewed.preview.as_deref());
    prepared.fence.check_uncommitted(reviewed.committed)?;
    Ok(out)
}

/// `POST /{ds}/claims/withdraw`: withdraw claim occurrences. `200` with the operation,
/// each withdrawal and what changed. `400` for a malformed request, `403` for a graph
/// the caller may not update or read, `409` for `head-changed`, `target-changed` and
/// `idempotency-conflict`, `422` for an unavailable claim, and `503` for
/// `projection-unavailable` and `authority-changed`.
pub(super) async fn withdraw(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(bytes): AdminBody,
) -> ApiResult {
    super::run_http("withdraw", st, name, p, bytes).await
}

pub(super) fn withdraw_now(
    st: Arc<AppState>,
    name: String,
    p: Principal,
    bytes: axum::body::Bytes,
    execution: super::Execution,
) -> ApiResult<J> {
    let r: WithdrawRequest = body_of(&st, &bytes, "withdrawal")?;
    let input = withdrawals(&r.withdrawals)?;
    let branch = execution.branch.clone();
    let prepared = prepare(
        &st,
        &name,
        &p,
        &branch,
        &execution,
        r.idempotency_key,
        r.if_head,
        r.dry_run,
        r.message,
    )?;
    let mut cache = ReadCache::default();
    let mut read = |ds: &sparkles::Dataset, b: &str| cache.get(&st, &p, ds, b);
    let withdrawn = sparkles::evidence::withdraw(
        &prepared.main.dataset,
        &prepared.logical.dataset,
        &input,
        &prepared.options,
        &mut read,
        &prepared.control,
    )?;
    let mut out = json!({
        "operation": withdrawn.operation.as_str(),
        "committed": withdrawn.committed,
        "replayed": withdrawn.replayed,
        "commit": withdrawn.commit,
        "branchId": withdrawn.branch,
        "withdrawals": withdrawn.withdrawals.iter().map(|w| json!({
            "key": w.key,
            "iri": w.iri.as_ref().map(|s| s.as_str()),
            "target": w.target.as_str(),
            "existing": w.existing,
        })).collect::<Vec<_>>(),
        "changed": changed_json(&withdrawn.changed),
    });
    preview_json(&mut out, withdrawn.preview.as_deref());
    prepared.fence.check_uncommitted(withdrawn.committed)?;
    Ok(out)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdentitiesRequest {
    policy: String,
    entities: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RebuildIdentitiesRequest {
    policy: String,
    policy_version: Option<u32>,
    if_head: Option<u64>,
    idempotency_key: Option<String>,
    #[serde(default)]
    dry_run: bool,
    message: Option<String>,
}

/// `POST /$/evidence/{ds}/identities/rebuild`: rebuild a matching policy's resolution map
/// in the current boundary, rewriting the output graph atomically and recording the new
/// protections and boundary in a fresh map revision. Needs unrestricted `admin` on `{ds}`.
pub(super) async fn rebuild_identities(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(bytes): AdminBody,
) -> ApiResult {
    super::run_http("rebuild-identities", st, name, p, bytes).await
}

pub(super) fn rebuild_identities_now(
    st: Arc<AppState>,
    name: String,
    p: Principal,
    bytes: axum::body::Bytes,
    execution: super::Execution,
) -> ApiResult<J> {
    let r: RebuildIdentitiesRequest = body_of(&st, &bytes, "resolution map rebuild")?;
    let branch = execution.branch.clone();
    let prepared = prepare(
        &st,
        &name,
        &p,
        &branch,
        &execution,
        r.idempotency_key,
        r.if_head,
        r.dry_run,
        r.message,
    )?;
    let here = crate::auth::on_branch(&prepared.main.name, &branch);
    if p.restricted(&here) {
        return Err(err(
            StatusCode::FORBIDDEN,
            format!("rebuilding a resolution map needs unrestricted admin access on /{here}"),
        ));
    }
    let rebuilt = sparkles::evidence::rebuild_resolution_map(
        &prepared.main.dataset,
        &prepared.logical.dataset,
        &r.policy,
        r.policy_version,
        &prepared.options,
        &prepared.control,
    )?;
    let mut out = json!({
        "operation": rebuilt.operation.as_str(),
        "committed": rebuilt.committed,
        "replayed": rebuilt.replayed,
        "commit": rebuilt.commit,
        "branchId": rebuilt.branch,
        "policy": rebuilt.policy.as_str(),
        "policyVersion": rebuilt.policy_version,
        "revision": rebuilt.revision.as_str(),
        "number": rebuilt.number,
        "previous": rebuilt.previous.as_ref().map(|p| p.as_str()),
        "boundary": rebuilt.boundary.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
        "changed": changed_json(&rebuilt.changed),
    });
    preview_json(&mut out, rebuilt.preview.as_deref());
    prepared.fence.check_uncommitted(rebuilt.committed)?;
    Ok(out)
}

/// `POST /{ds}/claims/identities`: what the current resolution map of a matching
/// policy selects for each entity. `200` with the map's revision and, for each entity,
/// its representative, its cluster's members and the accepted decisions that join it.
/// `400` for a malformed request or an unknown policy, `403` for a graph of the policy
/// the caller may not read, `422` for `incomplete-evidence` and `503` for
/// `projection-unavailable`.
pub(super) async fn identities(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(bytes): AdminBody,
) -> ApiResult {
    super::run_http("identities", st, name, p, bytes).await
}

pub(super) fn identities_now(
    st: Arc<AppState>,
    name: String,
    p: Principal,
    bytes: axum::body::Bytes,
    execution: super::Execution,
) -> ApiResult<J> {
    if bytes.len() > MAX_BYTES {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("an identity resolution holds at most {MAX_BYTES} bytes"),
        ));
    }
    let r: IdentitiesRequest =
        serde_json::from_slice(&bytes).map_err(|e| invalid("identity resolution", e))?;
    if r.entities.len() > sparkles::evidence::MAX_RESOLVE {
        return Err(bad(
            "entities".into(),
            format!(
                "a resolution names 1 to {} entities",
                sparkles::evidence::MAX_RESOLVE
            ),
        ));
    }
    let entities = r
        .entities
        .iter()
        .enumerate()
        .map(|(i, e)| named(&format!("entities[{i}]"), e))
        .collect::<ApiResult<Vec<_>>>()?;
    let branch = execution.branch.clone();
    use crate::auth::{Endpoint, on_branch};
    let logical = execution.dataset(&st, &name)?;
    let main = if branch == sparkles::branch::MAIN {
        logical.clone()
    } else {
        crate::http::main_dataset(&st, &name)?
    };
    let here = on_branch(&main.name, &branch);
    let caller = crate::http::caller::CallerContext::for_caller(&st, &logical, &p);
    let opts = caller.options_on(&crate::http::Params::empty(), &here, Endpoint::Query);
    let control = execution.control(&opts);
    let resolved = sparkles::evidence::resolve_entities(
        &main.dataset,
        &logical.dataset,
        &r.policy,
        &entities,
        opts.graphs.clone(),
        &control,
    )?;
    fn names(v: &[oxrdf::NamedNode]) -> Vec<&str> {
        v.iter().map(|n| n.as_str()).collect()
    }
    Ok(json!({
        "policy": resolved.policy.as_str(),
        "revision": resolved.revision.as_ref().map(|r| r.as_str()),
        "number": resolved.number,
        "commit": resolved.commit,
        "entities": resolved.entities.iter().map(|e| json!({
            "entity": e.entity.as_str(),
            "representative": e.representative.as_str(),
            "members": names(&e.members),
            "membersComplete": e.members_complete,
            "decisions": names(&e.decisions),
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DependentsRequest {
    changed: Vec<String>,
    #[serde(default)]
    transitive: bool,
}

/// `POST /{ds}/claims/dependents`: the authenticated assessments in the proposal graphs
/// the caller can read whose `ev:target` or `prov:used` names one of the changed record
/// identities, and the community reports in guarded reports graphs proven for the caller
/// (C22 §6) whose generating activity recorded `prov:used` for one of them (including
/// person-edited reports via `gr:editOf`), and with `transitive` the assessments and
/// reports that depend on those. `200` with the commit read, each dependent assessment
/// and each dependent report. `400` for a malformed request, `422 incomplete-evidence`
/// beyond the bound. Needs `read` through the query endpoint and counts as a query.
pub(super) async fn dependents(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(bytes): AdminBody,
) -> ApiResult {
    super::run_http("dependents", st, name, p, bytes).await
}

pub(super) fn dependents_now(
    st: Arc<AppState>,
    name: String,
    p: Principal,
    bytes: axum::body::Bytes,
    execution: super::Execution,
) -> ApiResult<J> {
    if bytes.len() > MAX_BYTES {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("a dependents request holds at most {MAX_BYTES} bytes"),
        ));
    }
    let r: DependentsRequest =
        serde_json::from_slice(&bytes).map_err(|e| invalid("dependents request", e))?;
    if r.changed.is_empty() || r.changed.len() > sparkles::evidence::MAX_CHANGED {
        return Err(bad(
            "changed".into(),
            format!(
                "a request names 1 to {} identities",
                sparkles::evidence::MAX_CHANGED
            ),
        ));
    }
    let changed = r
        .changed
        .iter()
        .enumerate()
        .map(|(i, e)| named(&format!("changed[{i}]"), e))
        .collect::<ApiResult<Vec<_>>>()?;
    let branch = execution.branch.clone();
    use crate::auth::{Endpoint, on_branch};
    let logical = execution.dataset(&st, &name)?;
    let main = if branch == sparkles::branch::MAIN {
        logical.clone()
    } else {
        crate::http::main_dataset(&st, &name)?
    };
    let here = on_branch(&main.name, &branch);
    let caller = crate::http::caller::CallerContext::for_caller(&st, &logical, &p);
    let opts = caller.options_on(&crate::http::Params::empty(), &here, Endpoint::Query);
    let control = execution.control(&opts);
    let found = sparkles::evidence::find_dependents(
        &main.dataset,
        &logical.dataset,
        &changed,
        r.transitive,
        opts.graphs.clone(),
        &control,
    )?;
    Ok(json!({
        "commit": found.commit,
        "dependents": found.dependents.iter().map(|d| json!({
            "assessment": d.assessment.as_str(),
            "graph": d.graph.as_str(),
            "on": d.on.as_str(),
            "dependence": d.dependence.name(),
            "depth": d.depth,
        })).collect::<Vec<_>>(),
        "reports": found.reports.iter().map(|rep| json!({
            "report": rep.report.as_str(),
            "community": rep.community.as_ref().map(|c| c.as_str()),
            "graph": rep.graph.as_str(),
            "on": rep.on.as_str(),
            "dependence": rep.dependence.name(),
            "depth": rep.depth,
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReconcileProjectionRequest {
    policy: String,
    policy_version: Option<u32>,
    if_head: Option<u64>,
    idempotency_key: Option<String>,
    #[serde(default)]
    dry_run: bool,
    message: Option<String>,
}

/// `POST /$/evidence/{ds}/projections/reconcile`: administered reconciliation of a
/// review policy's accepted projection after a changed protection. Rebuilds the output
/// graph from the live authenticated supports it vouches for (or empties it when none
/// vouch) and records the current protections in a fresh `ev:Projection` record, or
/// reuses the active projection record when protections and output already match. Needs
/// `admin` on the dataset (or selected branch).
pub(super) async fn reconcile_projection(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(bytes): AdminBody,
) -> ApiResult {
    let request: ReconcileProjectionRequest = body_of(&st, &bytes, "projection reconciliation")?;
    let (cancel, _cancel_on_drop) = crate::http::cancel_on_drop();
    let grant = crate::server_memory::acquire(&st).await?.map(Arc::new);
    let execution = super::Execution {
        branch: crate::http::branches::current()
            .unwrap_or_else(|| sparkles::branch::MAIN.to_string()),
        cancel,
        deadline: None,
        grant,
    };
    match crate::http::blocking(move || reconcile_projection_now(st, name, p, request, execution))
        .await
    {
        Ok((len, body)) => Ok((
            [
                (
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("application/json"),
                ),
                (
                    axum::http::header::CONTENT_LENGTH,
                    axum::http::HeaderValue::from(len),
                ),
            ],
            axum::body::Body::new(body),
        )
            .into_response()),
        Err(error) if error.audit_code() == "authority-changed" => {
            Ok(crate::auth::routes::authority_changed_response())
        }
        Err(error) => Err(error),
    }
}

/// Chunk size yielded per frame from [`HeldBody`] so the reservation and underlying pool
/// grant stay live while the response stream is consumed.
const RESPONSE_CHUNK_BYTES: usize = 64 << 10;

/// Backing JSON buffer paired with its memory reservation. `buf` is declared before
/// `_held` so the allocation frees before its pool credit returns, and attaching this
/// owner via [`axum::body::Bytes::from_owner`] keeps the reservation live until every
/// emitted frame slice has dropped.
struct HeldBytes {
    buf: Vec<u8>,
    _held: sparkles::access::Reservation,
}

impl AsRef<[u8]> for HeldBytes {
    fn as_ref(&self) -> &[u8] {
        &self.buf
    }
}

/// Buffered JSON response body whose backing [`HeldBytes`] owner keeps the memory
/// reservation (and underlying pool grant) charged until every emitted frame has dropped.
struct HeldBody {
    data: axum::body::Bytes,
}

impl http_body::Body for HeldBody {
    type Data = axum::body::Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        if self.data.is_empty() {
            return std::task::Poll::Ready(None);
        }
        let n = self.data.len().min(RESPONSE_CHUNK_BYTES);
        let chunk = self.data.split_to(n);
        std::task::Poll::Ready(Some(Ok(http_body::Frame::data(chunk))))
    }

    fn is_end_stream(&self) -> bool {
        self.data.is_empty()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.data.len() as u64)
    }
}

struct CountingWriter(usize);

impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.checked_add(buf.len()).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "response size overflow")
        })?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct ExactVecWriter {
    buf: Vec<u8>,
}

impl ExactVecWriter {
    fn with_exact_capacity(cap: usize) -> Self {
        let mut buf = Vec::new();
        buf.reserve_exact(cap);
        Self { buf }
    }

    fn into_vec(self) -> Vec<u8> {
        self.buf
    }
}

impl std::io::Write for ExactVecWriter {
    fn write(&mut self, src: &[u8]) -> std::io::Result<usize> {
        if src.len() > self.buf.capacity() - self.buf.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "serialized JSON exceeded reserved response capacity",
            ));
        }
        self.buf.extend_from_slice(src);
        Ok(src.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct NamedNodesRef<'a>(&'a [oxrdf::NamedNode]);
impl serde::Serialize for NamedNodesRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for n in self.0 {
            seq.serialize_element(n.as_str())?;
        }
        seq.end()
    }
}

struct TripleDisplay<'a>(&'a oxrdf::Triple);
impl serde::Serialize for TripleDisplay<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self.0)
    }
}

#[derive(serde::Serialize)]
struct ProjectionChangeRef<'a> {
    output: &'a str,
    triple: TripleDisplay<'a>,
}

struct ProjectionChangesRef<'a>(&'a [sparkles::evidence::ProjectionChange]);
impl serde::Serialize for ProjectionChangesRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for c in self.0 {
            seq.serialize_element(&ProjectionChangeRef {
                output: c.output.as_str(),
                triple: TripleDisplay(&c.triple),
            })?;
        }
        seq.end()
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangedRef<'a> {
    supports_added: NamedNodesRef<'a>,
    supports_ended: NamedNodesRef<'a>,
    inserted: ProjectionChangesRef<'a>,
    removed: ProjectionChangesRef<'a>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DryRunRef {
    would_commit: bool,
    inserted: u64,
    deleted: u64,
    error: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ReconcileProjectionResponse<'a> {
    operation: &'a str,
    committed: bool,
    replayed: bool,
    commit: u64,
    branch_id: uuid::Uuid,
    policy: &'a str,
    policy_version: u32,
    output: &'a str,
    projection: &'a str,
    vouched_supports: NamedNodesRef<'a>,
    changed: ChangedRef<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dry_run: Option<DryRunRef>,
}

fn reconcile_projection_now(
    st: Arc<AppState>,
    name: String,
    p: Principal,
    r: ReconcileProjectionRequest,
    execution: super::Execution,
) -> ApiResult<(usize, HeldBody)> {
    let branch = execution.branch.clone();
    let main = crate::http::main_dataset(&st, &name)?;
    crate::settings::evidence::require_reconciled(&st, &main)?;
    let prepared = prepare(
        &st,
        &name,
        &p,
        &branch,
        &execution,
        r.idempotency_key,
        r.if_head,
        r.dry_run,
        r.message,
    )?;
    use crate::auth::{Endpoint, on_branch};
    let here = on_branch(&prepared.main.name, &branch);
    let read_opts = crate::http::caller::CallerContext::for_caller(&st, &prepared.logical, &p)
        .options_on(&crate::http::Params::empty(), &here, Endpoint::Query);
    let preparation = match execution.grant {
        Some(grant) => sparkles::access::Preparation::with_grant(
            prepared.control.clone(),
            sparkles::evidence::resolve::MEMORY_BYTES,
            grant,
        ),
        None => sparkles::access::Preparation::new(
            prepared.control.clone(),
            sparkles::evidence::resolve::MEMORY_BYTES,
        ),
    }?;
    let (reconciled, mut response_held) = sparkles::evidence::reconcile_projection_prepared(
        &prepared.main.dataset,
        &prepared.logical.dataset,
        &r.policy,
        r.policy_version,
        &prepared.options,
        read_opts.graphs.clone(),
        &preparation,
        &prepared.control,
    )?;
    prepared.fence.check_uncommitted(reconciled.committed)?;
    let dry_run = reconciled.preview.as_deref().map(|preview| {
        let commit = preview.receipt_commit();
        DryRunRef {
            would_commit: matches!(preview.outcome(), sparkles::preview::Outcome::Commit),
            inserted: commit.inserted,
            deleted: commit.deleted,
            error: preview.error(),
        }
    });
    if let Some(error) = dry_run.as_ref().and_then(|d| d.error.as_deref()) {
        response_held.grow_by((error.len() as u64).saturating_mul(6).saturating_add(64))?;
    }
    let response = ReconcileProjectionResponse {
        operation: reconciled.operation.as_str(),
        committed: reconciled.committed,
        replayed: reconciled.replayed,
        commit: reconciled.commit,
        branch_id: reconciled.branch,
        policy: reconciled.policy.as_str(),
        policy_version: reconciled.policy_version,
        output: reconciled.output.as_str(),
        projection: reconciled.projection.as_str(),
        vouched_supports: NamedNodesRef(&reconciled.vouched_supports),
        changed: ChangedRef {
            supports_added: NamedNodesRef(&reconciled.changed.supports_added),
            supports_ended: NamedNodesRef(&reconciled.changed.supports_ended),
            inserted: ProjectionChangesRef(&reconciled.changed.inserted),
            removed: ProjectionChangesRef(&reconciled.changed.removed),
        },
        dry_run,
    };
    let mut counter = CountingWriter(0);
    serde_json::to_writer(&mut counter, &response)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let exact_len = counter.0;
    let committed = reconciled.committed;
    if committed {
        debug_assert!(
            exact_len as u64 <= response_held.bytes(),
            "committed reconciliation response ({exact_len} B) exceeded preadmitted reservation ({} B)",
            response_held.bytes()
        );
    } else if exact_len as u64 > response_held.bytes() {
        response_held.grow_to(exact_len as u64)?;
    }
    let mut writer = ExactVecWriter::with_exact_capacity(exact_len);
    serde_json::to_writer(&mut writer, &response)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let body = writer.into_vec();
    let cap = body.capacity() as u64;
    drop(reconciled);
    // Committed responses are preadmitted before publication and must never fail with a
    // post-commit deadline timeout; shrink to the exact retained buffer capacity and attach
    // the reservation to the backing Bytes owner so emitted frames keep it charged.
    if !committed && cap > response_held.bytes() {
        response_held.grow_to(cap)?;
    }
    response_held.release_to(cap);
    drop(preparation);
    let len = body.len();
    Ok((
        len,
        HeldBody {
            data: axum::body::Bytes::from_owner(HeldBytes {
                buf: body,
                _held: response_held,
            }),
        },
    ))
}

type Policy = Option<Arc<sparkles::access::GraphAccess>>;

/// What the caller may read on a dataset and branch, asked once per pair.
#[derive(Default)]
struct ReadCache(std::collections::HashMap<(uuid::Uuid, String), Policy>);

impl ReadCache {
    fn get(&mut self, st: &AppState, p: &Principal, ds: &sparkles::Dataset, b: &str) -> Policy {
        self.0
            .entry((ds.dataset_id(), b.to_string()))
            .or_insert_with(|| policy(st, p, ds, b))
            .clone()
    }
}
