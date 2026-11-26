//! `community_reports` (spec C22 §2.5): the GraphRAG reports that match a text, by
//! full-text and vector search over their titles, summaries and findings, with their
//! members' counts and findings, so an external agent can run its own global answering.
//! Global answering in Ask selects its reports through the same function.
//!
//! The reports are read through the caller's view, which the disclosure guard of C22 §6
//! restricts, so a caller the guard does not prove for the reports learns only that
//! they are unavailable to it.

use super::Reader;
use super::link::unavailable;
use super::recall::{ResolutionDoc, ResolvedDoc, policy_error, rank_terms};
use super::text::plain_query;
use crate::graphrag::read::{self, Filter, ReportRef};
use crate::mcp::Outcome;
use crate::mcp::errors::{ErrorContext, ToolError};
use crate::mcp::tools::{Tools, bounded, parse};
use oxrdf::{Literal, NamedNode, Term};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};

/// The most reports one call returns.
pub(crate) const MAX_LIMIT: u64 = 100;
/// The most distinct report entities one call resolves through a matching policy.
const MAX_REPORT_ENTITIES: usize = 10_000;
/// The most member IRI entries serialized across `resolution.entities` and all
/// per-report `identities` arrays in one `community_reports` response.
const MAX_IDENTITY_MEMBER_ENTRIES: usize = 8_192;
/// The upper bound on aggregate identity bytes across `resolution.entities` and
/// per-report `identities` arrays in one `community_reports` response.
const MAX_IDENTITY_BYTES: usize = 2 << 20;
/// Approximate JSON framing bytes per serialized identity object.
const IDENTITY_ENTRY_OVERHEAD: usize = 80;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Args {
    dataset: Option<String>,
    query: Option<String>,
    level: Option<u64>,
    #[serde(default)]
    reviewed_only: bool,
    limit: Option<u64>,
    findings: Option<bool>,
    /// select only reports read under this review policy (C25, item 811)
    review_policy: Option<String>,
    /// resolve entity identities referenced by the reports under this matching policy (C25, item 811)
    matching_policy: Option<String>,
    timeout_seconds: Option<f64>,
}

/// What a selection of reports found.
pub(crate) enum Selection {
    /// the reports cannot be used, and why: `off`, `unavailable` or `no-reports`
    Unavailable(&'static str, String),
    Reports {
        graph: String,
        level: u32,
        levels: Vec<(u32, u64)>,
        /// `search` or `rating`
        ranking: &'static str,
        reports: Vec<ReportRef>,
    },
}

fn identity_json(entity: &str, d: &ResolvedDoc, member_cap: usize) -> Value {
    let shown = d.members.len().min(member_cap);
    json!({
        "entity": entity,
        "representative": d.representative.as_str(),
        "members": d.members[..shown].iter().map(NamedNode::as_str).collect::<Vec<_>>(),
        "membersComplete": d.complete && shown == d.members.len(),
    })
}

/// Choose the largest uniform per-entity member cap so that the aggregate identity
/// payload across `resolution.entities` and every report's `identities` stays within
/// `max_members` member entries and `max_bytes` bytes. Refuses with `422
/// incomplete-evidence` if even the bare `entity -> representative` entries exceed
/// `max_bytes`.
fn budget_identities(
    doc: &ResolutionDoc,
    report_entities: &[Vec<String>],
    max_members: usize,
    max_bytes: usize,
) -> Result<usize, ToolError> {
    let mut copies: BTreeMap<&str, usize> = BTreeMap::new();
    for ents in report_entities {
        for e in ents {
            if doc.entities.get(e.as_str()).is_some_and(|d| d.named) {
                *copies.entry(e.as_str()).or_insert(1) += 1;
            }
        }
    }
    if copies.is_empty() {
        return Ok(usize::MAX);
    }
    let mut base_bytes = 0usize;
    let mut max_len = 0usize;
    let mut entries: Vec<(usize, &ResolvedDoc)> = Vec::with_capacity(copies.len());
    for (iri, count) in copies {
        let d = &doc.entities[iri];
        let per_copy = iri
            .len()
            .saturating_add(d.representative.as_str().len())
            .saturating_add(IDENTITY_ENTRY_OVERHEAD);
        base_bytes = base_bytes.saturating_add(count.saturating_mul(per_copy));
        max_len = max_len.max(d.members.len());
        entries.push((count, d));
    }
    if base_bytes > max_bytes {
        return Err(ToolError::new(
            "incomplete-evidence",
            422,
            format!(
                "the resolved identities for the selected reports exceed the {} byte response budget",
                max_bytes
            ),
        ));
    }
    let fits = |k: usize| -> bool {
        let mut total_members = 0usize;
        let mut total_bytes = base_bytes;
        for &(count, d) in &entries {
            let take = d.members.len().min(k);
            total_members = total_members.saturating_add(count.saturating_mul(take));
            if total_members > max_members {
                return false;
            }
            let mbytes: usize = d.members[..take]
                .iter()
                .map(|m| m.as_str().len().saturating_add(3))
                .sum();
            total_bytes = total_bytes.saturating_add(count.saturating_mul(mbytes));
            if total_bytes > max_bytes {
                return false;
            }
        }
        true
    };
    if fits(max_len) {
        return Ok(max_len);
    }
    let (mut lo, mut hi) = (0usize, max_len);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Ok(lo)
}

fn resolution_json(doc: &ResolutionDoc, entities: &[Value]) -> Value {
    json!({
        "policy": doc.name,
        "version": doc.version,
        "revision": doc.revision.as_ref().map(NamedNode::as_str),
        "number": doc.number,
        "entities": entities,
    })
}

/// The JSON form of a report. When `resolution` is `Some`, each finding includes its
/// `about` entity IRIs and the report includes the mapped `identities` its findings
/// reference, each capped to `member_cap` cluster members.
pub(crate) fn report_json(
    r: &ReportRef,
    resolution: Option<(&ResolutionDoc, &[String], usize)>,
) -> Value {
    let mut v = json!({
        "iri": r.iri,
        "community": r.community,
        "level": r.level,
        "title": r.title,
        "summary": r.summary,
        "status": r.status.as_str(),
        "rating": r.rating,
        "memberCount": r.member_count,
        "findings": r.findings.iter().map(|f| {
            let mut x = json!({
                "iri": f.iri,
                "text": f.text,
                "cites": f.cites,
            });
            // a report read under a review policy names the claims and decisions each
            // finding cites (spec C25, "Reports on accepted evidence")
            if r.review_policy.is_some() {
                x["claims"] = json!(f.claims);
                x["decisions"] = json!(f.decisions);
            }
            if resolution.is_some() {
                x["about"] = json!(f.about);
            }
            x
        }).collect::<Vec<_>>(),
    });
    if let Some(s) = r.score {
        v["score"] = json!(s);
    }
    if let Some(p) = &r.review_policy {
        v["reviewPolicy"] = json!({
            "iri": p.iri,
            "version": p.version,
            "configuration": p.configuration,
        });
    }
    if let Some((doc, entities, member_cap)) = resolution {
        let ids: Vec<Value> = entities
            .iter()
            .filter_map(|e| {
                doc.entities
                    .get(e.as_str())
                    .filter(|d| d.named)
                    .map(|d| identity_json(e.as_str(), d, member_cap))
            })
            .collect();
        v["identities"] = Value::Array(ids);
    }
    v
}

impl Tools<'_> {
    pub(crate) fn community_reports(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: Args = parse(args)?;
        let query = a.query.as_deref().map(str::trim).filter(|q| !q.is_empty());
        if a.query.as_ref().is_some_and(|q| q.chars().count() > 2000) {
            return Err(ToolError::bad_argument(
                "query must be at most 2000 characters",
            ));
        }
        let limit = bounded("limit", a.limit, 10, 1, MAX_LIMIT)? as usize;
        let level = match a.level {
            None => None,
            Some(l) if l <= 64 => Some(l as u32),
            Some(_) => return Err(ToolError::bad_argument("level must be between 0 and 64")),
        };
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let ctx = self.ctx(&[], timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let policies = a.review_policy.is_some() || a.matching_policy.is_some();
        let view = if policies {
            Some(self.evidence_view(&ds, None, None, deadline, &ctx)?)
        } else {
            None
        };
        let r = match &view {
            Some(v) => self.reader_on_evidence(&ds, v.clone(), Some(false), deadline, &ctx)?,
            None => self.reader(&ds, None, None, Some(false), deadline, &ctx)?,
        };
        let control = sparkles::task::Control {
            deadline: Some(deadline),
            ..sparkles::task::Control::with_cancel(self.call.cancel.clone())
        };
        let review_policy = match (&view, &a.review_policy) {
            (Some(v), Some(name)) => Some(
                sparkles::evidence::with_policy_claims(
                    v,
                    name,
                    r.opts.graphs.clone(),
                    &control,
                    |pc| Ok(pc.policy()),
                )
                .map_err(|e| policy_error("reviewPolicy", e, &ctx))?,
            ),
            _ => None,
        };
        let mut resolution = match (&view, &a.matching_policy) {
            (Some(v), Some(name)) => {
                let version =
                    sparkles::evidence::check_matching_policy(v, name, r.opts.graphs.as_ref())
                        .map_err(|e| policy_error("matchingPolicy", e, &ctx))?;
                let mut doc = ResolutionDoc::new(name, version);
                doc.read(v, &[], &r, &control)
                    .map_err(|e| policy_error("matchingPolicy", e, &ctx))?;
                Some(doc)
            }
            _ => None,
        };
        let filter = Filter {
            reviewed_only: a.reviewed_only,
            review_policy_iri: review_policy.as_ref().map(|p| p.iri.as_str()),
        };
        let sel = self.select_reports(
            &ds,
            &r,
            query,
            level,
            filter,
            limit,
            a.findings.unwrap_or(true),
            review_policy.as_ref().map(|p| p.name.as_str()),
            &ctx,
        )?;
        let mut out = match sel {
            Selection::Unavailable(code, message) => json!({
                "dataset": ds.name,
                "available": false,
                "reason": code,
                "message": message,
                "reports": [],
            }),
            Selection::Reports {
                graph,
                level,
                levels,
                ranking,
                reports,
            } => {
                let mut resolved_entities: Vec<Value> = Vec::new();
                let mut report_entities: Vec<Vec<String>> = Vec::new();
                let mut member_cap = usize::MAX;
                if let (Some(v), Some(doc)) = (&view, resolution.as_mut()) {
                    let mut seen: BTreeSet<String> = BTreeSet::new();
                    let mut wanted: Vec<NamedNode> = Vec::new();
                    report_entities.reserve(reports.len());
                    for rep in &reports {
                        let mut rep_seen: BTreeSet<String> = BTreeSet::new();
                        for f in &rep.findings {
                            for n in read::finding_entity_iris(&f.about, &f.cites) {
                                rep_seen.insert(n.as_str().to_string());
                                if seen.insert(n.as_str().to_string()) {
                                    if wanted.len() >= MAX_REPORT_ENTITIES {
                                        return Err(ToolError::new(
                                            "incomplete-evidence",
                                            422,
                                            format!(
                                                "the selected reports reference more than {MAX_REPORT_ENTITIES} distinct entities to resolve"
                                            ),
                                        ));
                                    }
                                    wanted.push(n);
                                }
                            }
                        }
                        report_entities.push(rep_seen.into_iter().collect());
                    }
                    doc.read(v, &wanted, &r, &control)
                        .map_err(|e| policy_error("matchingPolicy", e, &ctx))?;
                    let max_bytes = self.cfg().max_bytes.min(MAX_IDENTITY_BYTES);
                    member_cap = budget_identities(
                        doc,
                        &report_entities,
                        MAX_IDENTITY_MEMBER_ENTRIES,
                        max_bytes,
                    )?;
                    for iri in &seen {
                        if let Some(d) = doc.entities.get(iri.as_str()).filter(|d| d.named) {
                            resolved_entities.push(identity_json(iri, d, member_cap));
                        }
                    }
                }
                let mut j = json!({
                    "dataset": ds.name,
                    "available": true,
                    "reportsGraph": graph,
                    "level": level,
                    "levels": levels.iter().map(|(l, n)| json!({ "level": l, "reports": n })).collect::<Vec<_>>(),
                    "ranking": ranking,
                    "stale": reports.iter().filter(|r| r.status == crate::graphrag::store::Status::Stale).count(),
                    "reports": reports
                        .iter()
                        .enumerate()
                        .map(|(i, rep)| {
                            let res = resolution
                                .as_ref()
                                .map(|doc| (doc, report_entities[i].as_slice(), member_cap));
                            report_json(rep, res)
                        })
                        .collect::<Vec<_>>(),
                });
                if let Some(doc) = &resolution {
                    j["resolution"] = resolution_json(doc, &resolved_entities);
                }
                j
            }
        };
        if let Some(p) = &review_policy {
            out["reviewPolicy"] = json!({
                "name": p.name,
                "iri": p.iri.as_str(),
                "version": p.version,
            });
        }
        if out.get("resolution").is_none()
            && let Some(doc) = &resolution
        {
            out["resolution"] = resolution_json(doc, &[]);
        }
        Ok(Outcome::Structured(out))
    }

    /// The usable reports at `level`, or at the level §7.2 chooses, ranked by a search
    /// for `query` when the dataset has a full-text or embedding index and by rating
    /// otherwise.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn select_reports(
        &self,
        ds: &crate::state::Dataset,
        r: &Reader,
        query: Option<&str>,
        level: Option<u32>,
        filter: Filter<'_>,
        limit: usize,
        findings: bool,
        review_policy_name: Option<&str>,
        ctx: &ErrorContext,
    ) -> Result<Selection, ToolError> {
        let eng = |e| ctx.engine(e);
        let settings: crate::graphrag::settings::GraphragSettings =
            crate::settings::effective(&self.server.state, ds, &crate::settings::GRAPHRAG);
        if !settings.enabled {
            return Ok(Selection::Unavailable(
                "off",
                "GraphRAG is not enabled for this dataset".into(),
            ));
        }
        let graph = settings.reports_graph;
        let opts = r.timed().map_err(eng)?;
        if !r.readable_graph(&opts, &graph).map_err(eng)? {
            return Ok(Selection::Unavailable(
                "unavailable",
                "the community reports are not available to you, because your access does not cover everything their build read".into(),
            ));
        }
        let counts = r.level_counts(&opts, &graph, filter).map_err(eng)?;
        let Some(level) = level.or_else(|| read::choose_level(&counts, read::DEFAULT_MAX_REPORTS))
        else {
            let msg = match (filter.reviewed_only, review_policy_name) {
                (true, Some(p)) => {
                    format!("no reviewed community report exists under reviewPolicy \"{p}\"")
                }
                (false, Some(p)) => {
                    format!("no community report exists under reviewPolicy \"{p}\"")
                }
                (true, None) => "no reviewed community report exists".into(),
                (false, None) => "no community report exists".into(),
            };
            return Ok(Selection::Unavailable("no-reports", msg));
        };
        let levels: Vec<(u32, u64)> = counts.iter().map(|(l, n)| (*l, *n)).collect();
        let mut ranking = "rating";
        let mut reports = Vec::new();
        if let Some(q) = query {
            let hits = self
                .report_hits(ds, r, q, &graph, level, filter, limit)
                .map_err(eng)?;
            if !hits.is_empty() {
                let names: Vec<String> = hits.iter().map(|(r, _)| r.clone()).collect();
                let found = r
                    .reports_named_at(&opts, &graph, &names, level, filter)
                    .map_err(eng)?;
                let by: HashMap<&str, &ReportRef> =
                    found.iter().map(|x| (x.iri.as_str(), x)).collect();
                for (iri, score) in &hits {
                    if let Some(rep) = by.get(iri.as_str())
                        && rep.level == level
                    {
                        let mut rep = (*rep).clone();
                        rep.score = Some(*score);
                        reports.push(rep);
                    }
                }
                if !reports.is_empty() {
                    ranking = "search";
                }
            }
        }
        // reports the search did not find follow, best rated first
        if reports.len() < limit {
            let rest = r
                .reports_at(&opts, &graph, Some(level), filter, limit)
                .map_err(eng)?;
            for rep in rest {
                if reports.len() >= limit {
                    break;
                }
                if !reports.iter().any(|x: &ReportRef| x.iri == rep.iri) {
                    reports.push(rep);
                }
            }
        }
        reports.truncate(limit);
        if findings {
            r.with_findings(&opts, &graph, &mut reports).map_err(eng)?;
        }
        Ok(Selection::Reports {
            graph,
            level,
            levels,
            ranking,
            reports,
        })
    }

