//! Reading reports for the consumers of Phase 2 (spec C22 §2.4, §2.5 and §7): global
//! answering in Ask, `community_reports`, the communities of `recall` and the brief.
//!
//! Every function here reads through the caller's query options. The disclosure guard
//! of §6 is part of those options, so a caller whose view does not cover a build's
//! footprint reads no report triple at all, and [`readable`] tells that case apart from
//! a reports graph that is empty. Rejected and failed reports are never used. A stale
//! report is used and counted, so that a consumer can refuse a selection with too many.

use super::guard::gr;
use super::store::{self, Status};
use oxrdf::Term;
use sparkles::error::Error;
use sparkles::sparql::{self, QueryOptions};
use sparkles::store::{BoundReadSource, Snapshot, Store};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// The default for the most reports one global answer reads (§7.2).
pub const DEFAULT_MAX_REPORTS: usize = 100;

/// A finding of a report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindingRef {
    pub iri: String,
    pub order: u64,
    pub text: String,
    /// the entity IRIs the finding is about (`gr:about`)
    pub about: Vec<String>,
    /// the cited facts as N-Triples triple terms, their reifiers and the cited child
    /// findings
    pub cites: Vec<String>,
    /// under a review policy, the IRIs of the claims and decisions it cites
    pub claims: Vec<String>,
    pub decisions: Vec<String>,
}

/// The review policy a report was read under (spec C25, "Reports on accepted
/// evidence").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewPolicyRef {
    pub iri: String,
    pub version: Option<u64>,
    pub configuration: Option<String>,
}

/// A report that a consumer may use.
#[derive(Clone, Debug, PartialEq)]
pub struct ReportRef {
    pub iri: String,
    pub community: String,
    pub level: u32,
    pub title: String,
    pub summary: String,
    pub status: Status,
    pub rating: Option<u64>,
    pub member_count: u64,
    /// the relevance to a search, when one ranked the reports
    pub score: Option<f64>,
    pub findings: Vec<FindingRef>,
    pub review_policy: Option<ReviewPolicyRef>,
}

/// Which reports a consumer uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Filter<'a> {
    /// only reports a person marked reviewed (`reviewedOnly`, A7)
    pub reviewed_only: bool,
    /// only reports read under this review policy IRI (spec C25, item 811)
    pub review_policy_iri: Option<&'a str>,
}

impl Filter<'_> {
    pub fn admits(&self, s: Status) -> bool {
        match s {
            Status::Reviewed => true,
            Status::Unreviewed | Status::Stale => !self.reviewed_only,
            Status::Rejected | Status::Failed => false,
        }
    }

    /// The statuses as a SPARQL `VALUES` block for `?status`.
    pub(crate) fn values(&self) -> String {
        let v: Vec<String> = Status::ALL
            .into_iter()
            .filter(|s| self.admits(*s))
            .map(|s| format!("\"{}\"", s.as_str()))
            .collect();
        format!("VALUES ?status {{ {} }}", v.join(" "))
    }

    /// A required `gr:reviewPolicy` pattern on `?r` when filtering by policy IRI.
    pub(crate) fn policy_clause(&self) -> Result<String, Error> {
        let Some(p) = self.review_policy_iri else {
            return Ok(String::new());
        };
        let node = oxrdf::NamedNode::new(p)
            .map_err(|e| Error::Invalid(format!("reviewPolicy IRI: {e}")))?;
        Ok(format!(" ; <{}> {node}", gr("reviewPolicy")))
    }
}

pub use super::guard::readable;

#[derive(Clone, Copy)]
enum ReadInput<'a> {
    Snapshot(&'a Arc<Snapshot>),
    Source(&'a BoundReadSource, &'a Store),
}

fn rows_in(
    input: ReadInput<'_>,
    opts: &QueryOptions,
    q: &str,
) -> Result<sparql::output::OwnedRows, Error> {
    match input {
        ReadInput::Snapshot(snap) => sparql::query(snap.clone(), q, opts)?.rows(),
        ReadInput::Source(source, store) => {
            let mut source_opts = opts.clone();
            if source_opts.graphs.is_none() {
                source_opts.graphs = Some(Arc::new(sparkles::access::GraphAccess::all()));
            }
            sparql::query_source(source.clone(), store, q, &source_opts)?.rows()
        }
    }
}

fn lit(t: &Option<Term>) -> Option<String> {
    match t {
        Some(Term::Literal(l)) => Some(l.value().to_string()),
        _ => None,
    }
}

