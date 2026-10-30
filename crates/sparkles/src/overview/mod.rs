//! The graph overview (spec U06): the instances of a class, or of a group from an
//! earlier answer, grouped by the communities F14 wrote back, by a property's values or
//! ranges, or listed as members, with the counted edges between the groups, to their
//! sibling groups and to the other classes. The entities of the whole dataset can also be
//! grouped by the community hierarchy that a GraphRAG build stored in its reports graph
//! (spec C22 §4), one level at a time, with each group's report title and status. The
//! facets of a set (spec U11 §3.2) count a class, a list of nodes or a query's values by
//! ranges or values of literal properties, with brushes that filter each other.
//!
//! Every count comes from SPARQL queries run with the caller's [`QueryOptions`], so the
//! view, the budgets, the deadline and the cancellation of `opts` apply to all of them
//! and an answer never holds what a query with the same options could not read. A group
//! id encodes the class and the partitions that lead to the group, so a later request
//! can name it without state. [`Dataset::graph`](crate::Dataset::graph) is the entry
//! point, and [`groups_at`] answers at a snapshot the caller already holds.

use crate::error::{Error, Result};
use crate::history::At;
use crate::sparql::QueryOptions;
use crate::store::Snapshot;
use serde_json::Value as J;
use std::sync::Arc;

pub(crate) mod facets;
mod groups;
mod ids;
mod reports;

pub use facets::{
    Brush, DEFAULT_BINS, FacetSet, FacetSpec, FacetsRequest, Kind as FacetKind, MAX_BINS,
    MAX_FACETS, MAX_FILTERS, MAX_NODES as MAX_FACET_NODES, MAX_SET, MAX_TOP, brush_condition,
    facets_at, number_text, rdf_term_from_json, round_edges,
};
pub use groups::{DEFAULT_BUCKETS, MAX_BUCKETS, MAX_EDGES, MAX_LIMIT};
pub use ids::MAX_DEPTH;
pub use reports::{DEFAULT_REPORTS_GRAPH, DISCLOSURE_GRAPH, GUARDED_PREFIX, MAX_REPORT_LEVEL};

/// What a request groups and how.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupsRequest {
    pub within: Within,
    pub by: By,
    pub limit: usize,
    pub at: Option<At>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Within {
    Class(String),
    Group(String),
    /// the entities of the whole dataset, which only [`By::Reports`] groups
    Dataset,
}

#[derive(Clone, Debug, PartialEq)]
pub enum By {
    /// communities if F14 wrote some for the set, else the best property, else members
    Auto,
    Members,
    Community(String),
    Property {
        predicate: String,
        buckets: usize,
    },
    /// the community hierarchy of a GraphRAG reports graph (spec C22 §4)
    Reports(ReportsBy),
}

/// The GraphRAG grouping. `graph` is the reports graph, [`DEFAULT_REPORTS_GRAPH`] when
/// `None`, and a server fills in the one its settings name. `level` is the level the
/// dataset opens at, 0 (the coarsest) when `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReportsBy {
    pub graph: Option<String>,
    pub level: Option<u32>,
}

fn bad(msg: impl Into<String>) -> Error {
    Error::invalid(msg)
}

fn iri_field(v: &J, what: &str) -> Result<String> {
    let s = v
        .as_str()
        .ok_or_else(|| bad(format!("{what} must be an IRI string")))?;
    let s = s
        .strip_prefix('<')
        .and_then(|i| i.strip_suffix('>'))
        .unwrap_or(s);
    oxrdf::NamedNode::new(s).map_err(|e| bad(format!("{what}: invalid IRI '{s}': {e}")))?;
    Ok(s.to_string())
}

