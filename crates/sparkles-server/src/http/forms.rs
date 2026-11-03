//! Editing forms generated from SHACL shapes over HTTP
//! ([U02](../../../../docs/specs/U02-shape-driven-forms.md)): the form model of a resource
//! `GET /$/forms/{ds}`, and the check and the save of a form's changes
//! `POST /$/forms/{ds}/check` and `POST /$/forms/{ds}/save`. The library builds the
//! model and the update (`sparkles::forms`); these handlers read the dataset's `forms`
//! settings, choose the branch a save goes to, and apply the caller's grants.

use super::*;
use crate::auth::{Endpoint, on_branch};
use sparkles::branch::{BranchOptions, MAIN};
use sparkles::forms::{
    FieldChange, FormChange, FormRequest, FormSaved, FormSettings, NodeChange, SaveTo, ShapesSource,
};
use sparkles::history::At;

/// The settings of the dataset (its main branch's), with `shapes=` overriding the source.
fn settings(st: &AppState, main: &Dataset, shapes: Option<&str>) -> ApiResult<FormSettings> {
    let mut s: FormSettings = crate::settings::effective(st, main, &crate::settings::FORMS);
    if let Some(v) = shapes {
        s.shapes = ShapesSource::parse(v).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                format!("shapes must be guard, graphs, drafted or skos, not '{v}'"),
            )
        })?;
        if s.shapes == ShapesSource::Graphs && s.graphs.is_empty() {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "shapes=graphs needs the forms setting graphs: the shapes graphs to read",
            ));
        }
    }
    Ok(s)
}

/// Today as `yyyymmdd`, for the session branch.
fn today() -> String {
    chrono::Utc::now().format("%Y%m%d").to_string()
}

/// The name the session branch of the caller takes.
fn user_name(p: &Principal) -> String {
    match author(p) {
        Some(a) => a.rsplit(':').next().unwrap_or(&a).to_string(),
        None if p.name.is_empty() => "anonymous".into(),
        None => p.name.to_string(),
    }
}

/// Where a save of a form opened on `branch` goes, and whether that branch exists yet.
fn save_target(
    main: &Dataset,
    s: &FormSettings,
    p: &Principal,
    branch: &str,
) -> ApiResult<(String, bool)> {
    if branch != MAIN {
        return Ok((branch.to_string(), true));
    }
    let session = s.session_branch(&user_name(p), &today());
    let to_branch = match s.save_to {
        SaveTo::Main => false,
        SaveTo::Branch => true,
        SaveTo::Auto => main.store.branch_protected(),
    };
    if !to_branch {
        return Ok((MAIN.to_string(), true));
    }
    let exists = main.store.branch_id_of(&session).is_ok();
    Ok((session, exists))
}

/// Whether the caller may save on `branch`: write through the update endpoint on an
/// existing branch, or the right to create a branch of that name.
fn may_save(st: &AppState, p: &Principal, ds: &str, branch: &str, exists: bool) -> bool {
    if st.read_only {
        return false;
    }
    let q = on_branch(ds, branch);
    let write = p
        .level_at(&q, Endpoint::Update)
        .is_some_and(|l| l >= Level::Write);
    if exists {
        write
    } else {
        write
            && p.level_at(&q, Endpoint::Branches)
                .is_some_and(|l| l >= Level::Write)
            && !p.restricted(&q)
            && !p.restricted(&on_branch(ds, MAIN))
    }
}