fn iri(t: &Option<Term>) -> Option<String> {
    match t {
        Some(Term::NamedNode(n)) => Some(n.as_str().to_string()),
        _ => None,
    }
}

fn int(t: &Option<Term>) -> Option<u64> {
    lit(t).and_then(|v| v.parse().ok())
}

/// The number of usable reports at each level.
pub fn level_counts(
    snap: &Arc<Snapshot>,
    opts: &QueryOptions,
    graph: &str,
    filter: Filter<'_>,
) -> Result<BTreeMap<u32, u64>, Error> {
    level_counts_in(ReadInput::Snapshot(snap), opts, graph, filter)
}

/// [`level_counts`] evaluated over a [`BoundReadSource`] without extracting a raw snapshot.
pub fn level_counts_source(
    source: &BoundReadSource,
    store: &Store,
    opts: &QueryOptions,
    graph: &str,
    filter: Filter<'_>,
) -> Result<BTreeMap<u32, u64>, Error> {
    level_counts_in(ReadInput::Source(source, store), opts, graph, filter)
}

fn level_counts_in(
    input: ReadInput<'_>,
    opts: &QueryOptions,
    graph: &str,
    filter: Filter<'_>,
) -> Result<BTreeMap<u32, u64>, Error> {
    let policy_clause = filter.policy_clause()?;
    let q = format!(
        "SELECT ?l (COUNT(?r) AS ?n) WHERE {{ {values} GRAPH <{graph}> {{ ?c a <{community}> ; <{level}> ?l ; <{report}> ?r . ?r <{status}> ?status{policy_clause} }} }} GROUP BY ?l",
        values = filter.values(),
        community = gr("Community"),
        level = gr("level"),
        report = gr("report"),
        status = gr("status"),
    );
    let mut out = BTreeMap::new();
    for r in rows_in(input, opts, &q)? {
        let r = r.values();
        if let (Some(l), Some(n)) = (int(&r[0]), int(&r[1])) {
            out.insert(l as u32, n);
        }
    }
    Ok(out)
}

/// The level a global answer reads: the finest level with at most `max` usable
/// reports, or the coarsest level when every level has more (§7.2, §13 question 1).
pub fn choose_level(counts: &BTreeMap<u32, u64>, max: usize) -> Option<u32> {
    counts
        .iter()
        .rev()
        .find(|(_, n)| **n as usize <= max)
        .or_else(|| counts.iter().next())
        .map(|(l, _)| *l)
}

/// The usable reports at `level` (every level when `None`), without their findings,
/// best rated first.
pub fn reports_at(
    snap: &Arc<Snapshot>,
    opts: &QueryOptions,
    graph: &str,
    level: Option<u32>,
    filter: Filter<'_>,
    limit: usize,
) -> Result<Vec<ReportRef>, Error> {
    reports_at_in(ReadInput::Snapshot(snap), opts, graph, level, filter, limit)
}

/// [`reports_at`] evaluated over a [`BoundReadSource`] without extracting a raw snapshot.
pub fn reports_at_source(
    source: &BoundReadSource,
    store: &Store,
    opts: &QueryOptions,
    graph: &str,
    level: Option<u32>,
    filter: Filter<'_>,
    limit: usize,
) -> Result<Vec<ReportRef>, Error> {
    reports_at_in(
        ReadInput::Source(source, store),
        opts,
        graph,
        level,
        filter,
        limit,
    )
}