    /// The eligible reports at `level` matching `filter` that a search for `q` finds in
    /// `graph`, best first, with their fused scores. A hit on a finding counts for its
    /// report, and eligibility is applied before the search limit and rank fusion so
    /// excluded hits consume no slots and cannot change eligible ranks or scores.
    #[allow(clippy::too_many_arguments)]
    fn report_hits(
        &self,
        ds: &crate::state::Dataset,
        r: &Reader,
        q: &str,
        graph: &str,
        level: u32,
        filter: Filter<'_>,
        limit: usize,
    ) -> Result<Vec<(String, f64)>, sparkles::error::Error> {
        let depth = limit.max(super::link::LIST_DEPTH);
        let policy_clause = filter.policy_clause()?;
        let values = filter.values();
        let report_pat = format!(
            "{values} ?c a <{community}> ; <{lv}> ?l ; <{report}> ?r . FILTER(?l = {level}) ?r <{status}> ?status{policy_clause} .",
            community = crate::graphrag::guard::gr("Community"),
            lv = crate::graphrag::guard::gr("level"),
            report = crate::graphrag::guard::gr("report"),
            status = crate::graphrag::guard::gr("status"),
        );
        let eligible_subjects = format!(
            "{{ {report_pat} BIND(?r AS ?s) }} UNION {{ {report_pat} ?r <{finding}> ?s . }}",
            finding = crate::graphrag::guard::gr("finding"),
        );
        let run_ranked =
            |search_call: &str| -> Result<Vec<(NamedNode, f64)>, sparkles::error::Error> {
                let sparql = format!(
                    "SELECT ?r (MAX(?score) AS ?best) WHERE {{ GRAPH <{graph}> {{ {eligible_subjects} {search_call} . }} }} GROUP BY ?r ORDER BY DESC(?best) ?r LIMIT {depth}"
                );
                Ok(r.rows(&sparql, Vec::new())?
                    .into_iter()
                    .filter_map(|row| match row.as_slice() {
                        [Some(Term::NamedNode(rep)), Some(Term::Literal(sc))] => {
                            let score: f64 = sc.value().parse().ok()?;
                            score.is_finite().then_some((rep.clone(), score))
                        }
                        _ => None,
                    })
                    .collect())
            };
        let mut lists: Vec<HashMap<String, usize>> = Vec::new();
        if let Some(hits) = self.eligible_text_hits(ds, r, q, graph, level, filter, depth)?
            && !hits.is_empty()
        {
            lists.push(rank_terms(hits.into_iter()));
        }
        // every index that may embed report text, each its own ranked list, restricted
        // to the eligible report and finding subjects via candidates:join
        let vec_k = depth.saturating_mul(4);
        for pred in report_embeddings(ds) {
            let call = format!(
                "(?s ?score) <{}> ({pred} {} {vec_k} \"metric:cosine\" \"candidates:join\")",
                sparkles::vector::VECTOR_SEARCH,
                Literal::new_simple_literal(q)
            );
            match run_ranked(&call) {
                Ok(h) if !h.is_empty() => lists.push(rank_terms(h.into_iter())),
                Ok(_) => {}
                Err(e) if unavailable(&e) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(sparkles::rank::fuse_rrf(
            lists.into_iter().map(|l| (1.0, l)),
            super::RRF_K,
        ))
    }

    /// Eligible-only streaming BM25 text ranking over the report and finding literals
    /// matching `level` and `filter` in `graph`, gated on the dataset's full-text index
    /// being enabled, ready, and covering `graph`.
    ///
    /// Uses the index's configured `maxTextBytes` prefix and default accent-folding
    /// analyzer (`StandardAnalyzer`), selects the narrowest single-predicate anchor scan
    /// (`gr:level`, `gr:reviewPolicy`, or admitted `gr:status`) in lock-step under
    /// `RejectMaterialization` (zero `DISTINCT` hash set and `O(1)` scan state), verifies
    /// eligibility in bounded batches via grounded per-subject index lookups, and streams
    /// literals in two passes (corpus stats, then per-report best score into a bounded
    /// top-`depth` heap).
    #[allow(clippy::too_many_arguments)]
    fn eligible_text_hits(
        &self,
        ds: &crate::state::Dataset,
        r: &Reader,
        q: &str,
        graph: &str,
        level: u32,
        filter: Filter<'_>,
        depth: usize,
    ) -> Result<Option<Vec<(NamedNode, f64)>>, sparkles::error::Error> {
        if !ds.dataset.indexes().text().enabled() {
            return Ok(None);
        }
        let Some(status) = ds.dataset.indexes().text().status() else {
            return Ok(None);
        };
        if !status.enabled
            || status.state != "ready"
            || !status.config.graphs.include.contains(graph)
            || status.config.graphs.exclude.iter().any(|g| g == graph)
        {
            return Ok(None);
        }
        if depth == 0 {
            return Ok(Some(Vec::new()));
        }
        let Some(plain) = plain_query(q) else {
            return Ok(None);
        };
        let mut analyzer = sparkles::text::StandardAnalyzer::new();
        let mut q_tokens: Vec<String> = Vec::new();
        analyzer.for_each_token(&plain, |tok| q_tokens.push(tok.to_string()));
        q_tokens.sort();
        q_tokens.dedup();
        if q_tokens.is_empty() {
            return Ok(None);
        }
        let preds: Vec<NamedNode> = match &status.config.predicates {
            sparkles::text::PredicateSet::Only(p) => p
                .iter()
                .filter_map(|p| NamedNode::new(p.as_str()).ok())
                .collect(),
            sparkles::text::PredicateSet::All => vec![
                NamedNode::new_unchecked(crate::graphrag::store::RDFS_LABEL),
                NamedNode::new_unchecked(crate::graphrag::guard::gr("summary")),
                NamedNode::new_unchecked(crate::graphrag::guard::gr("text")),
            ],
        };
        if preds.is_empty() {
            return Ok(None);
        }
        let max_text_bytes = status.config.max_text_bytes;
        let finding = crate::graphrag::guard::gr("finding");
        let candidate_anchors = candidate_anchor_plans(graph, level, filter)?;
        let report_cursor_opts = report_cursor_opts();
        let Some(anchor) = select_narrowest_anchor(r, candidate_anchors, &report_cursor_opts)?
        else {
            return Ok(Some(Vec::new()));
        };
        let q_index: HashMap<&str, usize> = q_tokens
            .iter()
            .enumerate()
            .map(|(i, t)| (t.as_str(), i))
            .collect();

        const FINDING_BATCH: usize = 32;
        const LITERAL_BATCH_ROWS: usize = 64;
        const LITERAL_BRANCH_BATCH: usize = 32;
        const K1: f64 = 1.2;
        const B: f64 = 0.75;

        let lit_cursor_opts = sparkles::sparql::CursorOptions {
            batch_rows: LITERAL_BATCH_ROWS,
            batch_bytes: 256 << 10,
            fallback: sparkles::sparql::FallbackPolicy::AllowMaterialization,
        };

        let lit_filter = "FILTER(isLiteral(?lit) && (DATATYPE(?lit) = <http://www.w3.org/2001/XMLSchema#string> || DATATYPE(?lit) = <http://www.w3.org/1999/02/22-rdf-syntax-ns#langString>))";

        type SubjectLitFn<'a> = &'a mut dyn FnMut(&str, &str) -> Result<(), sparkles::error::Error>;
        type BatchLitFn<'a> = &'a mut dyn FnMut(usize, &str) -> Result<(), sparkles::error::Error>;
        type ReportBatchFn<'a> =
            &'a mut dyn FnMut(&[NamedNode]) -> Result<(), sparkles::error::Error>;

        // Pin each `(subject, predicate)` pair directly in a bounded balanced UNION
        // (depth <= 5) so every read is an exact GSPO(graph, subject, pred) prefix
        // lookup and never scans unrelated outgoing predicates (`gr:finding`,
        // `gr:citesReport`, `gr:cites`, `gr:about`).
        let stream_subject_literals = |subjects: &[NamedNode],
                                       on_lit: SubjectLitFn<'_>|
         -> Result<(), sparkles::error::Error> {
            if subjects.is_empty() {
                return Ok(());
            }
            let mut branches: Vec<String> = Vec::with_capacity(LITERAL_BRANCH_BATCH);
            let mut flush_branches =
                |chunk: &mut Vec<String>| -> Result<(), sparkles::error::Error> {
                    if chunk.is_empty() {
                        return Ok(());
                    }
                    let sparql = format!(
                        "SELECT ?s ?lit WHERE {{ GRAPH <{graph}> {{ {{ {} }} {lit_filter} }} }}",
                        balanced_union(chunk),
                    );
                    let mut cursor = r.select_cursor(&sparql, &lit_cursor_opts)?;
                    while let Some(batch) = cursor.next_batch()? {
                        for row in 0..batch.len() {
                            let Some(owned_lit) = batch.term(row, 1)? else {
                                continue;
                            };
                            let Term::Literal(lit) = owned_lit.as_term() else {
                                continue;
                            };
                            let Some(owned_subj) = batch.term(row, 0)? else {
                                continue;
                            };
                            let Term::NamedNode(subj) = owned_subj.as_term() else {
                                continue;
                            };
                            on_lit(subj.as_str(), lit.value())?;
                        }
                    }
                    chunk.clear();
                    Ok(())
                };
            for s in subjects {
                for p in &preds {
                    branches.push(format!("{s} {p} ?lit . BIND({s} AS ?s)"));
                    if branches.len() >= LITERAL_BRANCH_BATCH {
                        flush_branches(&mut branches)?;
                    }
                }
            }
            flush_branches(&mut branches)
        };

        let for_each_batch_literals =
            |reps: &[NamedNode], on_lit: BatchLitFn<'_>| -> Result<(), sparkles::error::Error> {
                let rep_pos: HashMap<&str, usize> = reps
                    .iter()
                    .enumerate()
                    .map(|(i, rep)| (rep.as_str(), i))
                    .collect();

                // 1) Report literals (title, summary, etc.)
                stream_subject_literals(reps, &mut |subj, lit| {
                    if let Some(&idx) = rep_pos.get(subj) {
                        on_lit(idx, lit)?;
                    }
                    Ok(())
                })?;

                // 2) Finding literals, fetched in bounded finding chunks
                let f_branches: Vec<String> = reps
                    .iter()
                    .map(|rep| format!("{rep} <{finding}> ?f . BIND({rep} AS ?r)"))
                    .collect();
                let f_sparql = format!(
                    "SELECT ?r ?f WHERE {{ GRAPH <{graph}> {{ {} }} }}",
                    balanced_union(&f_branches),
                );
                let mut f_cursor = r.select_cursor(&f_sparql, &lit_cursor_opts)?;
                let mut f_chunk: Vec<(usize, NamedNode)> = Vec::with_capacity(FINDING_BATCH);
                let flush_findings = |chunk: &mut Vec<(usize, NamedNode)>,
                                      on_lit: BatchLitFn<'_>|
                 -> Result<(), sparkles::error::Error> {
                    if chunk.is_empty() {
                        return Ok(());
                    }
                    let mut uniq: Vec<NamedNode> = Vec::with_capacity(chunk.len());
                    let mut by_finding: HashMap<&str, Vec<usize>> = HashMap::new();
                    for (idx, f) in chunk.iter() {
                        let entry = by_finding.entry(f.as_str()).or_default();
                        if entry.is_empty() {
                            uniq.push(f.clone());
                        }
                        entry.push(*idx);
                    }
                    stream_subject_literals(&uniq, &mut |subj, lit| {
                        if let Some(indices) = by_finding.get(subj) {
                            for &idx in indices {
                                on_lit(idx, lit)?;
                            }
                        }
                        Ok(())
                    })?;
                    chunk.clear();
                    Ok(())
                };
                while let Some(batch) = f_cursor.next_batch()? {
                    for row in 0..batch.len() {
                        let Some(owned_r) = batch.term(row, 0)? else {
                            continue;
                        };
                        let Term::NamedNode(rep) = owned_r.as_term() else {
                            continue;
                        };
                        let Some(&idx) = rep_pos.get(rep.as_str()) else {
                            continue;
                        };
                        let Some(owned_f) = batch.term(row, 1)? else {
                            continue;
                        };
                        let Term::NamedNode(f) = owned_f.as_term() else {
                            continue;
                        };
                        f_chunk.push((idx, f.clone()));
                        if f_chunk.len() >= FINDING_BATCH {
                            flush_findings(&mut f_chunk, on_lit)?;
                        }
                    }
                }
                drop(f_cursor);
                flush_findings(&mut f_chunk, on_lit)?;
                Ok(())
            };

        let for_each_report_batch =
            |on_batch: ReportBatchFn<'_>| -> Result<(), sparkles::error::Error> {
                enumerate_eligible_reports(
                    r,
                    graph,
                    level,
                    filter,
                    &anchor,
                    &report_cursor_opts,
                    on_batch,
                )
            };

        // Pass 1: stream corpus statistics (N, total_len, df) over eligible literals.
        let mut total_docs: usize = 0;
        let mut total_len: usize = 0;
        let mut df: Vec<usize> = vec![0; q_tokens.len()];
        let mut seen: Vec<bool> = vec![false; q_tokens.len()];
        let mut touched: Vec<usize> = Vec::with_capacity(q_tokens.len());
        let mut has_match = false;
        for_each_report_batch(&mut |reps| {
            for_each_batch_literals(reps, &mut |_idx, lit_val| {
                total_docs += 1;
                let prefix = sparkles::text::truncate_text(lit_val, max_text_bytes);
                let mut doc_len = 0usize;
                touched.clear();
                analyzer.for_each_token(prefix, |tok| {
                    doc_len += 1;
                    if let Some(&qi) = q_index.get(tok)
                        && !seen[qi]
                    {
                        seen[qi] = true;
                        touched.push(qi);
                    }
                });
                total_len += doc_len;
                if !touched.is_empty() {
                    has_match = true;
                    for &qi in &touched {
                        df[qi] += 1;
                        seen[qi] = false;
                    }
                }
                Ok(())
            })
        })?;

        if !has_match || total_docs == 0 {
            return Ok(Some(Vec::new()));
        }

        // Pass 2: stream the same authorized view, reducing each report batch to its
        // per-report MAX(score) and keeping only the top `depth` reports.
        let n = total_docs as f64;
        let avgdl = (total_len as f64 / n).max(1.0);
        let idf: Vec<f64> = df
            .iter()
            .map(|&d| {
                let df_i = d as f64;
                (1.0 + (n - df_i + 0.5) / (df_i + 0.5)).ln()
            })
            .collect();
        let mut counts: Vec<usize> = vec![0; q_tokens.len()];

        /// Heap entry ordered worst-first so `.peek()` / `.pop()` evicts the lowest
        /// score (or lexicographically largest IRI on tie).
        struct WorstFirst(f64, NamedNode);
        impl PartialEq for WorstFirst {
            fn eq(&self, other: &Self) -> bool {
                self.0.total_cmp(&other.0) == Ordering::Equal && self.1 == other.1
            }
        }
        impl Eq for WorstFirst {}
        impl PartialOrd for WorstFirst {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }
        impl Ord for WorstFirst {
            fn cmp(&self, other: &Self) -> Ordering {
                other
                    .0
                    .total_cmp(&self.0)
                    .then_with(|| self.1.as_str().cmp(other.1.as_str()))
            }
        }

        let mut top: BinaryHeap<WorstFirst> = BinaryHeap::with_capacity(depth);
        for_each_report_batch(&mut |reps| {
            let mut batch_best: Vec<f64> = vec![0.0; reps.len()];
            for_each_batch_literals(reps, &mut |idx, lit_val| {
                let prefix = sparkles::text::truncate_text(lit_val, max_text_bytes);
                let mut doc_len = 0usize;
                touched.clear();
                analyzer.for_each_token(prefix, |tok| {
                    doc_len += 1;
                    if let Some(&qi) = q_index.get(tok) {
                        if counts[qi] == 0 {
                            touched.push(qi);
                        }
                        counts[qi] += 1;
                    }
                });
                if touched.is_empty() {
                    return Ok(());
                }
                let norm = 1.0 - B + B * (doc_len as f64) / avgdl;
                let mut score = 0.0f64;
                for &qi in &touched {
                    let f = counts[qi] as f64;
                    counts[qi] = 0;
                    score += idf[qi] * f * (K1 + 1.0) / (f + K1 * norm);
                }
                if score.is_finite() && score > batch_best[idx] {
                    batch_best[idx] = score;
                }
                Ok(())
            })?;
            for (rep, score) in reps.iter().zip(batch_best) {
                if !(score.is_finite() && score > 0.0) {
                    continue;
                }
                if top.len() < depth {
                    top.push(WorstFirst(score, rep.clone()));
                } else if let Some(worst) = top.peek() {
                    let better = match score.total_cmp(&worst.0) {
                        Ordering::Greater => true,
                        Ordering::Equal => rep.as_str() < worst.1.as_str(),
                        Ordering::Less => false,
                    };
                    if better {
                        top.pop();
                        top.push(WorstFirst(score, rep.clone()));
                    }
                }
            }
            Ok(())
        })?;

        let mut out: Vec<(NamedNode, f64)> = top
            .into_iter()
            .map(|WorstFirst(score, rep)| (rep, score))
            .collect();
        out.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| a.0.as_str().cmp(b.0.as_str()))
        });
        Ok(Some(out))
    }
}

