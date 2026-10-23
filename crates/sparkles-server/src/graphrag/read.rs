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
use sparkles::store::Snapshot;
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
    /// the cited facts as N-Triples triple terms, their reifiers and the cited child
    /// findings
    pub cites: Vec<String>,
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
}

/// Which reports a consumer uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// only reports a person marked reviewed (`reviewedOnly`, A7)
    pub reviewed_only: bool,
}

impl Filter {
    pub fn admits(&self, s: Status) -> bool {
        match s {
            Status::Reviewed => true,
            Status::Unreviewed | Status::Stale => !self.reviewed_only,
            Status::Rejected | Status::Failed => false,
        }
    }

    /// The statuses as a SPARQL `VALUES` block for `?status`.
    fn values(&self) -> String {
        let v: Vec<String> = Status::ALL
            .into_iter()
            .filter(|s| self.admits(*s))
            .map(|s| format!("\"{}\"", s.as_str()))
            .collect();
        format!("VALUES ?status {{ {} }}", v.join(" "))
    }
}

/// Whether the caller's view reads `graph` in `snap`. A view that the guard did not
/// prove for the reports' footprints does not.
pub fn readable(snap: &Arc<Snapshot>, opts: &QueryOptions, graph: &str) -> bool {
    match &opts.graphs {
        None => true,
        Some(v) => v.bound(snap).read.allows_iri(graph),
    }
}

fn rows(
    snap: &Arc<Snapshot>,
    opts: &QueryOptions,
    q: &str,
) -> Result<Vec<Vec<Option<Term>>>, Error> {
    Ok(sparql::query(snap.clone(), q, opts)?.rows())
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
    filter: Filter,
) -> Result<BTreeMap<u32, u64>, Error> {
    let q = format!(
        "SELECT ?l (COUNT(?r) AS ?n) WHERE {{ {values} GRAPH <{graph}> {{ ?c a <{community}> ; <{level}> ?l ; <{report}> ?r . ?r <{status}> ?status }} }} GROUP BY ?l",
        values = filter.values(),
        community = gr("Community"),
        level = gr("level"),
        report = gr("report"),
        status = gr("status"),
    );
    let mut out = BTreeMap::new();
    for r in rows(snap, opts, &q)? {
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
    filter: Filter,
    limit: usize,
) -> Result<Vec<ReportRef>, Error> {
    let level_filter = level.map_or(String::new(), |l| format!("FILTER(?l = {l})"));
    reports_where(snap, opts, graph, &level_filter, filter, limit)
}

/// The usable reports among `reports`.
pub fn reports_named(
    snap: &Arc<Snapshot>,
    opts: &QueryOptions,
    graph: &str,
    reports: &[String],
    filter: Filter,
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
    reports_where(snap, opts, graph, &values, filter, names.len())
}

fn reports_where(
    snap: &Arc<Snapshot>,
    opts: &QueryOptions,
    graph: &str,
    extra: &str,
    filter: Filter,
    limit: usize,
) -> Result<Vec<ReportRef>, Error> {
    let q = format!(
        "SELECT ?r ?c ?l ?title ?summary ?status ?rating ?n WHERE {{ {values} {extra} GRAPH <{graph}> {{ ?c a <{community}> ; <{level}> ?l ; <{report}> ?r ; <{count}> ?n . ?r <{status}> ?status ; <{label}> ?title ; <{summary}> ?summary . OPTIONAL {{ ?r <{rating}> ?rating }} }} }} ORDER BY DESC(?rating) ?r LIMIT {limit}",
        values = filter.values(),
        community = gr("Community"),
        level = gr("level"),
        report = gr("report"),
        count = gr("memberCount"),
        status = gr("status"),
        label = store::RDFS_LABEL,
        summary = gr("summary"),
        rating = gr("rating"),
    );
    Ok(rows(snap, opts, &q)?
        .into_iter()
        .filter_map(|r| {
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
    if reports.is_empty() {
        return Ok(());
    }
    let names: Vec<String> = reports
        .iter()
        .filter_map(|r| oxrdf::NamedNode::new(r.iri.as_str()).ok())
        .map(|n| n.to_string())
        .collect();
    let q = format!(
        "SELECT ?r ?f ?o ?text ?cite WHERE {{ VALUES ?r {{ {} }} GRAPH <{graph}> {{ ?r <{finding}> ?f . ?f <{order}> ?o ; <{text}> ?text . OPTIONAL {{ ?f <{cites}> ?cite }} }} }} ORDER BY ?r ?o",
        names.join(" "),
        finding = gr("finding"),
        order = gr("order"),
        text = gr("text"),
        cites = gr("cites"),
    );
    let mut by: HashMap<String, Vec<FindingRef>> = HashMap::new();
    for row in rows(snap, opts, &q)? {
        let (Some(r), Some(f), Some(text)) = (iri(&row[0]), iri(&row[1]), lit(&row[3])) else {
            continue;
        };
        let list = by.entry(r).or_default();
        let cite = row[4].as_ref().map(|t| t.to_string());
        match list.iter_mut().find(|x| x.iri == f) {
            Some(x) => x.cites.extend(cite),
            None => list.push(FindingRef {
                iri: f,
                order: int(&row[2]).unwrap_or(0),
                text,
                cites: cite.into_iter().collect(),
            }),
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
    filter: Filter,
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
    let mut pairs: Vec<(String, String, u64)> = rows(snap, opts, &q)?
        .into_iter()
        .filter_map(|r| Some((iri(&r[0])?, iri(&r[1])?, int(&r[2])?)))
        .collect();
    // the finest first, so each member keeps the finest report that passes the filter
    pairs.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
    let mut wanted: Vec<String> = pairs.iter().map(|(_, r, _)| r.clone()).collect();
    wanted.sort();
    wanted.dedup();
    let found = reports_named(snap, opts, graph, &wanted, filter)?;
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

/// The report a finding belongs to, from the finding's IRI (`store::finding_iri`).
pub fn report_of_finding(f: &str) -> Option<&str> {
    let (r, n) = f.rsplit_once("#f")?;
    n.parse::<u64>().ok().map(|_| r)
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
    fn findings_name_their_report() {
        assert_eq!(
            report_of_finding(&store::finding_iri("urn:r:1", 3)),
            Some("urn:r:1")
        );
        assert_eq!(report_of_finding("urn:r:1#fx"), None);
    }

    #[test]
    fn reviewed_only_keeps_reviewed_reports() {
        let f = Filter {
            reviewed_only: true,
        };
        assert!(f.admits(Status::Reviewed));
        assert!(!f.admits(Status::Unreviewed));
        assert!(!f.admits(Status::Stale));
        let all = Filter::default();
        assert!(all.admits(Status::Stale));
        assert!(!all.admits(Status::Rejected));
        assert!(!all.admits(Status::Failed));
    }
}