/// Return up to `limit` distinct usable reports ordered by `DESC(?rating), ?r` using
/// bounded Top-K pages (`O(limit)` memory, never building a full-graph `GROUP BY` hash
/// table) and keyset advancement so duplicate community links or statuses on one report
/// never evict lower-rated reports.
fn reports_at_in(
    input: ReadInput<'_>,
    opts: &QueryOptions,
    graph: &str,
    level: Option<u32>,
    filter: Filter<'_>,
    limit: usize,
) -> Result<Vec<ReportRef>, Error> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let page_limit = limit.max(32);
    let policy_clause = filter.policy_clause()?;
    let level_filter = level.map_or(String::new(), |l| format!("FILTER(?l = {l})"));
    let mut out: Vec<ReportRef> = Vec::with_capacity(limit);
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(limit);
    let mut cursor: Option<(Option<u64>, String)> = None;

    loop {
        let cursor_filter = match &cursor {
            None => String::new(),
            Some((Some(rat), last_iri)) => format!(
                "FILTER(!BOUND(?rating) || ?rating < {rat} || (?rating = {rat} && STR(?r) > {}))",
                oxrdf::Literal::new_simple_literal(last_iri),
            ),
            Some((None, last_iri)) => format!(
                "FILTER(!BOUND(?rating) && STR(?r) > {})",
                oxrdf::Literal::new_simple_literal(last_iri),
            ),
        };
        let q = format!(
            "SELECT ?r ?c ?l ?title ?summary ?status ?rating ?n ?policy ?version ?configuration WHERE {{ {values} GRAPH <{graph}> {{ ?c a <{community}> ; <{level_pred}> ?l ; <{report}> ?r ; <{count}> ?n . {level_filter} ?r <{status}> ?status ; <{label}> ?title ; <{summary}> ?summary{policy_clause} . OPTIONAL {{ ?r <{rating}> ?rating }} OPTIONAL {{ ?r <{policy}> ?policy }} OPTIONAL {{ ?r <{version}> ?version }} OPTIONAL {{ ?r <{configuration}> ?configuration }} {cursor_filter} }} }} ORDER BY DESC(?rating) ?r ?c ?status LIMIT {page_limit}",
            values = filter.values(),
            community = gr("Community"),
            level_pred = gr("level"),
            report = gr("report"),
            count = gr("memberCount"),
            status = gr("status"),
            label = store::RDFS_LABEL,
            summary = gr("summary"),
            rating = gr("rating"),
            policy = gr("reviewPolicy"),
            version = gr("policyVersion"),
            configuration = gr("evidenceConfiguration"),
        );
        let rows = rows_in(input, opts, &q)?;
        let raw_count = rows.len();
        let mut last_key: Option<(Option<u64>, String)> = None;
        for row in rows {
            let r = row.values();
            let Some(rep_iri) = iri(&r[0]) else {
                continue;
            };
            let rating = int(&r[6]);
            last_key = Some((rating, rep_iri.clone()));
            if seen.contains(&rep_iri) {
                continue;
            }
            let (Some(community), Some(lv), Some(title), Some(summary), Some(st_str)) =
                (iri(&r[1]), int(&r[2]), lit(&r[3]), lit(&r[4]), lit(&r[5]))
            else {
                continue;
            };
            let Some(status) = Status::parse(&st_str) else {
                continue;
            };
            let review_policy = match filter.review_policy_iri {
                Some(req) => Some(ReviewPolicyRef {
                    iri: req.to_string(),
                    version: int(&r[9]),
                    configuration: lit(&r[10]),
                }),
                None => iri(&r[8]).map(|p| ReviewPolicyRef {
                    iri: p,
                    version: int(&r[9]),
                    configuration: lit(&r[10]),
                }),
            };
            seen.insert(rep_iri.clone());
            out.push(ReportRef {
                iri: rep_iri,
                community,
                level: lv as u32,
                title,
                summary,
                status,
                rating,
                member_count: int(&r[7]).unwrap_or(0),
                score: None,
                findings: Vec::new(),
                review_policy,
            });
            if out.len() >= limit {
                break;
            }
        }
        if out.len() >= limit || raw_count < page_limit {
            break;
        }
        match last_key {
            Some(next) if cursor.as_ref() != Some(&next) => cursor = Some(next),
            _ => break,
        }
    }
    Ok(out)
}

/// The usable reports among `reports`.
#[cfg(test)]
pub fn reports_named(
    snap: &Arc<Snapshot>,
    opts: &QueryOptions,
    graph: &str,
    reports: &[String],
    filter: Filter<'_>,
) -> Result<Vec<ReportRef>, Error> {
    reports_named_in(
        ReadInput::Snapshot(snap),
        opts,
        graph,
        reports,
        None,
        filter,
    )
}

/// The usable reports among `reports` linked from a community at `level`.
pub fn reports_named_at(
    snap: &Arc<Snapshot>,
    opts: &QueryOptions,
    graph: &str,
    reports: &[String],
    level: u32,
    filter: Filter<'_>,
) -> Result<Vec<ReportRef>, Error> {
    reports_named_in(
        ReadInput::Snapshot(snap),
        opts,
        graph,
        reports,
        Some(level),
        filter,
    )
}