const REPORT_BATCH: usize = 16;
const PROP_LOOKUP_BATCH: usize = 8;

fn report_cursor_opts() -> sparkles::sparql::CursorOptions {
    // RejectMaterialization enforces that candidate anchor scans and grounded
    // per-candidate eligibility probes stream strictly without materialization or
    // blocking join/DISTINCT state.
    sparkles::sparql::CursorOptions {
        batch_rows: REPORT_BATCH,
        batch_bytes: 256 << 10,
        fallback: sparkles::sparql::FallbackPolicy::RejectMaterialization,
    }
}

fn balanced_union(branches: &[String]) -> String {
    debug_assert!(!branches.is_empty());
    if branches.len() == 1 {
        return branches[0].clone();
    }
    let mid = branches.len() / 2;
    format!(
        "{{ {} }} UNION {{ {} }}",
        balanced_union(&branches[..mid]),
        balanced_union(&branches[mid..]),
    )
}

enum AnchorPlan {
    /// Scan communities at the target `gr:level` via a streaming numeric `RangeScan`,
    /// skipping retained unlinked revisions and reports at other levels.
    Level { sparql: String },
    /// Scan report subjects by `gr:reviewPolicy` or admitted `gr:status` via single-
    /// predicate ordered `IndexScan`s.
    Report { scans: Vec<ReportAnchorScan> },
}