/// `GET /$/forms/{ds}`: the form model of `focus`.
pub(super) async fn model(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let main = ds.main().unwrap_or_else(|| ds.clone());
    let params = Params::from_query(&uri);
    let class = params
        .get("class")
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(|c| c.trim_start_matches('<').trim_end_matches('>').to_string());
    let focus = params
        .get("focus")
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(|f| f.trim_start_matches('<').trim_end_matches('>').to_string());
    if focus.is_some() == class.is_some() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "name either 'focus', the IRI of a resource, or 'class', the class of a new instance",
        ));
    }
    let s = settings(&st, &main, params.get("shapes"))?;
    let at = history::at_param(&params)?;
    let branch = ds.branch_name();
    let mut opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    let timeout = opts.timeout.unwrap_or(st.default_timeout);
    opts.deadline = Some(std::time::Instant::now() + timeout);
    let (cancel, _cancel_on_drop) = cancel_on_drop();
    opts.cancel = Some(cancel);
    let (target, exists) = save_target(&main, &s, &p, &branch)?;
    let writable = at.is_none() && may_save(&st, &p, &name, &target, exists);
    // what is writable follows the grants of the update endpoint on the branch saves go to
    let mut wq = QueryOptions::default();
    crate::auth::restrict(&mut wq, &p, &on_branch(&name, &target), Endpoint::Update);
    let lang = params.get("lang").map(str::to_string);
    let nested = match params.get("nested") {
        None | Some("false") | Some("0") => false,
        Some("true") | Some("1") | Some("") => true,
        Some(v) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!("nested must be true or false, not '{v}'"),
            ));
        }
    };
    if class.is_some() && at.is_some() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "a form for a new instance reads the head: leave out 'at'",
        ));
    }
    blocking(move || {
        // a new instance's form reads an IRI minted for it, which holds nothing yet
        let focus = focus.unwrap_or_else(|| s.mint_iri());
        let mut req = FormRequest::new(focus, s.clone());
        req.class = class;
        req.at = at;
        req.lang = lang;
        req.writable = writable;
        req.write_graphs = Some(wq.graphs);
        let forms = ds.dataset.forms();
        // the nested model refuses as a whole past its allowances: the flat form remains
        let (mut j, commit) = match nested.then(|| forms.nested_model(&req, &opts)) {
            Some(Ok(m)) => (serde_json::to_value(&m).unwrap_or(J::Null), m.root.commit),
            Some(Err(sparkles::Error::Invalid(reason))) => {
                let m = forms
                    .model(&req, &opts)
                    .map_err(|e| with_timeout(e.into(), Some(timeout)))?;
                let mut j = serde_json::to_value(&m).unwrap_or(J::Null);
                j["nested"] = json!({ "refused": reason });
                (j, m.commit)
            }
            Some(Err(e)) => return Err(with_timeout(e.into(), Some(timeout))),
            None => {
                let m = forms
                    .model(&req, &opts)
                    .map_err(|e| with_timeout(e.into(), Some(timeout)))?;
                (serde_json::to_value(&m).unwrap_or(J::Null), m.commit)
            }
        };
        j["save"] = json!({
            "to": if target == MAIN { "main" } else { "branch" },
            "branch": target,
            "exists": exists,
            "mainProtected": main.store.branch_protected(),
        });
        j["guard"] = guard_json(&main);
        Ok(with_commit(Json(j).into_response(), &ds, commit))
    })
    .await
}

/// The write guard's mode, which decides whether Save waits for a passing check.
fn guard_json(ds: &Dataset) -> J {
    match ds.validation.read().as_ref() {
        Some(g) => json!({
            "language": g.language().name(),
            "mode": g.json()["config"]["mode"],
        }),
        None => J::Null,
    }
}

/// A check or save request (spec U02 §3.2).
struct Body {
    focus: Option<String>,
    class: Option<String>,
    branch: Option<String>,
    shapes: Option<String>,
    changes: Vec<FieldChange>,
    expect: std::collections::BTreeMap<String, Vec<oxrdf::Term>>,
    message: Option<String>,
    nested: Vec<NodeChange>,
}