/// [`reports_named_at`] evaluated over a [`BoundReadSource`] without extracting a raw snapshot.
pub fn reports_named_at_source(
    source: &BoundReadSource,
    store: &Store,
    opts: &QueryOptions,
    graph: &str,
    reports: &[String],
    level: u32,
    filter: Filter<'_>,
) -> Result<Vec<ReportRef>, Error> {
    reports_named_in(
        ReadInput::Source(source, store),
        opts,
        graph,
        reports,
        Some(level),
        filter,
    )
}

fn reports_named_in(
    input: ReadInput<'_>,
    opts: &QueryOptions,
    graph: &str,
    reports: &[String],
    level: Option<u32>,
    filter: Filter<'_>,
) -> Result<Vec<ReportRef>, Error> {
    let mut names: Vec<String> = reports
        .iter()
        .filter_map(|r| oxrdf::NamedNode::new(r.as_str()).ok())
        .map(|n| n.to_string())
        .collect();
    names.sort();
    names.dedup();
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let values = format!("VALUES ?r {{ {} }}", names.join(" "));
    reports_where_in(input, opts, graph, &values, level, filter, names.len())
}

fn reports_where_in(
    input: ReadInput<'_>,
    opts: &QueryOptions,
    graph: &str,
    extra: &str,
    level: Option<u32>,
    filter: Filter<'_>,
    limit: usize,
) -> Result<Vec<ReportRef>, Error> {
    const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
    const PROP_BRANCH_BATCH: usize = 32;

    if limit == 0 {
        return Ok(Vec::new());
    }
    let policy_clause = filter.policy_clause()?;
    let level_filter = level.map_or(String::new(), |l| format!("FILTER(?l = {l})"));
    let (community, level_pred, report, count, status_pred, label_pred, summary_pred, rating_pred) = (
        gr("Community"),
        gr("level"),
        gr("report"),
        gr("memberCount"),
        gr("status"),
        store::RDFS_LABEL,
        gr("summary"),
        gr("rating"),
    );

    // Step 1: pick one deterministic canonical community `MIN(?c)` per distinct
    // eligible report IRI before applying `ORDER BY` and `LIMIT`, so multiple
    // community links or admitted statuses on one report never evict another report.
    let q = format!(
        "SELECT ?r (MIN(?c) AS ?comm) (MAX(?rating) AS ?best_rating) WHERE {{ {values} {extra} GRAPH <{graph}> {{ ?c a <{community}> ; <{level_pred}> ?l ; <{report}> ?r ; <{count}> ?n . {level_filter} ?r <{status_pred}> ?status ; <{label_pred}> ?title ; <{summary_pred}> ?summary{policy_clause} . OPTIONAL {{ ?r <{rating_pred}> ?rating }} }} }} GROUP BY ?r ORDER BY DESC(?best_rating) ?r LIMIT {limit}",
        values = filter.values(),
    );
    let mut selected: Vec<(oxrdf::NamedNode, oxrdf::NamedNode)> = Vec::new();
    for row in rows_in(input, opts, &q)? {
        let row = row.values();
        if let (Some(Term::NamedNode(r)), Some(Term::NamedNode(c))) = (&row[0], &row[1]) {
            selected.push((r.clone(), c.clone()));
        }
    }
    if selected.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2a: hydrate the chosen community's `gr:memberCount` (and `gr:level` when
    // unfiltered) via predicate-pinned GSPO lookups, avoiding `gr:member` scans.
    let mut uniq_comms: Vec<oxrdf::NamedNode> = Vec::with_capacity(selected.len());
    let mut seen_comms: std::collections::HashSet<&str> =
        std::collections::HashSet::with_capacity(selected.len());
    for (_, c) in &selected {
        if seen_comms.insert(c.as_str()) {
            uniq_comms.push(c.clone());
        }
    }
    #[derive(Default)]
    struct CommMeta {
        level: Option<u32>,
        member_count: Option<u64>,
    }
    let mut comm_meta: HashMap<String, CommMeta> = HashMap::with_capacity(uniq_comms.len());
    let comm_preds: Vec<&str> = if level.is_some() {
        vec![count.as_str()]
    } else {
        vec![count.as_str(), level_pred.as_str()]
    };
    let comm_chunk_size = (PROP_BRANCH_BATCH / comm_preds.len()).max(1);
    for chunk in uniq_comms.chunks(comm_chunk_size) {
        let mut branches = Vec::with_capacity(chunk.len() * comm_preds.len());
        for c in chunk {
            for &p in &comm_preds {
                branches.push(format!("{c} <{p}> ?o . BIND({c} AS ?c) BIND(<{p}> AS ?p)"));
            }
        }
        let cq = format!(
            "SELECT ?c ?p ?o WHERE {{ GRAPH <{graph}> {{ {} }} }}",
            balanced_union(&branches),
        );
        for row in rows_in(input, opts, &cq)? {
            let row = row.values();
            let (Some(c_iri), Some(p)) = (iri(&row[0]), iri(&row[1])) else {
                continue;
            };
            let entry = comm_meta.entry(c_iri).or_default();
            if p == count {
                if let Some(n) = int(&row[2]) {
                    entry.member_count = Some(entry.member_count.map_or(n, |cur| cur.min(n)));
                }
            } else if p == level_pred
                && let Some(lv) = int(&row[2])
            {
                let lv = lv as u32;
                entry.level = Some(entry.level.map_or(lv, |cur| cur.min(lv)));
            }
        }
    }

    // Step 2b: hydrate report metadata via predicate-pinned GSPO lookups so unrelated
    // predicates (`gr:finding`, `gr:citesReport`, `gr:editOf`) are never scanned.
    #[derive(Default)]
    struct RepMeta {
        title: Option<String>,
        summary: Option<String>,
        status: Option<Status>,
        rating: Option<u64>,
        policy: Option<String>,
        version: Option<u64>,
        configuration: Option<String>,
    }
    let (policy_pred, version_pred, config_pred) = (
        gr("reviewPolicy"),
        gr("policyVersion"),
        gr("evidenceConfiguration"),
    );
    let rep_preds = [
        label_pred,
        summary_pred.as_str(),
        status_pred.as_str(),
        rating_pred.as_str(),
        policy_pred.as_str(),
        version_pred.as_str(),
        config_pred.as_str(),
    ];
    let rep_chunk_size = (PROP_BRANCH_BATCH / rep_preds.len()).max(1);
    let mut rep_meta: HashMap<String, RepMeta> = HashMap::with_capacity(selected.len());
    for chunk in selected.chunks(rep_chunk_size) {
        let mut branches = Vec::with_capacity(chunk.len() * rep_preds.len());
        for (r, _) in chunk {
            for &p in &rep_preds {
                branches.push(format!("{r} <{p}> ?o . BIND({r} AS ?r) BIND(<{p}> AS ?p)"));
            }
        }
        let rq = format!(
            "SELECT ?r ?p ?o WHERE {{ GRAPH <{graph}> {{ {} }} }}",
            balanced_union(&branches),
        );
        for row in rows_in(input, opts, &rq)? {
            let row = row.values();
            let (Some(r_iri), Some(p)) = (iri(&row[0]), iri(&row[1])) else {
                continue;
            };
            let entry = rep_meta.entry(r_iri).or_default();
            if p == label_pred {
                if let Some(v) = lit(&row[2])
                    && entry.title.as_ref().is_none_or(|cur| v < *cur)
                {
                    entry.title = Some(v);
                }
            } else if p == summary_pred {
                if let Some(v) = lit(&row[2])
                    && entry.summary.as_ref().is_none_or(|cur| v < *cur)
                {
                    entry.summary = Some(v);
                }
            } else if p == status_pred {
                if let Some(Term::Literal(l)) = &row[2]
                    && l.datatype().as_str() == XSD_STRING
                    && let Some(st) = Status::parse(l.value()).filter(|st| filter.admits(*st))
                    && entry.status.is_none_or(|cur| st.as_str() < cur.as_str())
                {
                    entry.status = Some(st);
                }
            } else if p == rating_pred {
                if let Some(v) = int(&row[2]) {
                    entry.rating = Some(entry.rating.map_or(v, |cur| cur.max(v)));
                }
            } else if p == policy_pred {
                if let Some(pol) = iri(&row[2]) {
                    match filter.review_policy_iri {
                        Some(req) if pol == req => entry.policy = Some(pol),
                        Some(_) => {}
                        None if entry.policy.as_ref().is_none_or(|cur| pol < *cur) => {
                            entry.policy = Some(pol);
                        }
                        None => {}
                    }
                }
            } else if p == version_pred {
                if let Some(v) = int(&row[2]) {
                    entry.version = Some(entry.version.map_or(v, |cur| cur.min(v)));
                }
            } else if p == config_pred
                && let Some(v) = lit(&row[2])
                && entry.configuration.as_ref().is_none_or(|cur| v < *cur)
            {
                entry.configuration = Some(v);
            }
        }
    }

    let mut out = Vec::with_capacity(selected.len());
    for (r, c) in selected {
        let Some(rm) = rep_meta.remove(r.as_str()) else {
            continue;
        };
        let cm = comm_meta.get(c.as_str());
        let Some(lv) = level.or_else(|| cm.and_then(|m| m.level)) else {
            continue;
        };
        let (Some(title), Some(summary), Some(status)) = (rm.title, rm.summary, rm.status) else {
            continue;
        };
        if filter.review_policy_iri.is_some() && rm.policy.is_none() {
            continue;
        }
        out.push(ReportRef {
            iri: r.as_str().to_string(),
            community: c.as_str().to_string(),
            level: lv,
            title,
            summary,
            status,
            rating: rm.rating,
            member_count: cm.and_then(|m| m.member_count).unwrap_or(0),
            score: None,
            findings: Vec::new(),
            review_policy: rm.policy.map(|p| ReviewPolicyRef {
                iri: p,
                version: rm.version,
                configuration: rm.configuration,
            }),
        });
    }
    Ok(out)
}