/// The `reports` member of a `by`: `{}`, or `{"level": n, "graph": IRI}`.
fn reports_by(v: &J) -> Result<ReportsBy> {
    let o = match v {
        J::Null => return Ok(ReportsBy::default()),
        J::Object(o) => o,
        _ => return Err(bad("by.reports must be an object such as {\"level\": 0}")),
    };
    for k in o.keys() {
        if !matches!(k.as_str(), "level" | "graph") {
            return Err(bad(format!(
                "unknown member '{k}' of by.reports: level or graph"
            )));
        }
    }
    let level = match o.get("level") {
        None | Some(J::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .filter(|n| *n <= u64::from(MAX_REPORT_LEVEL))
                .ok_or_else(|| {
                    bad(format!(
                        "by.reports.level must be an integer from 0 to {MAX_REPORT_LEVEL}"
                    ))
                })? as u32,
        ),
    };
    let graph = match o.get("graph") {
        None | Some(J::Null) => None,
        Some(v) => {
            let g = iri_field(v, "by.reports.graph")?;
            reports::check_graph(&g).map_err(bad)?;
            Some(g)
        }
    };
    Ok(ReportsBy { graph, level })
}

impl GroupsRequest {
    /// Parse and check a request body.
    pub fn from_json(j: &J) -> Result<GroupsRequest> {
        let o = j
            .as_object()
            .ok_or_else(|| bad("the request must be a JSON object"))?;
        for k in o.keys() {
            if !matches!(k.as_str(), "within" | "by" | "limit" | "at") {
                return Err(bad(if k == "branch" {
                    "name the branch with ?branch=, as on the other routes".to_string()
                } else {
                    format!("unknown member '{k}': within, by, limit or at")
                }));
            }
        }
        let within = match o.get("within") {
            None | Some(J::Null) => Within::Dataset,
            Some(J::Object(w)) if w.len() == 1 && w.contains_key("class") => {
                Within::Class(iri_field(&w["class"], "within.class")?)
            }
            Some(J::Object(w)) if w.len() == 1 && w.contains_key("group") => {
                let g = w["group"]
                    .as_str()
                    .ok_or_else(|| bad("within.group must be a group id string"))?;
                Within::Group(g.to_string())
            }
            _ => {
                return Err(bad(
                    "within must be {\"class\": IRI} or {\"group\": id from an earlier answer}, or absent with by {\"reports\": {}}",
                ));
            }
        };
        let by = match o.get("by") {
            None | Some(J::Null) => By::Auto,
            Some(J::String(s)) if s == "auto" => By::Auto,
            Some(J::String(s)) if s == "members" => By::Members,
            Some(J::Object(b)) if b.len() == 1 && b.contains_key("community") => {
                By::Community(iri_field(&b["community"], "by.community")?)
            }
            Some(J::Object(b))
                if b.contains_key("property")
                    && b.keys().all(|k| k == "property" || k == "buckets") =>
            {
                let buckets = match b.get("buckets") {
                    None | Some(J::Null) => DEFAULT_BUCKETS,
                    Some(v) => v
                        .as_u64()
                        .filter(|n| (2..=MAX_BUCKETS as u64).contains(n))
                        .ok_or_else(|| {
                            bad(format!(
                                "by.buckets must be an integer from 2 to {MAX_BUCKETS}"
                            ))
                        })? as usize,
                };
                By::Property {
                    predicate: iri_field(&b["property"], "by.property")?,
                    buckets,
                }
            }
            Some(J::Object(b)) if b.len() == 1 && b.contains_key("reports") => {
                By::Reports(reports_by(&b["reports"])?)
            }
            _ => {
                return Err(bad(
                    "by must be \"auto\", \"members\", {\"community\": IRI}, {\"property\": IRI, \"buckets\": n} or {\"reports\": {\"level\": n}}",
                ));
            }
        };
        check_pair(&within, &by)?;
        let limit = match o.get("limit") {
            None | Some(J::Null) => MAX_LIMIT,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=MAX_LIMIT as u64).contains(n))
                .ok_or_else(|| bad(format!("limit must be an integer from 1 to {MAX_LIMIT}")))?
                as usize,
        };
        let at = match o.get("at") {
            None | Some(J::Null) => None,
            Some(J::String(s)) => Some(s.parse::<At>().map_err(|e| bad(format!("at: {e}")))?)
                .filter(|a| *a != At::Head),
            Some(_) => return Err(bad("at must be a string: a commit, a time or a snapshot")),
        };
        Ok(GroupsRequest {
            within,
            by,
            limit,
            at,
        })
    }

    /// Whether the request reads a GraphRAG reports graph: it groups by reports, or
    /// opens a group of an earlier reports answer.
    pub fn reads_reports(&self) -> bool {
        matches!(self.by, By::Reports(_))
            || matches!(&self.within, Within::Group(g) if reports::is_reports_id(g))
    }

    /// Refuse a request that reads GraphRAG reports on a branch other than `main`. The
    /// reports belong to main, whose builds wrote them at their build commits (spec C22
    /// §6.5), so on another branch they would group that branch's data by main's
    /// communities.
    pub fn check_branch(&self, branch: &str) -> Result<()> {
        if self.reads_reports() && branch != "main" {
            return Err(bad(format!(
                "GraphRAG communities are read on main, where their reports are built, and this request reads the branch '{branch}'"
            )));
        }
        Ok(())
    }

    /// Fill in the reports graph a server's settings name, when the request groups the
    /// whole dataset by reports without naming one. A group id names its own graph, so a
    /// request that opens a group keeps it even when the settings have moved since.
    pub fn default_reports_graph(&mut self, graph: &str) {
        if matches!(self.within, Within::Dataset)
            && let By::Reports(r) = &mut self.by
            && r.graph.is_none()
        {
            r.graph = Some(graph.to_string());
        }
    }

    /// The request as a cache key: every member, in a fixed order.
    pub fn key(&self) -> String {
        format!("{self:?}")
    }
}