fn body(bytes: &[u8]) -> ApiResult<Body> {
    let bad = |m: String| err(StatusCode::BAD_REQUEST, m);
    let j: J = serde_json::from_slice(bytes).map_err(|e| bad(format!("invalid JSON: {e}")))?;
    let o = j
        .as_object()
        .ok_or_else(|| bad("the body is a JSON object".into()))?;
    if let Some(k) = o.keys().find(|k| {
        !matches!(
            k.as_str(),
            "focus" | "class" | "branch" | "shapes" | "changes" | "expect" | "message" | "nested"
        )
    }) {
        return Err(bad(format!(
            "unknown member '{k}': focus, class, branch, shapes, changes, expect, message and nested"
        )));
    }
    let string = |k: &str| -> ApiResult<Option<String>> {
        match o.get(k) {
            None | Some(J::Null) => Ok(None),
            Some(J::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(bad(format!("{k}: a string"))),
        }
    };
    let changes = sparkles::forms::field_changes_from_json(o.get("changes"), "changes")
        .map_err(|e| bad(e.to_string()))?;
    let expect = sparkles::forms::expect_from_json(o.get("expect"), "expect")
        .map_err(|e| bad(e.to_string()))?;
    let nested =
        sparkles::forms::nested_from_json(o.get("nested")).map_err(|e| bad(e.to_string()))?;
    let focus = string("focus")?.filter(|f| !f.is_empty());
    let class = string("class")?.filter(|c| !c.is_empty());
    if focus.is_some() == class.is_some() {
        return Err(bad(
            "name either focus, the IRI of the resource, or class, the class of a new instance"
                .into(),
        ));
    }
    Ok(Body {
        focus,
        class,
        branch: string("branch")?,
        shapes: string("shapes")?,
        changes,
        expect,
        message: string("message")?,
        nested,
    })
}

/// What a check or a save works on: the branch's dataset, the change, the options.
struct Prepared {
    ds: Arc<Dataset>,
    branch: String,
    created: bool,
    change: FormChange,
    opts: QueryOptions,
    restricted: bool,
    timeout: Option<Duration>,
    /// stops the write when the client goes away
    _cancel: CancelOnDrop,
}

/// Read the body, choose the branch, check the caller's grants on it and build the
/// options of the write. A save to a session branch that does not exist yet creates it
/// from `main`'s head when `create` is set.
async fn prepare(
    st: &Arc<AppState>,
    name: &str,
    p: &Principal,
    uri: &Uri,
    headers: &HeaderMap,
    bytes: &[u8],
    create: bool,
) -> ApiResult<Prepared> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let main = st
        .get(name)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("no such dataset: /{name}")))?;
    let b = body(bytes)?;
    let requested = branches::current();
    if let (Some(q), Some(bb)) = (&requested, &b.branch)
        && q != bb
    {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "invalid-branch",
            format!("the query names branch {q} and the body {bb}"),
        ));
    }
    let s = settings(st, &main, b.shapes.as_deref())?;
    let opened = b.branch.or(requested).unwrap_or_else(|| MAIN.to_string());
    let (branch, exists) = if opened == MAIN {
        save_target(&main, &s, p, MAIN)?
    } else {
        let exists = main.store.branch_id_of(&opened).is_ok();
        (opened, exists)
    };
    let mut created = false;
    if !exists {
        // only the caller's own session branch is created here
        let session = s.session_branch(&user_name(p), &today());
        if branch != session {
            return Err(err_code(
                StatusCode::NOT_FOUND,
                "no-such-branch",
                format!("no such branch: {branch}"),
            ));
        }
        if !may_save(st, p, name, &branch, false) {
            return Err(err(
                StatusCode::FORBIDDEN,
                format!("creating branch {branch} of /{name} for your edits is not allowed"),
            ));
        }
        if create {
            let m = main.clone();
            let n = branch.clone();
            blocking(move || {
                let o = BranchOptions {
                    from: MAIN.to_string(),
                    at: At::Head,
                    protected: false,
                    note: Some("Edits made through forms".into()),
                };
                match m.store.create_branch(&n, &o) {
                    Ok(_) => Ok(()),
                    // a save of another page created it meanwhile
                    Err(_) if m.store.branch_id_of(&n).is_ok() => Ok(()),
                    Err(e) => Err(e.into()),
                }
            })
            .await?;
            created = true;
        }
    } else {
        branches::check(p, name, &branch, Level::Write, Some(Endpoint::Update))?;
    }
    // a check of a branch not created yet runs on main's head, which the branch would
    // start from
    let ds = if branch == MAIN || (!exists && !create) {
        main.clone()
    } else {
        st.branch_dataset(&main, &branch)?
    };
    let params = Params::from_query(uri);
    let q = on_branch(name, &branch);
    let mut opts = CallerContext::for_caller(st, &ds, p).options_on(&params, &q, Endpoint::Update);
    let timeout = update_timeout(st, &params);
    opts.timeout = timeout;
    let mut w = validation::write_options(st, &params, headers, timeout)?;
    // a check is a dry run whatever the request says, a save never is
    w.dry_run = None;
    // a check of a session branch not created yet previews its first commit on main's
    // head, which main's protection must not refuse
    w.preview_as_new_branch = !exists && !create;
    w.graphs = opts.graphs.clone();
    w.author = author(p);
    let (cancel, cancel_guard) = cancel_on_drop();
    w.cancel = Some(cancel.clone());
    opts.cancel = Some(cancel);
    opts.write = w;
    let restricted = opts.graphs.is_some();
    Ok(Prepared {
        ds,
        branch,
        created,
        change: FormChange {
            // a new instance takes an IRI minted from the forms setting
            focus: b.focus.unwrap_or_else(|| s.mint_iri()),
            class: b.class,
            changes: b.changes,
            expect: b.expect,
            message: b.message,
            settings: s,
            nested: b.nested,
        },
        opts,
        restricted,
        timeout,
        _cancel: cancel_guard,
    })
}

