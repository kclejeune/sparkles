//! Write previews (dry runs): `dryRun=true` or `Sparkles-Dry-Run: true` on an update, a
//! Graph Store write or an upload runs the write up to its commit and answers with what
//! the commit would be (see `docs/specs/C15-write-previews.md`). The write handlers catch
//! [`Error::DryRun`] before they map errors, and answer with [`Request::respond`].

use super::*;
use sparkles::preview::{DryRun, MAX_LISTED_CHANGES, Outcome, Preview};

/// Response header: the response describes a dry run, and nothing was written.
pub(super) const SPARKLES_DRY_RUN: &str = "sparkles-dry-run";
/// Response header: how the previewed write would end (`commit`, `rejected`, …).
pub(super) const SPARKLES_DRY_RUN_OUTCOME: &str = "sparkles-dry-run-outcome";

/// The body a dry run answers with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Format {
    Json,
    Patch(&'static str),
    PatchBinary,
}

/// What a dry-run request asked for, for its response.
#[derive(Clone, Copy, Debug)]
pub(super) struct Request {
    format: Format,
    /// the changed quads to list in JSON
    changes: usize,
    /// the caller's grants cover only some graphs
    pub(super) restricted: bool,
    /// `--max-export-mb`
    max_bytes: Option<u64>,
}

/// `true`, `false` or empty (true); anything else is refused, so that a misspelt flag
/// never turns a preview into a write.
fn flag(v: &str, what: &str) -> ApiResult<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "" | "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(err(
            StatusCode::BAD_REQUEST,
            format!("{what} must be true or false, not '{v}'"),
        )),
    }
}

/// The dry run a write request asks for: `None` for a real write.
pub(super) fn parse(
    st: &AppState,
    params: &Params,
    headers: &HeaderMap,
) -> ApiResult<Option<DryRun>> {
    let mut on = false;
    for v in params.all("dryRun") {
        on |= flag(&v, "dryRun")?;
    }
    if let Some(h) = headers.get(SPARKLES_DRY_RUN) {
        let v = h.to_str().map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                "Sparkles-Dry-Run must be true or false",
            )
        })?;
        on |= flag(v, "Sparkles-Dry-Run")?;
    }
    if !on {
        return Ok(None);
    }
    let changes = match params.get("changes") {
        Some(v) => v
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| *n <= MAX_LISTED_CHANGES)
            .ok_or_else(|| {
                err(
                    StatusCode::BAD_REQUEST,
                    format!("changes must be a number from 0 to {MAX_LISTED_CHANGES}"),
                )
            })?,
        None => 0,
    };
    let patch = format(headers) != Format::Json;
    if patch && params.get("changes").is_some() {
        // a patch without some of its changes would lead to a wrong state
        return Err(err(
            StatusCode::BAD_REQUEST,
            "changes does not apply to RDF Patch: a patch lists every change",
        ));
    }
    Ok(Some(DryRun {
        changes,
        all_changes: patch,
        max_changes: st.limits.max_rows as u64,
    }))
}

/// JSON, or an RDF Patch when `Accept` prefers one.
fn format(headers: &HeaderMap) -> Format {
    let offers = [
        "application/json",
        SolutionsFormat::Sparkles.media_type(),
        sparkles::patch::MEDIA_TYPE,
        "text/rdf-patch",
        sparkles::patch::MEDIA_TYPE_BINARY,
    ];
    match headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .and_then(|a| negotiate(a, &offers))
    {
        Some(2) => Format::Patch(sparkles::patch::MEDIA_TYPE),
        Some(3) => Format::Patch("text/rdf-patch"),
        Some(4) => Format::PatchBinary,
        _ => Format::Json,
    }
}

impl Request {
    pub(super) fn new(st: &AppState, headers: &HeaderMap, dr: &DryRun, restricted: bool) -> Self {
        Request {
            format: format(headers),
            changes: dr.changes,
            restricted,
            max_bytes: st.limits.max_export_bytes,
        }
    }

    /// The response to a dry run: the status the write would get, the preview as JSON
    /// (or as an RDF Patch of a write that would succeed), and the dry-run headers.
    pub(super) fn respond(&self, ds: &Dataset, p: &Preview) -> Response {
        let outcome = p.outcome();
        let status = match outcome {
            Outcome::Commit | Outcome::NoChange => StatusCode::OK,
            Outcome::PreconditionFailed => StatusCode::PRECONDITION_FAILED,
            Outcome::Rejected => StatusCode::UNPROCESSABLE_ENTITY,
            Outcome::StorageRefused => StatusCode::INSUFFICIENT_STORAGE,
        };
        let succeeds = matches!(outcome, Outcome::Commit | Outcome::NoChange);
        let resp = match self.format {
            Format::Patch(_) | Format::PatchBinary if succeeds => match self.patch(ds, p) {
                Ok(r) => r,
                Err(e) => return e.into_response(),
            },
            _ => (
                status,
                [(header::CONTENT_TYPE, "application/json")],
                self.json(ds, p).to_string(),
            )
                .into_response(),
        };
        let mut resp = with_commit(resp, ds, p.head.seq);
        let h = resp.headers_mut();
        h.insert(SPARKLES_DRY_RUN, header::HeaderValue::from_static("true"));
        h.insert(
            SPARKLES_DRY_RUN_OUTCOME,
            header::HeaderValue::from_static(outcome.name()),
        );
        let validation = p.validation.as_deref().filter(|_| !self.restricted);
        let report = RequestReport {
            rows: Some(p.graphs.iter().map(|g| g.inserted + g.deleted).sum()),
            ..Default::default()
        };
        report.attach(validation::with_validation(resp, validation))
    }