impl AnchorPlan {
    fn scan_sparql(&self, idx: usize) -> Option<&str> {
        match self {
            AnchorPlan::Level { sparql } => (idx == 0).then_some(sparql.as_str()),
            AnchorPlan::Report { scans } => scans.get(idx).map(|s| s.sparql.as_str()),
        }
    }
}

struct ReportAnchorScan {
    sparql: String,
    /// For status scans, `(status, earlier_admitted_statuses)` so a report carrying
    /// multiple admitted statuses is admitted at most once across scans. `None` for
    /// a `gr:reviewPolicy` anchor scan.
    status_scan: Option<(&'static str, Vec<&'static str>)>,
}

/// Build the candidate single-predicate anchor families for `(graph, level, filter)`:
/// level communities, the requested `reviewPolicy` (when set), and admitted `gr:status`
/// values. Racing them in lock-step selects the narrowest anchor without scanning past
/// excluded reports or relying on redacted planner estimates.
fn candidate_anchor_plans(
    graph: &str,
    level: u32,
    filter: Filter<'_>,
) -> Result<Vec<AnchorPlan>, sparkles::error::Error> {
    let lv = crate::graphrag::guard::gr("level");
    let mut plans = Vec::with_capacity(3);
    plans.push(AnchorPlan::Level {
        sparql: format!(
            "SELECT ?c ?l WHERE {{ GRAPH <{graph}> {{ ?c <{lv}> ?l . FILTER(?l = {level}) }} }}"
        ),
    });
    if let Some(p) = filter.review_policy_iri {
        let node = NamedNode::new(p)
            .map_err(|e| sparkles::error::Error::Invalid(format!("reviewPolicy IRI: {e}")))?;
        let rp = crate::graphrag::guard::gr("reviewPolicy");
        plans.push(AnchorPlan::Report {
            scans: vec![ReportAnchorScan {
                sparql: format!("SELECT ?r WHERE {{ GRAPH <{graph}> {{ ?r <{rp}> {node} . }} }}"),
                status_scan: None,
            }],
        });
    }
    let statuses: Vec<&'static str> = crate::graphrag::store::Status::ALL
        .into_iter()
        .filter(|s| filter.admits(*s))
        .map(|s| s.as_str())
        .collect();
    let status_pred = crate::graphrag::guard::gr("status");
    let mut status_scans = Vec::with_capacity(statuses.len());
    for (i, &st) in statuses.iter().enumerate() {
        status_scans.push(ReportAnchorScan {
            sparql: format!(
                "SELECT ?r WHERE {{ GRAPH <{graph}> {{ ?r <{status_pred}> \"{st}\" . }} }}"
            ),
            status_scan: Some((st, statuses[..i].to_vec())),
        });
    }
    plans.push(AnchorPlan::Report {
        scans: status_scans,
    });
    Ok(plans)
}

fn is_row_budget_error(e: &sparkles::error::Error) -> bool {
    matches!(
        e,
        sparkles::error::Error::BudgetExceeded(sparkles::error::Budget {
            kind: sparkles::error::BudgetKind::Rows | sparkles::error::BudgetKind::RowsProduced,
            ..
        })
    )
}

/// Race candidate single-predicate anchor plans in lock-step (`REPORT_BATCH` rows per
/// round, immediately dropping each batch so memory stays `O(1)`), returning the first
/// anchor that reaches EOF, or `Ok(None)` when the narrowest anchor is empty. Any
/// broader anchor that hits a row budget is eliminated while narrower candidates
/// continue; non-row-budget errors propagate immediately.
fn select_narrowest_anchor(
    r: &Reader,
    candidates: Vec<AnchorPlan>,
    cursor_opts: &sparkles::sparql::CursorOptions,
) -> Result<Option<AnchorPlan>, sparkles::error::Error> {
    struct Racer {
        plan: AnchorPlan,
        scan_idx: usize,
        cursor: Option<sparkles::sparql::QueryCursor>,
        total_rows: usize,
    }
    let mut racers: Vec<Racer> = candidates
        .into_iter()
        .map(|plan| Racer {
            plan,
            scan_idx: 0,
            cursor: None,
            total_rows: 0,
        })
        .collect();
    let mut first_row_err: Option<sparkles::error::Error> = None;
    while !racers.is_empty() {
        let mut idx = 0;
        while idx < racers.len() {
            let step = (|| -> Result<bool, sparkles::error::Error> {
                let racer = &mut racers[idx];
                loop {
                    if racer.cursor.is_none() {
                        let Some(sparql) = racer.plan.scan_sparql(racer.scan_idx) else {
                            return Ok(true);
                        };
                        racer.cursor = Some(r.select_cursor(sparql, cursor_opts)?);
                    }
                    let cursor = racer.cursor.as_mut().expect("opened above");
                    match cursor.next_batch()? {
                        Some(batch) => {
                            racer.total_rows = racer.total_rows.saturating_add(batch.len());
                            drop(batch);
                            return Ok(false);
                        }
                        None => {
                            racer.cursor = None;
                            racer.scan_idx += 1;
                            if racer.plan.scan_sparql(racer.scan_idx).is_none() {
                                return Ok(true);
                            }
                        }
                    }
                }
            })();
            match step {
                Ok(true) => {
                    let winner = racers.swap_remove(idx);
                    drop(racers);
                    return Ok((winner.total_rows > 0).then_some(winner.plan));
                }
                Ok(false) => {
                    idx += 1;
                }
                Err(e) if is_row_budget_error(&e) => {
                    first_row_err.get_or_insert(e);
                    racers.remove(idx);
                }
                Err(e) => return Err(e),
            }
        }
    }
    match first_row_err {
        Some(e) => Err(e),
        None => Ok(None),
    }
}

fn literal_equals_level(lit: &Literal, level: u32) -> bool {
    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
    let Some(local) = lit.datatype().as_str().strip_prefix(XSD) else {
        return false;
    };
    let lex = lit.value().trim();
    match local {
        "integer" | "int" | "long" | "short" | "byte" | "nonNegativeInteger"
        | "positiveInteger" | "unsignedInt" | "unsignedLong" | "unsignedShort" | "unsignedByte"
        | "nonPositiveInteger" | "negativeInteger" => {
            lex.parse::<i128>().ok() == Some(i128::from(level))
        }
        "decimal" => match lex.split_once('.') {
            Some((int_part, frac_part)) => {
                !frac_part.is_empty()
                    && frac_part.bytes().all(|b| b == b'0')
                    && (int_part.is_empty() && level == 0
                        || int_part.parse::<i128>().ok() == Some(i128::from(level)))
            }
            None => lex.parse::<i128>().ok() == Some(i128::from(level)),
        },
        "float" | "double" => lex
            .parse::<f64>()
            .ok()
            .is_some_and(|v| v.is_finite() && v == f64::from(level)),
        _ => false,
    }
}

/// Build a strictly grounded, predicate-pinned GSPO lookup of `gr:status` and
/// `gr:reviewPolicy` triples for up to `PROP_LOOKUP_BATCH` candidate report IRIs (at
/// most 16 UNION leaves, zero joins, zero `DISTINCT`, zero `EXISTS` decorrelation, and
/// never scanning `gr:finding`, `gr:citesReport`, or other outgoing predicates).
fn build_report_props_lookup_sparql(graph: &str, reps: &[NamedNode]) -> String {
    let status_pred = crate::graphrag::guard::gr("status");
    let policy_pred = crate::graphrag::guard::gr("reviewPolicy");
    let mut branches = Vec::with_capacity(reps.len() * 2);
    for rep in reps {
        branches.push(format!("{rep} <{status_pred}> ?st . BIND({rep} AS ?r)"));
        branches.push(format!("{rep} <{policy_pred}> ?pol . BIND({rep} AS ?r)"));
    }
    format!(
        "SELECT ?r ?st ?pol WHERE {{ GRAPH <{graph}> {{ {} }} }}",
        balanced_union(&branches),
    )
}

/// Build a strictly grounded lookup of `<gr:report>` community links pointing to up to
/// `REPORT_BATCH` report IRIs.
fn build_report_communities_lookup_sparql(graph: &str, reps: &[NamedNode]) -> String {
    let report_pred = crate::graphrag::guard::gr("report");
    let branches: Vec<String> = reps
        .iter()
        .map(|rep| format!("?c <{report_pred}> {rep} . BIND({rep} AS ?r)"))
        .collect();
    format!(
        "SELECT ?r ?c WHERE {{ GRAPH <{graph}> {{ {} }} }}",
        balanced_union(&branches),
    )
}

/// Build a strictly grounded, predicate-pinned GSPO lookup of `rdf:type <gr:Community>`
/// (emits `?o` unbound) and `gr:level` (emits `?o` bound) for up to `PROP_LOOKUP_BATCH`
/// community IRIs without scanning `gr:member` or other outgoing community predicates.
fn build_community_props_lookup_sparql(graph: &str, comms: &[NamedNode]) -> String {
    let community_cls = crate::graphrag::guard::gr("Community");
    let lv = crate::graphrag::guard::gr("level");
    let mut branches = Vec::with_capacity(comms.len() * 2);
    for c in comms {
        branches.push(format!(
            "{c} <{rdf_type}> <{community_cls}> . BIND({c} AS ?c)",
            rdf_type = super::RDF_TYPE,
        ));
        branches.push(format!("{c} <{lv}> ?o . BIND({c} AS ?c)"));
    }
    format!(
        "SELECT ?c ?o WHERE {{ GRAPH <{graph}> {{ {} }} }}",
        balanced_union(&branches),
    )
}

