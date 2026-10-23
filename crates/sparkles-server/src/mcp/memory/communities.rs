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
use super::recall::rank_terms;
use super::text::plain_query;
use crate::graphrag::read::{self, Filter, ReportRef};
use crate::mcp::Outcome;
use crate::mcp::errors::{ErrorContext, ToolError};
use crate::mcp::tools::{Tools, bounded, parse};
use oxrdf::NamedNode;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

/// The most reports one call returns.
pub(crate) const MAX_LIMIT: u64 = 100;

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

/// The JSON form of a report.
pub(crate) fn report_json(r: &ReportRef) -> Value {
    let mut v = json!({
        "iri": r.iri,
        "community": r.community,
        "level": r.level,
        "title": r.title,
        "summary": r.summary,
        "status": r.status.as_str(),
        "rating": r.rating,
        "memberCount": r.member_count,
        "findings": r.findings.iter().map(|f| json!({
            "iri": f.iri,
            "text": f.text,
            "cites": f.cites,
        })).collect::<Vec<_>>(),
    });
    if let Some(s) = r.score {
        v["score"] = json!(s);
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
        let r = self.reader(&ds, None, None, Some(false), deadline, &ctx)?;
        let filter = Filter {
            reviewed_only: a.reviewed_only,
        };
        let sel = self.select_reports(
            &ds,
            &r,
            query,
            level,
            filter,
            limit,
            a.findings.unwrap_or(true),
            &ctx,
        )?;
        let out = match sel {
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
            } => json!({
                "dataset": ds.name,
                "available": true,
                "reportsGraph": graph,
                "level": level,
                "levels": levels.iter().map(|(l, n)| json!({ "level": l, "reports": n })).collect::<Vec<_>>(),
                "ranking": ranking,
                "stale": reports.iter().filter(|r| r.status == crate::graphrag::store::Status::Stale).count(),
                "reports": reports.iter().map(report_json).collect::<Vec<_>>(),
            }),
        };
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
        filter: Filter,
        limit: usize,
        findings: bool,
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
        if !read::readable(&r.snap, &opts, &graph) {
            return Ok(Selection::Unavailable(
                "unavailable",
                "the community reports are not available to you, because your access does not cover everything their build read".into(),
            ));
        }
        let counts = read::level_counts(&r.snap, &opts, &graph, filter).map_err(eng)?;
        let Some(level) = level.or_else(|| read::choose_level(&counts, read::DEFAULT_MAX_REPORTS))
        else {
            return Ok(Selection::Unavailable(
                "no-reports",
                if filter.reviewed_only {
                    "no reviewed community report exists".into()
                } else {
                    "no community report exists".into()
                },
            ));
        };
        let levels: Vec<(u32, u64)> = counts.iter().map(|(l, n)| (*l, *n)).collect();
        let mut ranking = "rating";
        let mut reports = Vec::new();
        if let Some(q) = query {
            let hits = self.report_hits(ds, r, q, &graph).map_err(eng)?;
            if !hits.is_empty() {
                let names: Vec<String> = hits.iter().map(|(r, _)| r.clone()).collect();
                let found =
                    read::reports_named(&r.snap, &opts, &graph, &names, filter).map_err(eng)?;
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
                ranking = "search";
            }
        }
        // reports the search did not find follow, best rated first
        if reports.len() < limit {
            let rest = read::reports_at(
                &r.snap,
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
            read::with_findings(&r.snap, &opts, &graph, &mut reports).map_err(eng)?;
        }
        Ok(Selection::Reports {
            graph,
            level,
            levels,
            ranking,
            reports,
        })
    }

    /// The reports a search for `q` finds in `graph`, best first, with their fused
    /// scores. A hit on a finding counts for its report. A dataset without a usable
    /// index finds none.
    fn report_hits(
        &self,
        ds: &crate::state::Dataset,
        r: &Reader,
        q: &str,
        graph: &str,
    ) -> Result<Vec<(String, f64)>, sparkles::error::Error> {
        let graphs = [NamedNode::new_unchecked(graph)];
        let mut lists: Vec<HashMap<String, usize>> = Vec::new();
        if ds.dataset.indexes().text().enabled()
            && let Some(plain) = plain_query(q)
        {
            let preds: Vec<NamedNode> = match ds
                .dataset
                .indexes()
                .text()
                .status()
                .map(|s| s.config.predicates)
            {
                Some(sparkles::text::PredicateSet::Only(p)) => p
                    .iter()
                    .map(|p| NamedNode::new_unchecked(p.as_str()))
                    .collect(),
                _ => Vec::new(),
            };
            match self.text_hits(r, &preds, &plain, &graphs) {
                Ok(h) => lists.push(rank_terms(h.into_iter().map(|(s, sc, _)| (s, sc)))),
                Err(e) if unavailable(&e) => {}
                Err(e) => return Err(e),
            }
        }
        // every index that may embed report text, each its own ranked list
        for pred in report_embeddings(ds) {
            match self.vector_hits(r, &pred, q, &graphs) {
                Ok(h) => lists.push(rank_terms(h.into_iter())),
                Err(e) if unavailable(&e) => {}
                Err(e) => return Err(e),
            }
        }
        let mut fused: HashMap<String, f64> = HashMap::new();
        for l in &lists {
            // a report's best subject counts once per list
            let mut best: HashMap<String, usize> = HashMap::new();
            for (s, rank) in l {
                let rep = read::report_of_finding(s).unwrap_or(s).to_string();
                let e = best.entry(rep).or_insert(*rank);
                if *rank < *e {
                    *e = *rank;
                }
            }
            for (rep, rank) in best {
                *fused.entry(rep).or_default() += 1.0 / (super::RRF_K + rank as f64);
            }
        }
        let mut out: Vec<(String, f64)> = fused.into_iter().collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
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