/// Fill in the findings of `reports`, in their order, with their citations.
pub fn with_findings(
    snap: &Arc<Snapshot>,
    opts: &QueryOptions,
    graph: &str,
    reports: &mut [ReportRef],
) -> Result<(), Error> {
    with_findings_in(ReadInput::Snapshot(snap), opts, graph, reports)
}

/// [`with_findings`] evaluated over a [`BoundReadSource`] without extracting a raw snapshot.
pub fn with_findings_source(
    source: &BoundReadSource,
    store: &Store,
    opts: &QueryOptions,
    graph: &str,
    reports: &mut [ReportRef],
) -> Result<(), Error> {
    with_findings_in(ReadInput::Source(source, store), opts, graph, reports)
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

fn with_findings_in(
    input: ReadInput<'_>,
    opts: &QueryOptions,
    graph: &str,
    reports: &mut [ReportRef],
) -> Result<(), Error> {
    const REPORT_FINDINGS_BATCH: usize = 16;
    const FINDING_PROPS_BATCH: usize = 16;

    if reports.is_empty() {
        return Ok(());
    }
    let report_nodes: Vec<oxrdf::NamedNode> = reports
        .iter()
        .filter_map(|r| oxrdf::NamedNode::new(r.iri.as_str()).ok())
        .collect();
    if report_nodes.is_empty() {
        for r in reports {
            r.findings.clear();
        }
        return Ok(());
    }

    // Step 1: fetch (report, finding) links via grounded GSPO point lookups for the
    // selected reports only, avoiding full-graph scans under tight row budgets.
    let finding_pred = gr("finding");
    let mut report_findings: HashMap<String, Vec<String>> = HashMap::new();
    let mut uniq_findings: Vec<oxrdf::NamedNode> = Vec::new();
    let mut seen_findings: std::collections::HashSet<String> = std::collections::HashSet::new();
    for chunk in report_nodes.chunks(REPORT_FINDINGS_BATCH) {
        let branches: Vec<String> = chunk
            .iter()
            .map(|rep| format!("BIND({rep} AS ?r) {rep} <{finding_pred}> ?f ."))
            .collect();
        let q = format!(
            "SELECT ?r ?f WHERE {{ GRAPH <{graph}> {{ {} }} }}",
            balanced_union(&branches),
        );
        for row in rows_in(input, opts, &q)? {
            let row = row.values();
            let (Some(r), Some(Term::NamedNode(f_node))) = (iri(&row[0]), &row[1]) else {
                continue;
            };
            let f_iri = f_node.as_str().to_string();
            let list = report_findings.entry(r).or_default();
            if !list.iter().any(|existing| existing == &f_iri) {
                list.push(f_iri.clone());
            }
            if seen_findings.insert(f_iri) {
                uniq_findings.push(f_node.clone());
            }
        }
    }

    // Step 2: fetch order, text, and optional citations strictly for the selected
    // finding IDs (grounded GSPO prefix lookups per finding) so citations on unselected
    // reports or other review policies are never scanned.
    struct RawFinding {
        has_order: bool,
        order: u64,
        text: Option<String>,
        about: Vec<String>,
        cites: Vec<String>,
        claims: Vec<String>,
        decisions: Vec<String>,
    }
    let (order_pred, text_pred, about_pred, cites_pred, claim_pred, decision_pred) = (
        gr("order"),
        gr("text"),
        gr("about"),
        gr("cites"),
        gr("citesClaim"),
        gr("citesDecision"),
    );
    let pred_filter = format!(
        "FILTER(?p IN (<{order_pred}>, <{text_pred}>, <{about_pred}>, <{cites_pred}>, <{claim_pred}>, <{decision_pred}>))"
    );
    let mut raw: HashMap<String, RawFinding> = HashMap::with_capacity(uniq_findings.len());
    for chunk in uniq_findings.chunks(FINDING_PROPS_BATCH) {
        let branches: Vec<String> = chunk
            .iter()
            .map(|f| format!("BIND({f} AS ?f) {f} ?p ?v ."))
            .collect();
        let q = format!(
            "SELECT ?f ?p ?v WHERE {{ GRAPH <{graph}> {{ {{ {} }} {pred_filter} }} }}",
            balanced_union(&branches),
        );
        for row in rows_in(input, opts, &q)? {
            let row = row.values();
            let (Some(f_iri), Some(p)) = (iri(&row[0]), iri(&row[1])) else {
                continue;
            };
            let entry = raw.entry(f_iri).or_insert_with(|| RawFinding {
                has_order: false,
                order: 0,
                text: None,
                about: Vec::new(),
                cites: Vec::new(),
                claims: Vec::new(),
                decisions: Vec::new(),
            });
            if p == order_pred {
                let ord = int(&row[2]).unwrap_or(0);
                if !entry.has_order || ord < entry.order {
                    entry.has_order = true;
                    entry.order = ord;
                }
            } else if p == text_pred {
                if entry.text.is_none() {
                    entry.text = lit(&row[2]);
                }
            } else if p == about_pred {
                if let Some(Term::NamedNode(n)) = &row[2]
                    && !entry.about.iter().any(|a| a == n.as_str())
                {
                    entry.about.push(n.as_str().to_string());
                }
            } else if p == claim_pred {
                if let Some(Term::NamedNode(n)) = &row[2]
                    && !entry.claims.iter().any(|c| c == n.as_str())
                {
                    entry.claims.push(n.as_str().to_string());
                }
            } else if p == decision_pred {
                if let Some(Term::NamedNode(n)) = &row[2]
                    && !entry.decisions.iter().any(|c| c == n.as_str())
                {
                    entry.decisions.push(n.as_str().to_string());
                }
            } else if p == cites_pred
                && let Some(t) = &row[2]
            {
                let t = t.to_string();
                if !entry.cites.contains(&t) {
                    entry.cites.push(t);
                }
            }
        }
    }

    for r in reports {
        let mut list: Vec<FindingRef> = Vec::new();
        if let Some(f_iris) = report_findings.remove(&r.iri) {
            for f_iri in f_iris {
                if let Some(rf) = raw.get(&f_iri)
                    && rf.has_order
                    && let Some(text) = &rf.text
                {
                    list.push(FindingRef {
                        iri: f_iri,
                        order: rf.order,
                        text: text.clone(),
                        about: rf.about.clone(),
                        cites: rf.cites.clone(),
                        claims: rf.claims.clone(),
                        decisions: rf.decisions.clone(),
                    });
                }
            }
        }
        list.sort_by_key(|x| x.order);
        r.findings = list;
    }
    Ok(())
}

/// The report of the community at `level` that holds each of `members`, for members
/// that one holds. Without a level, each member takes the finest community that holds it
/// and has a report passing `filter`, because a build folds small communities into their
/// parents and a member can lack a community at the finer levels. A member with no
/// such community has no entry.
pub fn reports_of_members(
    snap: &Arc<Snapshot>,
    opts: &QueryOptions,
    graph: &str,
    members: &[String],
    level: Option<u32>,
    filter: Filter<'_>,
) -> Result<Vec<(String, ReportRef)>, Error> {
    reports_of_members_in(
        ReadInput::Snapshot(snap),
        opts,
        graph,
        members,
        level,
        filter,
    )
}

/// [`reports_of_members`] evaluated over a [`BoundReadSource`] without extracting a raw snapshot.
pub fn reports_of_members_source(
    source: &BoundReadSource,
    store: &Store,
    opts: &QueryOptions,
    graph: &str,
    members: &[String],
    level: Option<u32>,
    filter: Filter<'_>,
) -> Result<Vec<(String, ReportRef)>, Error> {
    reports_of_members_in(
        ReadInput::Source(source, store),
        opts,
        graph,
        members,
        level,
        filter,
    )
}

fn reports_of_members_in(
    input: ReadInput<'_>,
    opts: &QueryOptions,
    graph: &str,
    members: &[String],
    level: Option<u32>,
    filter: Filter<'_>,
) -> Result<Vec<(String, ReportRef)>, Error> {
    let names: Vec<String> = members
        .iter()
        .filter_map(|m| oxrdf::NamedNode::new(m.as_str()).ok())
        .map(|n| n.to_string())
        .collect();
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let at = match level {
        Some(l) => format!("FILTER(?l = {l})"),
        None => String::new(),
    };
    let q = format!(
        "SELECT ?m ?r ?l WHERE {{ VALUES ?m {{ {} }} GRAPH <{graph}> {{ ?c <{member}> ?m ; <{lv}> ?l ; <{report}> ?r }} {at} }}",
        names.join(" "),
        member = gr("member"),
        lv = gr("level"),
        report = gr("report"),
    );
    let mut pairs: Vec<(String, String, u64)> = rows_in(input, opts, &q)?
        .into_iter()
        .filter_map(|r| {
            let r = r.values();
            Some((iri(&r[0])?, iri(&r[1])?, int(&r[2])?))
        })
        .collect();
    // the finest first, so each member keeps the finest report that passes the filter
    pairs.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
    let mut wanted: Vec<String> = pairs.iter().map(|(_, r, _)| r.clone()).collect();
    wanted.sort();
    wanted.dedup();
    let found = reports_named_in(input, opts, graph, &wanted, level, filter)?;
    let by: HashMap<&str, &ReportRef> = found.iter().map(|r| (r.iri.as_str(), r)).collect();
    let mut out = Vec::new();
    for m in members {
        for (pm, r, _) in &pairs {
            if pm == m
                && let Some(rep) = by.get(r.as_str())
            {
                out.push((m.clone(), (*rep).clone()));
                break;
            }
        }
    }
    Ok(out)
}

/// The distinct entity IRIs referenced by a finding's `gr:about` list and any
/// N-Triples triple terms in its `gr:cites` list, in deterministic order.
pub fn finding_entity_iris(about: &[String], cites: &[String]) -> Vec<oxrdf::NamedNode> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    let mut push = |n: oxrdf::NamedNode| {
        if seen.insert(n.as_str().to_string()) {
            out.push(n);
        }
    };
    for iri in about {
        if let Ok(n) = oxrdf::NamedNode::new(iri.as_str()) {
            push(n);
        }
    }
    for cite in cites {
        if let Some(Term::Triple(t)) = store::parse_term(cite) {
            if let oxrdf::NamedOrBlankNode::NamedNode(s) = t.subject {
                push(s);
            }
            if let Term::NamedNode(o) = t.object {
                push(o);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_is_the_finest_that_fits() {
        let c: BTreeMap<u32, u64> = [(0, 3), (1, 40), (2, 300)].into_iter().collect();
        assert_eq!(choose_level(&c, 100), Some(1));
        assert_eq!(choose_level(&c, 2), Some(0));
        assert_eq!(choose_level(&c, 1000), Some(2));
        assert_eq!(choose_level(&BTreeMap::new(), 100), None);
    }

    #[test]
    fn reviewed_only_keeps_reviewed_reports() {
        let f = Filter {
            reviewed_only: true,
            review_policy_iri: None,
        };
        assert!(f.admits(Status::Reviewed));
        assert!(!f.admits(Status::Unreviewed));
        assert!(!f.admits(Status::Stale));
        let all = Filter::default();
        assert!(all.admits(Status::Stale));
        assert!(!all.admits(Status::Rejected));
        assert!(!all.admits(Status::Failed));
    }

    #[test]
    fn policy_clause_validates_and_formats_iri() {
        let none = Filter::default();
        assert_eq!(none.policy_clause().unwrap(), "");
        let ok = Filter {
            reviewed_only: false,
            review_policy_iri: Some("urn:x-sparkles:evidence:policy:main"),
        };
        let clause = ok.policy_clause().unwrap();
        assert!(clause.contains(&gr("reviewPolicy")), "{clause}");
        assert!(
            clause.contains("<urn:x-sparkles:evidence:policy:main>"),
            "{clause}"
        );
        let bad = Filter {
            reviewed_only: false,
            review_policy_iri: Some("not a valid iri"),
        };
        assert!(bad.policy_clause().is_err());
    }
}