/// Build a strictly grounded lookup of `<gr:report>` links for up to `REPORT_BATCH`
/// community IRIs.
fn build_community_reports_lookup_sparql(graph: &str, comms: &[NamedNode]) -> String {
    let report_pred = crate::graphrag::guard::gr("report");
    let branches: Vec<String> = comms
        .iter()
        .map(|c| format!("{c} <{report_pred}> ?r . BIND({c} AS ?c)"))
        .collect();
    format!(
        "SELECT ?c ?r WHERE {{ GRAPH <{graph}> {{ {} }} }}",
        balanced_union(&branches),
    )
}

#[derive(Default)]
struct ReportProps {
    has_policy: bool,
    has_admitted_status: bool,
    has_prior_status: bool,
}

/// Fetch `gr:status` and `gr:reviewPolicy` for `reps` (`<= REPORT_BATCH`) via a grounded
/// `RejectMaterialization` GSPO lookup and return which entries satisfy `filter` (and
/// carry no earlier status when `status_scan` is set).
fn check_report_props(
    r: &Reader,
    graph: &str,
    filter: Filter<'_>,
    reps: &[NamedNode],
    status_scan: Option<(&'static str, &[&'static str])>,
    cursor_opts: &sparkles::sparql::CursorOptions,
) -> Result<Vec<bool>, sparkles::error::Error> {
    const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

    if reps.is_empty() {
        return Ok(Vec::new());
    }
    let pos: HashMap<&str, usize> = reps
        .iter()
        .enumerate()
        .map(|(i, rep)| (rep.as_str(), i))
        .collect();
    let mut state: Vec<ReportProps> = (0..reps.len()).map(|_| ReportProps::default()).collect();
    for chunk in reps.chunks(PROP_LOOKUP_BATCH) {
        let sparql = build_report_props_lookup_sparql(graph, chunk);
        let mut cursor = r.select_cursor(&sparql, cursor_opts)?;
        while let Some(batch) = cursor.next_batch()? {
            for row in 0..batch.len() {
                let Some(owned_r) = batch.term(row, 0)? else {
                    continue;
                };
                let Term::NamedNode(rep) = owned_r.as_term() else {
                    continue;
                };
                let Some(&idx) = pos.get(rep.as_str()) else {
                    continue;
                };
                if let Some(owned_st) = batch.term(row, 1)?
                    && let Term::Literal(lit) = owned_st.as_term()
                    && lit.datatype().as_str() == XSD_STRING
                {
                    if let Some((_, prior)) = status_scan
                        && prior.contains(&lit.value())
                    {
                        state[idx].has_prior_status = true;
                    }
                    if crate::graphrag::store::Status::parse(lit.value())
                        .is_some_and(|st| filter.admits(st))
                    {
                        state[idx].has_admitted_status = true;
                    }
                }
                if let Some(owned_pol) = batch.term(row, 2)?
                    && let Term::NamedNode(pol) = owned_pol.as_term()
                    && filter.review_policy_iri == Some(pol.as_str())
                {
                    state[idx].has_policy = true;
                }
            }
        }
    }
    Ok(state
        .into_iter()
        .map(|s| {
            let policy_ok = filter.review_policy_iri.is_none() || s.has_policy;
            policy_ok && s.has_admitted_status && !s.has_prior_status
        })
        .collect())
}

#[derive(Default)]
struct CommunityProps {
    is_community: bool,
    min_level_key: Option<(String, String)>,
}