/// Refuse a `within` and a `by` that do not go together.
fn check_pair(within: &Within, by: &By) -> Result<()> {
    match (within, by) {
        (Within::Dataset, By::Reports(_)) => Ok(()),
        (Within::Dataset, _) => Err(bad(
            "within is required, except with by {\"reports\": {}}, which groups the whole dataset",
        )),
        (Within::Class(_), By::Reports(_)) => Err(bad(
            "by reports groups the whole dataset: leave out within, or name a group of an earlier reports answer",
        )),
        (Within::Group(g), by) if reports::is_reports_id(g) => match by {
            By::Auto | By::Members => Ok(()),
            By::Reports(r) if r.level.is_none() => Ok(()),
            By::Reports(_) => Err(bad(
                "by.reports.level chooses the level the dataset opens at, and a group opens into the level below it",
            )),
            _ => Err(bad(
                "a group of the GraphRAG communities opens by \"auto\", \"members\" or {\"reports\": {}}",
            )),
        },
        (Within::Group(_), By::Reports(_)) => Err(bad(
            "by reports opens only the groups of an earlier reports answer",
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
thread_local! {
    // The relative timeout and absolute deadline of each overview query on this thread.
    static QUERY_BOUNDS: std::cell::RefCell<Vec<(Option<std::time::Duration>, Option<std::time::Instant>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// `opts` with its relative timeout turned into one absolute deadline, the earlier of
/// that timeout from now and `opts.deadline`. An overview runs many queries, and each
/// query would otherwise start the timeout again.
pub(crate) fn one_deadline(opts: &QueryOptions) -> Result<std::borrow::Cow<'_, QueryOptions>> {
    let Some(timeout) = opts.timeout else {
        return Ok(std::borrow::Cow::Borrowed(opts));
    };
    let relative = std::time::Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| Error::invalid("query timeout is too large"))?;
    let mut anchored = opts.clone();
    anchored.timeout = None;
    anchored.deadline = Some(opts.deadline.map_or(relative, |d| d.min(relative)));
    Ok(std::borrow::Cow::Owned(anchored))
}

/// The answer to `req` at snapshot `snap`, as the JSON object that API.md describes
/// under "Graph overview", with the snapshot's `commit`. `req.at` is not read: the
/// caller chose the snapshot, and with it the branch, which
/// [`GroupsRequest::check_branch`] checks for a request that reads reports.
pub fn groups_at(snap: Arc<Snapshot>, req: &GroupsRequest, opts: &QueryOptions) -> Result<J> {
    let opts = &*one_deadline(opts)?;
    let commit = snap.commit;
    let run = groups::Run { snap, opts };
    let mut answer = if req.reads_reports() {
        reports::answer(&run, req)?
    } else {
        run.answer(req)?
    };
    if let Some(o) = answer.as_object_mut() {
        o.insert("commit".into(), commit.into());
    }
    Ok(answer)
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    use std::time::{Duration, Instant};

    type Bounds = Vec<(Option<Duration>, Option<Instant>)>;

    /// The bounds of every query `run` makes, and the instants around the call.
    fn recorded(run: impl FnOnce() -> Result<J>) -> (Bounds, Instant, Instant) {
        QUERY_BOUNDS.with(|b| b.borrow_mut().clear());
        let before = Instant::now();
        run().unwrap();
        let after = Instant::now();
        let bounds = QUERY_BOUNDS.with(|b| std::mem::take(&mut *b.borrow_mut()));
        (bounds, before, after)
    }

    /// Every query has no relative timeout and the same deadline, within `range`.
    fn assert_one_deadline(bounds: &Bounds, range: std::ops::RangeInclusive<Instant>) {
        assert!(
            bounds.len() >= 2,
            "the fixture needs several overview queries"
        );
        let deadline = bounds[0].1.expect("an absolute deadline");
        assert!(range.contains(&deadline));
        for &(timeout, each) in bounds {
            assert_eq!(timeout, None, "a query received the relative timeout again");
            assert_eq!(each, Some(deadline), "queries received different deadlines");
        }
    }

    fn dataset() -> crate::Dataset {
        let ds = crate::Dataset::memory();
        ds.update(
            "INSERT DATA { <urn:a> a <urn:T> ; <urn:p> 1 ; <urn:r> <urn:x> ; <urn:q> <urn:b> . \
             <urn:c> a <urn:T> ; <urn:p> 2 ; <urn:r> <urn:y> . <urn:b> a <urn:U> }",
        )
        .unwrap();
        ds
    }

    #[test]
    fn every_overview_query_shares_one_absolute_deadline() {
        let ds = dataset();
        let timeout = Duration::from_secs(3600);
        let opts = QueryOptions {
            timeout: Some(timeout),
            no_cache: true,
            ..Default::default()
        };
        let groups = GroupsRequest::from_json(
            &serde_json::json!({"within": {"class": "urn:T"}, "by": {"property": "urn:r"}}),
        )
        .unwrap();
        let facets = FacetsRequest::from_json(&serde_json::json!({
            "within": {"class": "urn:T"},
            "facets": [{"property": "urn:p"}],
            "filters": [{"property": "urn:p", "range": [0, 3]}],
        }))
        .unwrap();
        let snap = ds.store().snapshot();
        let runs: [&dyn Fn() -> Result<J>; 4] = [
            &|| ds.graph().groups(&groups, &opts),
            &|| ds.graph().facets(&facets, &opts),
            &|| groups_at(snap.clone(), &groups, &opts),
            &|| facets_at(snap.clone(), &facets, &opts),
        ];
        for run in runs {
            let (bounds, before, after) = recorded(run);
            assert_one_deadline(&bounds, before + timeout..=after + timeout);
        }
        // an earlier caller deadline wins over the relative timeout
        let early = Instant::now() + Duration::from_secs(60);
        let opts = QueryOptions {
            deadline: Some(early),
            ..opts
        };
        let (bounds, _, _) = recorded(|| groups_at(snap.clone(), &groups, &opts));
        assert_one_deadline(&bounds, early..=early);
    }

    #[test]
    fn one_deadline_keeps_options_without_a_timeout_and_refuses_overflow() {
        let opts = QueryOptions::default();
        assert!(matches!(
            one_deadline(&opts).unwrap(),
            std::borrow::Cow::Borrowed(_)
        ));
        let huge = QueryOptions {
            timeout: Some(Duration::MAX),
            ..Default::default()
        };
        assert!(matches!(one_deadline(&huge), Err(Error::Invalid(_))));
    }
}
