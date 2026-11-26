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
                    .reports_named(&opts, &graph, &names, filter)
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
                .reports_at(
                    &opts,
                    &graph,
                    Some(level),
                    filter,
                    read::DEFAULT_MAX_REPORTS.max(limit),
                )
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
        if let Some(hits) = self.eligible_text_hits(ds, r, q, graph, &report_pat, depth)?
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
    /// matched by `report_pat` in `graph`, gated on the dataset's full-text index being
    /// enabled, ready, and covering `graph`.
    ///
    /// Uses the index's configured `maxTextBytes` prefix and default accent-folding
    /// analyzer (`StandardAnalyzer`), streams eligible reports in bounded batches over
    /// the same authorized read view in two passes (corpus stats, then per-report best
    /// score into a bounded top-`depth` heap), and never materializes all eligible
    /// literals or all matching reports in memory.
    fn eligible_text_hits(
        &self,
        ds: &crate::state::Dataset,
        r: &Reader,
        q: &str,
        graph: &str,
        report_pat: &str,
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
        let pred_in = preds
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let max_text_bytes = status.config.max_text_bytes;
        let finding = crate::graphrag::guard::gr("finding");
        let reports_sparql =
            format!("SELECT DISTINCT ?r WHERE {{ GRAPH <{graph}> {{ {report_pat} }} }}");
        let q_index: HashMap<&str, usize> = q_tokens
            .iter()
            .enumerate()
            .map(|(i, t)| (t.as_str(), i))
            .collect();

        const REPORT_BATCH: usize = 16;
        const FINDING_BATCH: usize = 32;
        const LITERAL_BATCH_ROWS: usize = 64;
        const K1: f64 = 1.2;
        const B: f64 = 0.75;

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

        let report_cursor_opts = sparkles::sparql::CursorOptions {
            batch_rows: REPORT_BATCH,
            batch_bytes: 256 << 10,
            fallback: sparkles::sparql::FallbackPolicy::AllowMaterialization,
        };
        let lit_cursor_opts = sparkles::sparql::CursorOptions {
            batch_rows: LITERAL_BATCH_ROWS,
            batch_bytes: 256 << 10,
            fallback: sparkles::sparql::FallbackPolicy::AllowMaterialization,
        };

        let lit_filter = format!(
            "FILTER(?p IN ({pred_in}) && isLiteral(?lit) && (DATATYPE(?lit) = <http://www.w3.org/2001/XMLSchema#string> || DATATYPE(?lit) = <http://www.w3.org/1999/02/22-rdf-syntax-ns#langString>))"
        );

        type SubjectLitFn<'a> = &'a mut dyn FnMut(&str, &str) -> Result<(), sparkles::error::Error>;
        type BatchLitFn<'a> = &'a mut dyn FnMut(usize, &str) -> Result<(), sparkles::error::Error>;
        type ReportBatchFn<'a> =
            &'a mut dyn FnMut(&[NamedNode]) -> Result<(), sparkles::error::Error>;

        // Ground each subject directly in a balanced UNION (depth <= 5) so every read is
        // a bounded GSPO point lookup for those subjects rather than a full predicate
        // scan that could exceed `max_rows` on large graphs.
        let stream_subject_literals = |subjects: &[NamedNode],
                                       on_lit: SubjectLitFn<'_>|
         -> Result<(), sparkles::error::Error> {
            if subjects.is_empty() {
                return Ok(());
            }
            let branches: Vec<String> = subjects
                .iter()
                .map(|s| format!("BIND({s} AS ?s) {s} ?p ?lit ."))
                .collect();
            let sparql = format!(
                "SELECT ?s ?lit WHERE {{ GRAPH <{graph}> {{ {{ {} }} {lit_filter} }} }}",
                balanced_union(&branches),
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
            Ok(())
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
                    .map(|rep| format!("BIND({rep} AS ?r) {rep} <{finding}> ?f ."))
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
                let mut cursor = r.select_cursor(&reports_sparql, &report_cursor_opts)?;
                let mut reps: Vec<NamedNode> = Vec::with_capacity(REPORT_BATCH);
                while let Some(batch) = cursor.next_batch()? {
                    reps.clear();
                    for row in 0..batch.len() {
                        if let Some(owned) = batch.term(row, 0)?
                            && let Term::NamedNode(rep) = owned.as_term()
                        {
                            reps.push(rep.clone());
                        }
                    }
                    drop(batch);
                    if !reps.is_empty() {
                        on_batch(&reps)?;
                    }
                }
                Ok(())
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
}
