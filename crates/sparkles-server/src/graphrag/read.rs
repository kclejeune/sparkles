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
    let level_filter = level.map_or(String::new(), |l| format!("FILTER(?l = {l})"));
    reports_where(snap, opts, graph, &level_filter, filter, limit)
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
    let level_filter = level.map_or(String::new(), |l| format!("FILTER(?l = {l})"));
    reports_where_in(
        ReadInput::Source(source, store),
        opts,
        graph,
        &level_filter,
        filter,
        limit,
    )
}

/// The usable reports among `reports`.
pub fn reports_named(
    snap: &Arc<Snapshot>,
    opts: &QueryOptions,
    graph: &str,
    reports: &[String],
    filter: Filter<'_>,
) -> Result<Vec<ReportRef>, Error> {
    reports_named_in(ReadInput::Snapshot(snap), opts, graph, reports, filter)
}

/// [`reports_named`] evaluated over a [`BoundReadSource`] without extracting a raw snapshot.
pub fn reports_named_source(
    source: &BoundReadSource,
    store: &Store,
    opts: &QueryOptions,
    graph: &str,
    reports: &[String],
    filter: Filter<'_>,
) -> Result<Vec<ReportRef>, Error> {
    reports_named_in(
        ReadInput::Source(source, store),
        opts,
        graph,
        reports,
        filter,
    )
}

fn reports_named_in(
    input: ReadInput<'_>,
    opts: &QueryOptions,
    graph: &str,
    reports: &[String],
    filter: Filter<'_>,
) -> Result<Vec<ReportRef>, Error> {
    let names: Vec<String> = reports
        .iter()
        .filter_map(|r| oxrdf::NamedNode::new(r.as_str()).ok())
        .map(|n| n.to_string())
        .collect();
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let values = format!("VALUES ?r {{ {} }}", names.join(" "));
    reports_where_in(input, opts, graph, &values, filter, names.len())
}

fn reports_where(
    snap: &Arc<Snapshot>,
    opts: &QueryOptions,
    graph: &str,
    extra: &str,
    filter: Filter<'_>,
    limit: usize,
) -> Result<Vec<ReportRef>, Error> {
    reports_where_in(ReadInput::Snapshot(snap), opts, graph, extra, filter, limit)
}

fn reports_where_in(
    input: ReadInput<'_>,
    opts: &QueryOptions,
    graph: &str,
    extra: &str,
    filter: Filter<'_>,
    limit: usize,
) -> Result<Vec<ReportRef>, Error> {
    let policy_clause = filter.policy_clause()?;
    let q = format!(
        "SELECT ?r ?c ?l ?title ?summary ?status ?rating ?n ?policy ?version ?configuration WHERE {{ {values} {extra} GRAPH <{graph}> {{ ?c a <{community}> ; <{level}> ?l ; <{report}> ?r ; <{count}> ?n . ?r <{status}> ?status ; <{label}> ?title ; <{summary}> ?summary{policy_clause} . OPTIONAL {{ ?r <{rating}> ?rating }} OPTIONAL {{ ?r <{policy}> ?policy OPTIONAL {{ ?r <{version}> ?version }} OPTIONAL {{ ?r <{configuration}> ?configuration }} }} }} }} ORDER BY DESC(?rating) ?r LIMIT {limit}",
        values = filter.values(),
        community = gr("Community"),
        level = gr("level"),
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
    Ok(rows_in(input, opts, &q)?
        .into_iter()
        .filter_map(|r| {
            let r = r.values();
            Some(ReportRef {
                iri: iri(&r[0])?,
                community: iri(&r[1])?,
                level: int(&r[2])? as u32,
                title: lit(&r[3])?,
                summary: lit(&r[4])?,
                status: Status::parse(&lit(&r[5])?)?,
                rating: int(&r[6]),
                member_count: int(&r[7]).unwrap_or(0),
                score: None,
                findings: Vec::new(),
                review_policy: iri(&r[8]).map(|p| ReviewPolicyRef {
                    iri: p,
                    version: int(&r[9]),
                    configuration: lit(&r[10]),
                }),
            })
        })
        .collect())
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

fn with_findings_in(
    input: ReadInput<'_>,
    opts: &QueryOptions,
    graph: &str,
    reports: &mut [ReportRef],
) -> Result<(), Error> {
    if reports.is_empty() {
        return Ok(());
    }
    let names: Vec<String> = reports
        .iter()
        .filter_map(|r| oxrdf::NamedNode::new(r.iri.as_str()).ok())
        .map(|n| n.to_string())
        .collect();
    let q = format!(
        "SELECT ?r ?f ?o ?text ?p ?cite WHERE {{ GRAPH <{graph}> {{ VALUES ?r {{ {} }} ?r <{finding}> ?f . ?f <{order}> ?o ; <{text}> ?text . OPTIONAL {{ {{ ?f <{about}> ?cite BIND(<{about}> AS ?p) }} UNION {{ ?f <{cites}> ?cite BIND(<{cites}> AS ?p) }} UNION {{ ?f <{claim}> ?cite BIND(<{claim}> AS ?p) }} UNION {{ ?f <{decision}> ?cite BIND(<{decision}> AS ?p) }} }} }} }} ORDER BY ?r ?o",
        names.join(" "),
        finding = gr("finding"),
        order = gr("order"),
        text = gr("text"),
        about = gr("about"),
        cites = gr("cites"),
        claim = gr("citesClaim"),
        decision = gr("citesDecision"),
    );
    let mut by: HashMap<String, Vec<FindingRef>> = HashMap::new();
    for row in rows_in(input, opts, &q)? {
        let row = row.values();
        let (Some(r), Some(f), Some(text)) = (iri(&row[0]), iri(&row[1]), lit(&row[3])) else {
            continue;
        };
        let list = by.entry(r).or_default();
        let at = match list.iter().position(|x| x.iri == f) {
            Some(i) => i,
            None => {
                list.push(FindingRef {
                    iri: f,
                    order: int(&row[2]).unwrap_or(0),
                    text,
                    about: Vec::new(),
                    cites: Vec::new(),
                    claims: Vec::new(),
                    decisions: Vec::new(),
                });
                list.len() - 1
            }
        };
        let x = &mut list[at];
        let p = iri(&row[4]);
        match (p.as_deref(), &row[5]) {
            (Some(p), Some(Term::NamedNode(n))) if p == gr("about") => {
                if !x.about.iter().any(|a| a == n.as_str()) {
                    x.about.push(n.as_str().to_string());
                }
            }
            (Some(p), Some(Term::NamedNode(n))) if p == gr("citesClaim") => {
                if !x.claims.iter().any(|c| c == n.as_str()) {
                    x.claims.push(n.as_str().to_string());
                }
            }
            (Some(p), Some(Term::NamedNode(n))) if p == gr("citesDecision") => {
                if !x.decisions.iter().any(|c| c == n.as_str()) {
                    x.decisions.push(n.as_str().to_string());
                }
            }
            (Some(p), Some(t)) if p == gr("cites") => {
                let t = t.to_string();
                if !x.cites.contains(&t) {
                    x.cites.push(t);
                }
            }
            _ => {}
        }
    }
    for r in reports {
        let mut f = by.remove(&r.iri).unwrap_or_default();
        f.sort_by_key(|x| x.order);
        r.findings = f;
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
    let found = reports_named_in(input, opts, graph, &wanted, filter)?;
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