    /// The whole net change as an RDF Patch from the head to the state after the write.
    fn patch(&self, ds: &Dataset, p: &Preview) -> ApiResult {
        let id = ds.store.dataset_id();
        let to = p.commit.as_ref().unwrap_or(&p.head).seq;
        let binary = self.format == Format::PatchBinary;
        let mut pw = sparkles::patch::PatchWriter::new(Vec::new(), binary);
        let io = |e: std::io::Error| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
        pw.header("id", &sparkles::patch::commit_iri(id, to))
            .map_err(io)?;
        pw.header("prev", &sparkles::patch::commit_iri(id, p.head.seq))
            .map_err(io)?;
        pw.begin().map_err(io)?;
        for (op, q) in &p.changes {
            pw.change(*op, q).map_err(io)?;
        }
        pw.commit().map_err(io)?;
        let body = pw.into_inner();
        if let Some(max) = self.max_bytes
            && body.len() as u64 > max
        {
            return Err(Error::BudgetExceeded(sparkles::Budget {
                kind: BudgetKind::ResultBytes,
                limit: max,
                requested: body.len() as u64,
            })
            .into());
        }
        let ct = match self.format {
            Format::Patch(t) => format!("{t}; charset=utf-8"),
            _ => sparkles::patch::MEDIA_TYPE_BINARY.to_string(),
        };
        Ok(([(header::CONTENT_TYPE, ct)], body).into_response())
    }

    /// The preview as JSON (see the spec, §2.3).
    pub(super) fn json(&self, ds: &Dataset, p: &Preview) -> J {
        let outcome = p.outcome();
        let would = p.commit.is_some();
        let commit = match &p.commit {
            Some(c) => {
                let note = sparkles::annotations::Annotation {
                    message: p.message.clone(),
                    digest: None,
                };
                let mut j = json!(sparkles::commit::AnnotatedCommit {
                    commit: c,
                    annotation: Some(&note),
                });
                // only known when the commit is made
                if let Some(m) = j.as_object_mut() {
                    m.remove("timestamp");
                }
                j
            }
            None => {
                let note = ds.store.annotation(p.head.seq);
                json!(sparkles::commit::AnnotatedCommit {
                    commit: &p.head,
                    annotation: note.as_ref(),
                })
            }
        };
        let mut commit = commit;
        if self.restricted {
            redact_commit_json(&mut commit);
        }
        let graphs: Vec<J> = p
            .graphs
            .iter()
            .map(|g| {
                let name = match &g.graph {
                    oxrdf::GraphName::DefaultGraph => J::Null,
                    oxrdf::GraphName::NamedNode(n) => json!(n.as_str()),
                    oxrdf::GraphName::BlankNode(b) => json!(b.to_string()),
                };
                json!({ "graph": name, "inserted": g.inserted, "deleted": g.deleted })
            })
            .collect();
        let mut doc = json!({
            "dryRun": true,
            "dataset": ds.name,
            "datasetId": ds.store.owner_dataset_id(),
            "committed": false,
            "wouldCommit": would,
            "outcome": outcome.name(),
            "head": p.head.seq,
            "commit": commit,
            "graphs": graphs,
        });
        if let Some(total) = p.changes_total.filter(|_| self.changes > 0) {
            let quads: Vec<J> = p
                .changes
                .iter()
                .take(self.changes)
                .map(|(op, q)| diff::quad_json(*op, q))
                .collect();
            doc["changes"] = json!({
                "total": total,
                "limit": self.changes,
                "truncated": (quads.len() as u64) < total,
                "quads": quads,
            });
        }
        if let Some(v) = p.validation.as_deref().filter(|_| !self.restricted) {
            let mut v = serde_json::to_value(v).unwrap_or(J::Null);
            if p.rejected() {
                v["head"] = json!(p.head.seq);
                v["kind"] = json!(p.kind.name());
            }
            doc["validation"] = v;
        }
        if let Some(pre) = &p.precondition {
            doc["precondition"] = match pre {
                Ok(()) => json!({ "status": "passed" }),
                Err(m) => json!({ "status": "failed", "error": m }),
            };
        }
        let s = &p.storage;
        let mut storage = json!({
            "status": if s.refused.is_some() { "refused" } else { "fits" },
        });
        if !self.restricted {
            for (k, v) in [
                ("limit", s.limit),
                ("used", s.used),
                ("projected", s.projected),
            ] {
                if let Some(v) = v {
                    storage[k] = json!(v);
                }
            }
        }
        match &s.refused {
            Some(Error::BudgetExceeded(b)) => {
                storage["budget"] = json!(b.kind);
                storage["error"] = json!(s.refused.as_ref().unwrap().to_string());
            }
            Some(e) => {
                storage["code"] = json!("storage-full");
                storage["error"] = json!(e.to_string());
            }
            None => {}
        }
        doc["storage"] = storage;
        match outcome {
            Outcome::PreconditionFailed => {
                doc["code"] = json!("precondition-failed");
            }
            Outcome::StorageRefused => {
                if let Some(Error::BudgetExceeded(b)) = &s.refused {
                    doc["budget"] = json!(b.kind);
                    if !self.restricted {
                        doc["limit"] = json!(b.limit);
                        doc["requested"] = json!(b.requested);
                    }
                } else {
                    doc["code"] = json!("storage-full");
                }
            }
            Outcome::Rejected | Outcome::Commit | Outcome::NoChange => {}
        }
        let error = match outcome {
            // a limited caller learns that the guard refused, not what it found
            Outcome::Rejected if self.restricted => Some(
                "the write does not conform to the dataset's validation guard; it would not be committed"
                    .to_string(),
            ),
            _ => p.error(),
        };
        if let Some(e) = error {
            doc["error"] = json!(e);
        }
        doc
    }
}