/// `POST /$/forms/{ds}/check`: a dry run of the change and the validation of the nodes it
/// touches against the form's shapes.
pub(super) async fn check(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> ApiResult {
    let pr = prepare(&st, &name, &p, &uri, &headers, &bytes, false).await?;
    let req = dry_run::Request::new(
        &st,
        &headers,
        &sparkles::preview::DryRun {
            changes: 1000,
            all_changes: false,
            max_changes: 0,
            retain_states: false,
            state_max_memory: None,
        },
        pr.restricted,
    );
    let timeout = pr.timeout;
    blocking(move || {
        let c = pr
            .ds
            .dataset
            .forms()
            .check(&pr.change, &pr.opts)
            .map_err(|e| with_timeout(write_error(e, pr.restricted), timeout))?;
        let preview = req.json(&pr.ds, &c.preview);
        let j = json!({
            "branch": pr.branch,
            "preview": preview,
            "update": c.update,
            "conflicts": c.conflicts,
            "validation": {
                "checked": c.results.is_some(),
                "withheld": c.withheld,
                "timedOut": c.timed_out,
                "conforms": c.conforms,
                "results": c.results,
            },
        });
        Ok(with_commit(
            Json(j).into_response(),
            &pr.ds,
            c.preview.head.seq,
        ))
    })
    .await
}

/// `POST /$/forms/{ds}/save`: commit the change, or `409 form-conflict` with the fields
/// whose stored values changed since the form loaded them.
pub(super) async fn save(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> ApiResult {
    let pr = prepare(&st, &name, &p, &uri, &headers, &bytes, true).await?;
    let timeout = pr.timeout;
    blocking(move || {
        let saved = pr
            .ds
            .dataset
            .forms()
            .save(&pr.change, &pr.opts)
            .map_err(|e| with_timeout(write_error(e, pr.restricted), timeout))?;
        match saved {
            FormSaved::Conflict(fields) => Err(ApiError(
                StatusCode::CONFLICT,
                json!({
                    "error": "another commit changed these fields since the form loaded them",
                    "code": "form-conflict",
                    "branch": pr.branch,
                    "fields": fields,
                }),
            )),
            FormSaved::Saved(receipt) => {
                crate::otel::commit(&receipt);
                let mut j = serde_json::to_value(&*receipt).unwrap_or_else(|_| json!({}));
                if pr.restricted {
                    if let Some(m) = j.as_object_mut() {
                        m.remove("validation");
                    }
                    if let Some(c) = j.get_mut("commit") {
                        redact_commit_json(c);
                    }
                }
                j["dataset"] = json!(pr.ds.name);
                j["focus"] = json!(pr.change.focus);
                j["branch"] = json!(pr.branch);
                j["branchCreated"] = json!(pr.created);
                let validation = receipt.validation.clone().filter(|_| !pr.restricted);
                let resp = with_commit(Json(j).into_response(), &pr.ds, receipt.commit.seq);
                Ok(validation::with_validation(resp, validation.as_deref()))
            }
        }
    })
    .await
}