/// Fetch `rdf:type <gr:Community>` and `gr:level` for `comms` via chunked,
/// predicate-pinned `RejectMaterialization` GSPO lookups.
fn fetch_community_props(
    r: &Reader,
    graph: &str,
    level: u32,
    comms: &[NamedNode],
    cursor_opts: &sparkles::sparql::CursorOptions,
) -> Result<Vec<CommunityProps>, sparkles::error::Error> {
    if comms.is_empty() {
        return Ok(Vec::new());
    }
    let pos: HashMap<&str, usize> = comms
        .iter()
        .enumerate()
        .map(|(i, c)| (c.as_str(), i))
        .collect();
    let mut state: Vec<CommunityProps> = (0..comms.len())
        .map(|_| CommunityProps::default())
        .collect();
    for chunk in comms.chunks(PROP_LOOKUP_BATCH) {
        let sparql = build_community_props_lookup_sparql(graph, chunk);
        let mut cursor = r.select_cursor(&sparql, cursor_opts)?;
        while let Some(batch) = cursor.next_batch()? {
            for row in 0..batch.len() {
                let Some(owned_c) = batch.term(row, 0)? else {
                    continue;
                };
                let Term::NamedNode(comm) = owned_c.as_term() else {
                    continue;
                };
                let Some(&idx) = pos.get(comm.as_str()) else {
                    continue;
                };
                match batch.term(row, 1)? {
                    None => {
                        state[idx].is_community = true;
                    }
                    Some(owned_o) => {
                        if let Term::Literal(lit) = owned_o.as_term()
                            && literal_equals_level(lit, level)
                        {
                            let key =
                                (lit.datatype().as_str().to_string(), lit.value().to_string());
                            let replace = match &state[idx].min_level_key {
                                None => true,
                                Some(prev) => key < *prev,
                            };
                            if replace {
                                state[idx].min_level_key = Some(key);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(state)
}

fn enumerate_eligible_reports(
    r: &Reader,
    graph: &str,
    level: u32,
    filter: Filter<'_>,
    anchor: &AnchorPlan,
    cursor_opts: &sparkles::sparql::CursorOptions,
    on_batch: &mut dyn FnMut(&[NamedNode]) -> Result<(), sparkles::error::Error>,
) -> Result<(), sparkles::error::Error> {
    let mut eligible: Vec<NamedNode> = Vec::with_capacity(REPORT_BATCH);
    match anchor {
        AnchorPlan::Level { sparql } => {
            let mut entries: Vec<(NamedNode, Literal)> = Vec::with_capacity(REPORT_BATCH);
            let mut cursor = r.select_cursor(sparql, cursor_opts)?;
            while let Some(batch) = cursor.next_batch()? {
                entries.clear();
                for row in 0..batch.len() {
                    if let (Some(owned_c), Some(owned_l)) =
                        (batch.term(row, 0)?, batch.term(row, 1)?)
                        && let (Term::NamedNode(comm), Term::Literal(level_lit)) =
                            (owned_c.as_term(), owned_l.as_term())
                    {
                        entries.push((comm.clone(), level_lit.clone()));
                    }
                }
                drop(batch);
                if entries.is_empty() {
                    continue;
                }
                let mut uniq_comms: Vec<NamedNode> = Vec::with_capacity(entries.len());
                let mut comm_pos: HashMap<&str, usize> = HashMap::with_capacity(entries.len());
                for (comm, _) in &entries {
                    if !comm_pos.contains_key(comm.as_str()) {
                        comm_pos.insert(comm.as_str(), uniq_comms.len());
                        uniq_comms.push(comm.clone());
                    }
                }
                let comm_props = fetch_community_props(r, graph, level, &uniq_comms, cursor_opts)?;
                // Keep only eligible communities whose anchor level literal is their
                // canonical (smallest datatype, lexical) equal-valued level literal,
                // deduplicating mixed-datatype level literals across all batches.
                let mut valid_comms: Vec<NamedNode> = Vec::with_capacity(uniq_comms.len());
                for (comm, level_lit) in &entries {
                    let idx = comm_pos[comm.as_str()];
                    let props = &comm_props[idx];
                    if props.is_community
                        && props.min_level_key.as_ref().is_some_and(|(dt, lex)| {
                            dt == level_lit.datatype().as_str() && lex == level_lit.value()
                        })
                        && !valid_comms.iter().any(|existing| existing == comm)
                    {
                        valid_comms.push(comm.clone());
                    }
                }
                if valid_comms.is_empty() {
                    continue;
                }
                let lookup_sparql = build_community_reports_lookup_sparql(graph, &valid_comms);
                let mut lookup_cursor = r.select_cursor(&lookup_sparql, cursor_opts)?;
                let mut pair_chunk: Vec<(NamedNode, NamedNode)> = Vec::with_capacity(REPORT_BATCH);
                let flush_pairs = |chunk: &mut Vec<(NamedNode, NamedNode)>,
                                   eligible: &mut Vec<NamedNode>,
                                   on_batch: &mut dyn FnMut(
                    &[NamedNode],
                ) -> Result<
                    (),
                    sparkles::error::Error,
                >|
                 -> Result<(), sparkles::error::Error> {
                    if chunk.is_empty() {
                        return Ok(());
                    }
                    let mut uniq_reps: Vec<NamedNode> = Vec::with_capacity(chunk.len());
                    for (_, rep) in chunk.iter() {
                        if !uniq_reps.iter().any(|u| u == rep) {
                            uniq_reps.push(rep.clone());
                        }
                    }
                    let rep_idx_map: HashMap<&str, usize> = uniq_reps
                        .iter()
                        .enumerate()
                        .map(|(i, rep)| (rep.as_str(), i))
                        .collect();
                    let rep_ok =
                        check_report_props(r, graph, filter, &uniq_reps, None, cursor_opts)?;
                    let status_ok: Vec<(NamedNode, NamedNode)> = chunk
                        .drain(..)
                        .filter(|(_, rep)| rep_ok[rep_idx_map[rep.as_str()]])
                        .collect();
                    if status_ok.is_empty() {
                        return Ok(());
                    }
                    // Cross-batch canonical-community deduplication: admit `(comm, rep)`
                    // only when no lexicographically smaller eligible community at `level`
                    // also links to `rep`.
                    let mut ok_reps: Vec<NamedNode> = Vec::with_capacity(status_ok.len());
                    let mut ok_by_rep: HashMap<&str, Vec<usize>> =
                        HashMap::with_capacity(status_ok.len());
                    for (pair_idx, (_, rep)) in status_ok.iter().enumerate() {
                        let slots = ok_by_rep.entry(rep.as_str()).or_default();
                        if slots.is_empty() {
                            ok_reps.push(rep.clone());
                        }
                        slots.push(pair_idx);
                    }
                    let mut shadowed = vec![false; status_ok.len()];
                    let rev_sparql = build_report_communities_lookup_sparql(graph, &ok_reps);
                    let mut rev_cursor = r.select_cursor(&rev_sparql, cursor_opts)?;
                    let mut smaller_chunk: Vec<(usize, NamedNode)> =
                        Vec::with_capacity(REPORT_BATCH);
                    let flush_smaller = |sc: &mut Vec<(usize, NamedNode)>,
                                         shadowed: &mut [bool]|
                     -> Result<(), sparkles::error::Error> {
                        if sc.is_empty() {
                            return Ok(());
                        }
                        let mut uniq_c2: Vec<NamedNode> = Vec::with_capacity(sc.len());
                        for (_, c2) in sc.iter() {
                            if !uniq_c2.iter().any(|u| u == c2) {
                                uniq_c2.push(c2.clone());
                            }
                        }
                        let c2_pos: HashMap<&str, usize> = uniq_c2
                            .iter()
                            .enumerate()
                            .map(|(i, c)| (c.as_str(), i))
                            .collect();
                        let c2_props =
                            fetch_community_props(r, graph, level, &uniq_c2, cursor_opts)?;
                        for (pair_idx, c2) in sc.drain(..) {
                            let p = &c2_props[c2_pos[c2.as_str()]];
                            if p.is_community && p.min_level_key.is_some() {
                                shadowed[pair_idx] = true;
                            }
                        }
                        Ok(())
                    };
                    while let Some(rb) = rev_cursor.next_batch()? {
                        for row in 0..rb.len() {
                            if let (Some(owned_r), Some(owned_c2)) =
                                (rb.term(row, 0)?, rb.term(row, 1)?)
                                && let (Term::NamedNode(rep), Term::NamedNode(c2)) =
                                    (owned_r.as_term(), owned_c2.as_term())
                                && let Some(pair_indices) = ok_by_rep.get(rep.as_str())
                            {
                                for &pair_idx in pair_indices {
                                    if !shadowed[pair_idx]
                                        && c2.as_str() < status_ok[pair_idx].0.as_str()
                                    {
                                        smaller_chunk.push((pair_idx, c2.clone()));
                                        if smaller_chunk.len() >= REPORT_BATCH {
                                            flush_smaller(&mut smaller_chunk, &mut shadowed)?;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    drop(rev_cursor);
                    flush_smaller(&mut smaller_chunk, &mut shadowed)?;
                    eligible.clear();
                    for (pair_idx, (_, rep)) in status_ok.into_iter().enumerate() {
                        if !shadowed[pair_idx] {
                            eligible.push(rep);
                        }
                    }
                    if !eligible.is_empty() {
                        on_batch(eligible)?;
                    }
                    Ok(())
                };
                while let Some(lb) = lookup_cursor.next_batch()? {
                    for row in 0..lb.len() {
                        if let (Some(owned_c), Some(owned_r)) = (lb.term(row, 0)?, lb.term(row, 1)?)
                            && let (Term::NamedNode(comm), Term::NamedNode(rep)) =
                                (owned_c.as_term(), owned_r.as_term())
                        {
                            pair_chunk.push((comm.clone(), rep.clone()));
                            if pair_chunk.len() >= REPORT_BATCH {
                                flush_pairs(&mut pair_chunk, &mut eligible, on_batch)?;
                            }
                        }
                    }
                }
                drop(lookup_cursor);
                flush_pairs(&mut pair_chunk, &mut eligible, on_batch)?;
            }
        }
        AnchorPlan::Report { scans } => {
            let mut candidates: Vec<NamedNode> = Vec::with_capacity(REPORT_BATCH);
            for scan in scans {
                let mut cursor = r.select_cursor(&scan.sparql, cursor_opts)?;
                while let Some(batch) = cursor.next_batch()? {
                    candidates.clear();
                    for row in 0..batch.len() {
                        if let Some(owned) = batch.term(row, 0)?
                            && let Term::NamedNode(rep) = owned.as_term()
                        {
                            candidates.push(rep.clone());
                        }
                    }
                    drop(batch);
                    if candidates.is_empty() {
                        continue;
                    }
                    let status_scan = scan
                        .status_scan
                        .as_ref()
                        .map(|(st, prior)| (*st, prior.as_slice()));
                    let rep_ok = check_report_props(
                        r,
                        graph,
                        filter,
                        &candidates,
                        status_scan,
                        cursor_opts,
                    )?;
                    let surviving: Vec<NamedNode> = candidates
                        .iter()
                        .zip(rep_ok)
                        .filter(|&(_, ok)| ok)
                        .map(|(rep, _)| rep.clone())
                        .collect();
                    if surviving.is_empty() {
                        continue;
                    }
                    let surv_pos: HashMap<&str, usize> = surviving
                        .iter()
                        .enumerate()
                        .map(|(i, rep)| (rep.as_str(), i))
                        .collect();
                    let mut admitted = vec![false; surviving.len()];
                    let rev_sparql = build_report_communities_lookup_sparql(graph, &surviving);
                    let mut rev_cursor = r.select_cursor(&rev_sparql, cursor_opts)?;
                    let mut comm_chunk: Vec<(usize, NamedNode)> = Vec::with_capacity(REPORT_BATCH);
                    let flush_comms = |cc: &mut Vec<(usize, NamedNode)>,
                                       admitted: &mut [bool]|
                     -> Result<(), sparkles::error::Error> {
                        if cc.is_empty() {
                            return Ok(());
                        }
                        let mut uniq_c: Vec<NamedNode> = Vec::with_capacity(cc.len());
                        for (_, c) in cc.iter() {
                            if !uniq_c.iter().any(|u| u == c) {
                                uniq_c.push(c.clone());
                            }
                        }
                        let c_pos: HashMap<&str, usize> = uniq_c
                            .iter()
                            .enumerate()
                            .map(|(i, c)| (c.as_str(), i))
                            .collect();
                        let c_props = fetch_community_props(r, graph, level, &uniq_c, cursor_opts)?;
                        for (rep_idx, c) in cc.drain(..) {
                            let p = &c_props[c_pos[c.as_str()]];
                            if p.is_community && p.min_level_key.is_some() {
                                admitted[rep_idx] = true;
                            }
                        }
                        Ok(())
                    };
                    while let Some(rb) = rev_cursor.next_batch()? {
                        for row in 0..rb.len() {
                            if let (Some(owned_r), Some(owned_c)) =
                                (rb.term(row, 0)?, rb.term(row, 1)?)
                                && let (Term::NamedNode(rep), Term::NamedNode(comm)) =
                                    (owned_r.as_term(), owned_c.as_term())
                                && let Some(&rep_idx) = surv_pos.get(rep.as_str())
                                && !admitted[rep_idx]
                            {
                                comm_chunk.push((rep_idx, comm.clone()));
                                if comm_chunk.len() >= REPORT_BATCH {
                                    flush_comms(&mut comm_chunk, &mut admitted)?;
                                }
                            }
                        }
                    }
                    drop(rev_cursor);
                    flush_comms(&mut comm_chunk, &mut admitted)?;
                    eligible.clear();
                    for (rep, ok) in surviving.into_iter().zip(admitted) {
                        if ok {
                            eligible.push(rep);
                        }
                    }
                    if !eligible.is_empty() {
                        on_batch(&eligible)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// The predicates of the vector indexes that embed query text and may hold report text:
/// an index over `gr:summary`, `gr:text` or `rdfs:label`, one with no predicate list, or
/// one fed by its own query (round 1, finding 8).
fn report_embeddings(ds: &crate::state::Dataset) -> Vec<NamedNode> {
    let wanted = [
        crate::graphrag::guard::gr("summary"),
        crate::graphrag::guard::gr("text"),
        crate::graphrag::store::RDFS_LABEL.to_string(),
    ];
    let mut list = ds.dataset.indexes().vector().list();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list.into_iter()
        .filter(|s| {
            s.embedding.as_ref().is_some_and(|e| {
                e.config.query_text
                    && (e.config.query.is_some()
                        || e.config.predicates.is_empty()
                        || e.config.predicates.iter().any(|p| wanted.contains(p)))
            })
        })
        .map(|s| super::iri(&s.predicate))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn budget_identities_bounds_large_shared_cluster_across_reports() {
        let rep = NamedNode::new_unchecked("http://example.org/entity/m000");
        let members: Arc<[NamedNode]> = (0..500)
            .map(|i| NamedNode::new_unchecked(format!("http://example.org/entity/m{i:03}")))
            .collect::<Vec<_>>()
            .into();
        let mut doc = ResolutionDoc::new("people", 1);
        let mut report_ents = Vec::new();
        for i in 0..200 {
            let iri = format!("http://example.org/entity/m{i:03}");
            doc.entities.insert(
                iri.clone(),
                ResolvedDoc {
                    representative: rep.clone(),
                    members: Arc::clone(&members),
                    complete: true,
                    named: true,
                },
            );
            report_ents.push(iri);
        }
        // Three reports each referencing all 200 members of the same 500-member cluster
        // (800 identity copies total -> 400,000 raw member IRIs if unbounded).
        let reports = vec![report_ents.clone(), report_ents.clone(), report_ents];
        let cap = budget_identities(
            &doc,
            &reports,
            MAX_IDENTITY_MEMBER_ENTRIES,
            MAX_IDENTITY_BYTES,
        )
        .unwrap();
        assert!(cap < 500, "expected budget truncation, got cap={cap}");
        assert!(
            cap * 800 <= MAX_IDENTITY_MEMBER_ENTRIES,
            "cap {cap} exceeds aggregate member budget"
        );
        let item = identity_json(
            "http://example.org/entity/m001",
            &doc.entities["http://example.org/entity/m001"],
            cap,
        );
        assert_eq!(item["members"].as_array().unwrap().len(), cap);
        assert_eq!(item["membersComplete"], false);

        // If even bare entity -> representative entries exceed max_bytes, refuse with 422.
        let err = budget_identities(&doc, &reports, MAX_IDENTITY_MEMBER_ENTRIES, 1024).unwrap_err();
        assert_eq!(err.code.as_ref(), "incomplete-evidence");
        assert_eq!(err.status, 422);
    }

    #[test]
    fn report_enumeration_scan_and_eligibility_plans_are_bounded() {
        const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
        let gr = crate::graphrag::guard::gr;
        let (c_cls, lv, rep, stat, rp) = (
            gr("Community"),
            gr("level"),
            gr("report"),
            gr("status"),
            gr("reviewPolicy"),
        );
        let main_iri = "urn:ev:policy:main";

        let build_store = |count: usize| -> sparkles::store::Store {
            let store = sparkles::store::Store::in_memory(sparkles::store::StoreOptions::default());
            let prep =
                sparkles::access::Preparation::new(sparkles::task::Control::none(), 64 << 20)
                    .unwrap();
            store.activate_memory_configuration_history(&prep).unwrap();
            let mut trig = format!("<{REPORTS}> {{\n");
            for i in 0..count {
                trig.push_str(&format!(
                    "  <urn:c:{i:04}> a <{c_cls}> ; <{lv}> 0 ; <{rep}> <urn:r:{i:04}> .\n  <urn:r:{i:04}> <{stat}> \"reviewed\" ; <{rp}> <{main_iri}> .\n"
                ));
            }
            trig.push_str("}\n");
            store
                .load(&[sparkles::io::Source::from_bytes(
                    trig.into_bytes(),
                    sparkles::io::RdfFormat::TriG,
                    None,
                )])
                .unwrap();
            store
        };

        fn assert_streaming_no_joins(plan: &sparkles::sparql::CursorPlan) {
            assert!(
                !plan.materializes,
                "unexpected materialization at {}",
                plan.operator.operator
            );
            assert!(
                !plan.full_input_before_output,
                "unexpected blocking operator {}",
                plan.operator.operator
            );
            assert!(
                !matches!(
                    plan.operator.operator.as_str(),
                    "Distinct" | "HashJoin" | "MergeJoin" | "IndexJoin" | "CartesianProduct"
                ),
                "unexpected join/distinct operator {} in plan: {plan:?}",
                plan.operator.operator
            );
            for child in &plan.children {
                assert_streaming_no_joins(child);
            }
        }

        fn assert_index_anchor_no_growing_state(plan: &sparkles::sparql::CursorPlan) {
            assert!(
                !plan.growing_state,
                "unexpected growing state at {}",
                plan.operator.operator
            );
            for child in &plan.children {
                assert_index_anchor_no_growing_state(child);
            }
        }

        let scan_opts = report_cursor_opts();
        let q_opts = sparkles::sparql::QueryOptions {
            max_memory_bytes: Some(256 << 10),
            ..sparkles::sparql::QueryOptions::default()
        };
        let mut source_q_opts = q_opts.clone();
        source_q_opts.graphs = Some(Arc::new(sparkles::access::GraphAccess::all()));

        let small = build_store(50);
        let large = build_store(2_000);

        let open_cursor = |store: &sparkles::store::Store,
                           bound_source: bool,
                           sparql: &str|
         -> sparkles::sparql::QueryCursor {
            if bound_source {
                let prep =
                    sparkles::access::Preparation::new(sparkles::task::Control::none(), 64 << 20)
                        .unwrap();
                let view = store
                    .evidence_view(&sparkles::history::At::Head, &prep)
                    .unwrap();
                sparkles::sparql::select_source_cursor(
                    view.read_source(),
                    store,
                    sparql,
                    &source_q_opts,
                    &scan_opts,
                )
                .unwrap()
            } else {
                sparkles::sparql::select_cursor(store.snapshot(), sparql, &q_opts, &scan_opts)
                    .unwrap()
            }
        };

        for bound_source in [false, true] {
            for filter in [
                Filter {
                    reviewed_only: false,
                    review_policy_iri: Some(main_iri),
                },
                Filter {
                    reviewed_only: false,
                    review_policy_iri: None,
                },
                Filter {
                    reviewed_only: true,
                    review_policy_iri: None,
                },
            ] {
                let plans = candidate_anchor_plans(REPORTS, 0, filter).unwrap();
                for anchor in &plans {
                    let run_anchor = |store: &sparkles::store::Store, expected: usize| -> u64 {
                        let mut seen: std::collections::HashSet<String> =
                            std::collections::HashSet::new();
                        let mut max_peak = 0u64;
                        let mut idx = 0;
                        while let Some(sparql) = anchor.scan_sparql(idx) {
                            let mut cursor = open_cursor(store, bound_source, sparql);
                            assert_streaming_no_joins(cursor.plan());
                            if matches!(anchor, AnchorPlan::Report { .. }) {
                                assert_index_anchor_no_growing_state(cursor.plan());
                            }
                            while let Some(batch) = cursor.next_batch().unwrap() {
                                for row in 0..batch.len() {
                                    if let Some(owned) = batch.term(row, 0).unwrap()
                                        && let Term::NamedNode(n) = owned.as_term()
                                    {
                                        assert!(
                                            seen.insert(n.as_str().to_string()),
                                            "duplicate row from single-predicate anchor scan: {n}"
                                        );
                                    }
                                }
                            }
                            max_peak = max_peak.max(cursor.stats().mem_peak_bytes);
                            idx += 1;
                        }
                        assert_eq!(seen.len(), expected);
                        max_peak
                    };

                    let small_peak = run_anchor(&small, 50);
                    let large_peak = run_anchor(&large, 2_000);
                    assert!(
                        large_peak <= 64 << 10,
                        "anchor scan peak memory grew unexpectedly (bound_source={bound_source}): {large_peak} bytes (50-report peak: {small_peak})"
                    );
                    assert!(
                        large_peak <= small_peak.saturating_add(16 << 10),
                        "anchor scan peak memory should remain O(1) across 40x growth (bound_source={bound_source}): small={small_peak}, large={large_peak}"
                    );
                }

                // Inspect grounded eligibility lookup plans on both snapshot and
                // bound-source routes (under tight 256 KiB memory and RejectMaterialization),
                // verifying O(1) memory across 50 vs 2,000 reports.
                let sample_reps: Vec<NamedNode> = (0..REPORT_BATCH)
                    .map(|i| NamedNode::new_unchecked(format!("urn:r:{i:04}")))
                    .collect();
                let sample_comms: Vec<NamedNode> = (0..REPORT_BATCH)
                    .map(|i| NamedNode::new_unchecked(format!("urn:c:{i:04}")))
                    .collect();

                for (name, sparql) in [
                    (
                        "report_props",
                        build_report_props_lookup_sparql(
                            REPORTS,
                            &sample_reps[..PROP_LOOKUP_BATCH],
                        ),
                    ),
                    (
                        "report_communities",
                        build_report_communities_lookup_sparql(REPORTS, &sample_reps),
                    ),
                    (
                        "community_props",
                        build_community_props_lookup_sparql(
                            REPORTS,
                            &sample_comms[..PROP_LOOKUP_BATCH],
                        ),
                    ),
                    (
                        "community_reports",
                        build_community_reports_lookup_sparql(REPORTS, &sample_comms),
                    ),
                ] {
                    let run_elig = |store: &sparkles::store::Store| -> u64 {
                        let mut cursor = open_cursor(store, bound_source, &sparql);
                        assert_streaming_no_joins(cursor.plan());
                        let mut rows = 0usize;
                        while let Some(batch) = cursor.next_batch().unwrap() {
                            rows += batch.len();
                        }
                        assert!(
                            rows > 0,
                            "expected grounded eligibility query {name} to return rows"
                        );
                        cursor.stats().mem_peak_bytes
                    };
                    let small_elig_peak = run_elig(&small);
                    let large_elig_peak = run_elig(&large);
                    assert!(
                        large_elig_peak <= 200 << 10,
                        "grounded eligibility query {name} exceeded 200 KiB (bound_source={bound_source}): {large_elig_peak}"
                    );
                    assert!(
                        large_elig_peak <= small_elig_peak.saturating_add(16 << 10),
                        "grounded eligibility query {name} memory grew across 40x store growth (bound_source={bound_source}): small={small_elig_peak}, large={large_elig_peak}"
                    );
                }
            }
        }

        // Contrast: the legacy multi-pattern `SELECT DISTINCT ?r` query retains growing
        // state (DISTINCT/join) on the same 2,000-report store.
        let old_sparql = format!(
            "SELECT DISTINCT ?r WHERE {{ GRAPH <{REPORTS}> {{ VALUES ?status {{ \"reviewed\" \"unreviewed\" \"stale\" }} ?c a <{c_cls}> ; <{lv}> ?l ; <{rep}> ?r . FILTER(?l = 0) ?r <{stat}> ?status ; <{rp}> <{main_iri}> . }} }}"
        );
        let old_cursor = sparkles::sparql::select_cursor(
            large.snapshot(),
            &old_sparql,
            &sparkles::sparql::QueryOptions::default(),
            &sparkles::sparql::CursorOptions {
                batch_rows: 16,
                batch_bytes: 256 << 10,
                fallback: sparkles::sparql::FallbackPolicy::AllowMaterialization,
            },
        )
        .unwrap();
        fn has_growing_distinct_or_join(plan: &sparkles::sparql::CursorPlan) -> bool {
            (plan.growing_state
                && matches!(
                    plan.operator.operator.as_str(),
                    "Distinct" | "HashJoin" | "MergeJoin"
                ))
                || plan.children.iter().any(has_growing_distinct_or_join)
        }
        assert!(
            has_growing_distinct_or_join(old_cursor.plan()),
            "expected legacy SELECT DISTINCT join to carry growing state: {:?}",
            old_cursor.plan()
        );
    }

    #[test]
    fn report_enumeration_delivers_each_eligible_report_exactly_once() {
        const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
        let gr = crate::graphrag::guard::gr;
        let (c_cls, lv, rep, stat, rp, edit_of) = (
            gr("Community"),
            gr("level"),
            gr("report"),
            gr("status"),
            gr("reviewPolicy"),
            gr("editOf"),
        );
        let main_iri = "urn:ev:policy:main";
        let other_iri = "urn:ev:policy:other";

        let st = crate::state::AppState::standalone(
            Default::default(),
            std::time::Duration::from_secs(30),
        );
        let ds = st.attach("enum", crate::state::DbType::Mem, None).unwrap();
        let prep =
            sparkles::access::Preparation::new(sparkles::task::Control::none(), 64 << 20).unwrap();
        ds.dataset
            .store()
            .activate_memory_configuration_history(&prep)
            .unwrap();

        let mut trig = format!("<{REPORTS}> {{\n");
        // Duplicate community links separated by > REPORT_BATCH (16) communities so they
        // land in different batches under the Level anchor.
        trig.push_str(&format!(
            "  <urn:c:0000-dup-a> a <{c_cls}> ; <{lv}> 0 ; <{rep}> <urn:r:dup-comm> .\n  <urn:c:zzzz-dup-b> a <{c_cls}> ; <{lv}> 0 ; <{rep}> <urn:r:dup-comm> .\n  <urn:r:dup-comm> <{stat}> \"reviewed\" ; <{rp}> <{main_iri}> .\n"
        ));
        // Report with multiple admitted statuses ("reviewed" and "stale").
        trig.push_str(&format!(
            "  <urn:c:0001-multi> a <{c_cls}> ; <{lv}> 0 ; <{rep}> <urn:r:multi-stat> .\n  <urn:r:multi-stat> <{stat}> \"reviewed\", \"stale\" ; <{rp}> <{main_iri}> .\n"
        ));
        // Community with duplicate equal-valued numeric level literals (xsd:integer + xsd:int).
        trig.push_str(&format!(
            "  <urn:c:0002-typed> a <{c_cls}> ; <{lv}> 0, \"0\"^^<http://www.w3.org/2001/XMLSchema#int> ; <{rep}> <urn:r:typed-lv> .\n  <urn:r:typed-lv> <{stat}> \"reviewed\" ; <{rp}> <{main_iri}> ; <{edit_of}> <urn:r:rev-000> .\n"
        ));
        // Language-tagged status literals: `"unreviewed"@en` must not act as a prior
        // admitted status on the `"reviewed"` scan, and `"reviewed"@en` alone must not
        // admit a report under any anchor family.
        trig.push_str(&format!(
            "  <urn:c:0003-lang-ok> a <{c_cls}> ; <{lv}> 0 ; <{rep}> <urn:r:lang-plus-plain> .\n  <urn:r:lang-plus-plain> <{stat}> \"unreviewed\"@en, \"reviewed\" ; <{rp}> <{main_iri}> .\n"
        ));
        trig.push_str(&format!(
            "  <urn:c:0004-lang-only> a <{c_cls}> ; <{lv}> 0 ; <{rep}> <urn:r:lang-only> .\n  <urn:r:lang-only> <{stat}> \"reviewed\"@en ; <{rp}> <{main_iri}> .\n"
        ));
        // 24 filler eligible level-0 communities so dup-a and dup-b span multiple batches.
        for i in 0..24 {
            trig.push_str(&format!(
                "  <urn:c:0010-fill-{i:02}> a <{c_cls}> ; <{lv}> 0 ; <{rep}> <urn:r:fill-{i:02}> .\n  <urn:r:fill-{i:02}> <{stat}> \"reviewed\" ; <{rp}> <{main_iri}> .\n"
            ));
        }
        // Excluded reports: other policy, rejected, failed, other level, and 120 retained
        // unlinked same-policy revisions (exceeding a 100-row budget).
        trig.push_str(&format!(
            "  <urn:c:other-p> a <{c_cls}> ; <{lv}> 0 ; <{rep}> <urn:r:other-p> .\n  <urn:r:other-p> <{stat}> \"reviewed\" ; <{rp}> <{other_iri}> .\n"
        ));
        trig.push_str(&format!(
            "  <urn:c:rej> a <{c_cls}> ; <{lv}> 0 ; <{rep}> <urn:r:rej> .\n  <urn:r:rej> <{stat}> \"rejected\" ; <{rp}> <{main_iri}> .\n"
        ));
        trig.push_str(&format!(
            "  <urn:c:fail> a <{c_cls}> ; <{lv}> 0 ; <{rep}> <urn:r:fail> .\n  <urn:r:fail> <{stat}> \"failed\" ; <{rp}> <{main_iri}> .\n"
        ));
        trig.push_str(&format!(
            "  <urn:c:lvl1> a <{c_cls}> ; <{lv}> 1 ; <{rep}> <urn:r:lvl1> .\n  <urn:r:lvl1> <{stat}> \"reviewed\" ; <{rp}> <{main_iri}> .\n"
        ));
        for i in 0..120 {
            trig.push_str(&format!(
                "  <urn:r:rev-{i:03}> <{stat}> \"reviewed\" ; <{rp}> <{main_iri}> .\n"
            ));
        }
        trig.push_str("}\n");
        ds.store
            .load(&[sparkles::io::Source::from_bytes(
                trig.into_bytes(),
                sparkles::io::RdfFormat::TriG,
                None,
            )])
            .unwrap();

        let mut expected_policied: Vec<String> = vec![
            "urn:r:dup-comm".into(),
            "urn:r:lang-plus-plain".into(),
            "urn:r:multi-stat".into(),
            "urn:r:typed-lv".into(),
        ];
        for i in 0..24 {
            expected_policied.push(format!("urn:r:fill-{i:02}"));
        }
        expected_policied.sort();

        let mut expected_unpolicied = expected_policied.clone();
        expected_unpolicied.push("urn:r:other-p".into());
        expected_unpolicied.sort();

        let cursor_opts = report_cursor_opts();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let opts_open = sparkles::sparql::QueryOptions {
            deadline: Some(deadline),
            ..sparkles::sparql::QueryOptions::default()
        };
        let opts_tight = sparkles::sparql::QueryOptions {
            max_rows: Some(100),
            deadline: Some(deadline),
            ..sparkles::sparql::QueryOptions::default()
        };
        let timed_prep = sparkles::access::Preparation::new(
            sparkles::task::Control {
                deadline: Some(deadline),
                ..sparkles::task::Control::none()
            },
            64 << 20,
        )
        .unwrap();

        for bound_source in [false, true] {
            let make_reader = |opts: sparkles::sparql::QueryOptions| -> Reader {
                if bound_source {
                    let view = ds
                        .dataset
                        .store()
                        .evidence_view(&sparkles::history::At::Head, &timed_prep)
                        .unwrap();
                    Reader::from_evidence(view, ds.clone(), opts, deadline, false)
                } else {
                    Reader::from_snapshot(ds.store.snapshot(), opts, deadline, false)
                }
            };

            let r_open = make_reader(opts_open.clone());
            for (filter, expected) in [
                (
                    Filter {
                        reviewed_only: false,
                        review_policy_iri: Some(main_iri),
                    },
                    &expected_policied,
                ),
                (
                    Filter {
                        reviewed_only: false,
                        review_policy_iri: None,
                    },
                    &expected_unpolicied,
                ),
            ] {
                // Verify every single candidate anchor family individually delivers every
                // eligible report strictly once (no duplicates, no excluded reports).
                for anchor in candidate_anchor_plans(REPORTS, 0, filter).unwrap() {
                    let mut delivered: Vec<String> = Vec::new();
                    enumerate_eligible_reports(
                        &r_open,
                        REPORTS,
                        0,
                        filter,
                        &anchor,
                        &cursor_opts,
                        &mut |batch| {
                            delivered.extend(batch.iter().map(|n| n.as_str().to_string()));
                            Ok(())
                        },
                    )
                    .unwrap();
                    assert_eq!(
                        delivered.len(),
                        expected.len(),
                        "duplicate or missing report delivery (bound_source={bound_source}): {delivered:?}"
                    );
                    delivered.sort();
                    assert_eq!(&delivered, expected);
                }

                // Under `max_rows = 100` (< 120 retained revisions), narrowest-anchor
                // selection eliminates the broader Policy/Status scans and succeeds via
                // the Level anchor with exact single delivery.
                let r_tight = make_reader(opts_tight.clone());
                let winner = select_narrowest_anchor(
                    &r_tight,
                    candidate_anchor_plans(REPORTS, 0, filter).unwrap(),
                    &cursor_opts,
                )
                .unwrap()
                .expect("expected non-empty winning anchor");
                assert!(matches!(winner, AnchorPlan::Level { .. }));
                let mut delivered: Vec<String> = Vec::new();
                enumerate_eligible_reports(
                    &r_tight,
                    REPORTS,
                    0,
                    filter,
                    &winner,
                    &cursor_opts,
                    &mut |batch| {
                        delivered.extend(batch.iter().map(|n| n.as_str().to_string()));
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(delivered.len(), expected.len());
                delivered.sort();
                assert_eq!(&delivered, expected);
            }
        }
    }

    #[test]
    fn select_narrowest_anchor_eliminates_budget_failing_earlier_racer() {
        const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
        let gr = crate::graphrag::guard::gr;
        let (c_cls, lv, mem, rep, stat, rp, cites_rep) = (
            gr("Community"),
            gr("level"),
            gr("member"),
            gr("report"),
            gr("status"),
            gr("reviewPolicy"),
            gr("citesReport"),
        );
        let main_iri = "urn:ev:policy:main";
        let other_iri = "urn:ev:policy:other";

        let st = crate::state::AppState::standalone(
            Default::default(),
            std::time::Duration::from_secs(30),
        );
        let ds = st.attach("race", crate::state::DbType::Mem, None).unwrap();
        let prep =
            sparkles::access::Preparation::new(sparkles::task::Control::none(), 64 << 20).unwrap();
        ds.dataset
            .store()
            .activate_memory_configuration_history(&prep)
            .unwrap();

        // 120 level-0 communities (100 under `main`, 20 under `other`, all `"reviewed"`):
        // under `max_rows = 100` with `REPORT_BATCH = 16`, Turn 7 causes Racer 0 (`Level`,
        // 120 rows) and Racer 2 (`Status`, 120 rows) to exceed `max_rows` and be
        // eliminated while Racer 1 (`Policy`, 100 rows) returns rows 97..=100 and needs
        // Turn 8 to hit EOF. Attach > 100 `gr:member` and `gr:citesReport` triples to
        // verify predicate-pinned lookups stay within the same 100-row cap.
        let mut trig = format!("<{REPORTS}> {{\n");
        let mut expected: Vec<String> = Vec::with_capacity(100);
        for i in 0..100 {
            trig.push_str(&format!(
                "  <urn:c:main-{i:03}> a <{c_cls}> ; <{lv}> 0 ; <{rep}> <urn:r:main-{i:03}> .\n  <urn:r:main-{i:03}> <{stat}> \"reviewed\" ; <{rp}> <{main_iri}> .\n"
            ));
            expected.push(format!("urn:r:main-{i:03}"));
        }
        for i in 0..150 {
            trig.push_str(&format!(
                "  <urn:c:main-000> <{mem}> <urn:ent:m-{i:03}> .\n  <urn:r:main-000> <{cites_rep}> <urn:r:cited-{i:03}> .\n"
            ));
        }
        for i in 0..20 {
            trig.push_str(&format!(
                "  <urn:c:other-{i:03}> a <{c_cls}> ; <{lv}> 0 ; <{rep}> <urn:r:other-{i:03}> .\n  <urn:r:other-{i:03}> <{stat}> \"reviewed\" ; <{rp}> <{other_iri}> .\n"
            ));
        }
        trig.push_str("}\n");
        ds.store
            .load(&[sparkles::io::Source::from_bytes(
                trig.into_bytes(),
                sparkles::io::RdfFormat::TriG,
                None,
            )])
            .unwrap();
        expected.sort();

        let filter = Filter {
            reviewed_only: false,
            review_policy_iri: Some(main_iri),
        };
        let cursor_opts = report_cursor_opts();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let opts_100 = sparkles::sparql::QueryOptions {
            max_rows: Some(100),
            deadline: Some(deadline),
            ..sparkles::sparql::QueryOptions::default()
        };
        let opts_50 = sparkles::sparql::QueryOptions {
            max_rows: Some(50),
            deadline: Some(deadline),
            ..sparkles::sparql::QueryOptions::default()
        };
        let timed_prep = sparkles::access::Preparation::new(
            sparkles::task::Control {
                deadline: Some(deadline),
                ..sparkles::task::Control::none()
            },
            64 << 20,
        )
        .unwrap();

        for bound_source in [false, true] {
            let make_reader = |opts: sparkles::sparql::QueryOptions| -> Reader {
                if bound_source {
                    let view = ds
                        .dataset
                        .store()
                        .evidence_view(&sparkles::history::At::Head, &timed_prep)
                        .unwrap();
                    Reader::from_evidence(view, ds.clone(), opts, deadline, false)
                } else {
                    Reader::from_snapshot(ds.store.snapshot(), opts, deadline, false)
                }
            };

            let r_100 = make_reader(opts_100.clone());
            let winner = select_narrowest_anchor(
                &r_100,
                candidate_anchor_plans(REPORTS, 0, filter).unwrap(),
                &cursor_opts,
            )
            .unwrap()
            .expect("expected policy anchor to survive elimination of Level and Status");
            assert!(
                matches!(
                    &winner,
                    AnchorPlan::Report { scans } if scans.len() == 1 && scans[0].status_scan.is_none()
                ),
                "expected Policy anchor to win after Level elimination (bound_source={bound_source})"
            );

            let mut delivered: Vec<String> = Vec::new();
            enumerate_eligible_reports(
                &r_100,
                REPORTS,
                0,
                filter,
                &winner,
                &cursor_opts,
                &mut |batch| {
                    delivered.extend(batch.iter().map(|n| n.as_str().to_string()));
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(delivered.len(), expected.len());
            delivered.sort();
            assert_eq!(&delivered, &expected);

            // When every candidate exceeds `max_rows`, the race returns a row-budget error.
            let r_50 = make_reader(opts_50.clone());
            let err = match select_narrowest_anchor(
                &r_50,
                candidate_anchor_plans(REPORTS, 0, filter).unwrap(),
                &cursor_opts,
            ) {
                Err(e) => e,
                Ok(_) => panic!("expected row-budget error when all racers exceed max_rows=50"),
            };
            assert!(is_row_budget_error(&err), "unexpected error: {err:?}");
        }
    }
}
