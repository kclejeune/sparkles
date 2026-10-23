//! The groups of a GraphRAG community hierarchy (specs C22 §4 and U06 §3): the
//! communities a build stored in its reports graph, one level at a time, each with its
//! member count and its report's title and status, and the links between them counted
//! from the data.
//!
//! A reports graph holds `gr:Community` resources with `gr:level` (0 is the coarsest),
//! `skos:broader` to the community one level up, `gr:member` and `gr:memberCount`, and
//! `gr:report` to the current report, whose `rdfs:label` is its title and `gr:status` its
//! review status. The dataset opens at one level, a community opens into the
//! communities one level below it, and a community without any opens into its members.
//! A community's members that no community below it holds, because the build folded
//! their small community into it, form one more group.
//!
//! Reports graphs are guarded (C22 §6): a restricted view reads one only when the
//! snapshot's disclosure footprints prove it for the reader, which binding the view to
//! the snapshot decides. The answer is computed only when the bound view reads the
//! graph. Otherwise it says that the communities are unavailable and holds no community,
//! report, title, count or member of the graph, whatever the request named.
//!
//! A group id here is `r1.` and the URL-safe base64 of the graph, the level the path
//! starts at, the path of community IRIs and the folded flag. It is decoded through
//! checks, and the path is checked against the reports at the snapshot before it is
//! used, so an id a client makes up names no more than its own SPARQL queries could.

use super::groups::{
    EDGE_ROWS, Edges, LABELS, RDF_TYPE, Run, SKOS_BROADER, TOP_GROUPS, TOP_MEMBERS, count, get,
    iri_of,
};
use super::{By, GroupsRequest, MAX_EDGES, Within};
use crate::error::{Error, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use oxrdf::Term;
use serde::{Deserialize, Serialize};
use serde_json::{Value as J, json};
use std::collections::{HashMap, HashSet};

/// The namespace of the graphs the disclosure guard protects (C22 §6.1).
pub const GUARDED_PREFIX: &str = "urn:x-sparkles:graphrag:";
/// The reports graph a build writes when its settings name no other.
pub const DEFAULT_REPORTS_GRAPH: &str = "urn:x-sparkles:graphrag:reports";
/// The guarded graph of disclosure footprints, which holds no communities.
pub const DISCLOSURE_GRAPH: &str = "urn:x-sparkles:graphrag:disclosure";
/// The deepest level a request or a group id names. A build keeps at most eight.
pub const MAX_REPORT_LEVEL: u32 = 15;

const GR: &str = "urn:x-sparkles:graphrag#";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
/// The prefix and version of a reports group id.
const ID_PREFIX: &str = "r1.";
/// Bytes of an id at most.
const MAX_ID: usize = 32 << 10;

fn gr(name: &str) -> String {
    format!("{GR}{name}")
}

/// Whether `id` names a group of a reports answer.
pub(super) fn is_reports_id(id: &str) -> bool {
    id.starts_with(ID_PREFIX)
}

/// Refuse a graph that cannot be a reports graph: one outside the guarded namespace, the
/// namespace itself, or the graph of footprints.
pub(super) fn check_graph(g: &str) -> std::result::Result<(), String> {
    if !g.starts_with(GUARDED_PREFIX) || g.len() == GUARDED_PREFIX.len() {
        return Err(format!(
            "by.reports.graph must name a reports graph under {GUARDED_PREFIX}"
        ));
    }
    if g == DISCLOSURE_GRAPH {
        return Err(
            "by.reports.graph names the graph of footprints, which holds no reports".into(),
        );
    }
    Ok(())
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A set of a reports hierarchy: the members of every community at `level` when `path`
/// is empty, else the members of the last community of `path`, whose first community is
/// at `level` and each next one a child of the one before. With `folded`, only those
/// members of the last community that none of its children holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    #[serde(rename = "g")]
    graph: String,
    #[serde(rename = "l")]
    level: u32,
    #[serde(rename = "p")]
    path: Vec<String>,
    #[serde(rename = "o", default, skip_serializing_if = "is_false")]
    folded: bool,
}

impl Set {
    fn encode(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        format!("{ID_PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    /// The set an id names, checked.
    fn decode(id: &str) -> std::result::Result<Set, String> {
        if id.len() > MAX_ID {
            return Err("the group id is too long".into());
        }
        let body = id
            .strip_prefix(ID_PREFIX)
            .ok_or("not a group id of the GraphRAG communities")?;
        let bytes = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| "the group id is malformed")?;
        let s: Set = serde_json::from_slice(&bytes).map_err(|_| "the group id is malformed")?;
        let iri = |s: &str| {
            oxrdf::NamedNode::new(s)
                .map(|n| n.into_string())
                .map_err(|_| "the group id holds an invalid IRI".to_string())
        };
        let graph = iri(&s.graph)?;
        check_graph(&graph).map_err(|_| "the group id names no reports graph".to_string())?;
        if s.path.is_empty() || s.level as usize + s.path.len() > MAX_REPORT_LEVEL as usize + 1 {
            return Err("the group id holds no community or too many levels".into());
        }
        let path = s
            .path
            .iter()
            .map(|c| iri(c))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Set {
            graph,
            level: s.level,
            path,
            folded: s.folded,
        })
    }

    /// The id of a set with the same graph and start level.
    fn with(&self, path: Vec<String>, folded: bool) -> Set {
        Set {
            graph: self.graph.clone(),
            level: self.level,
            path,
            folded,
        }
    }

    /// The group a node is in when its key in this set's partition is `k`: the child
    /// community, or the folded members when `None`.
    fn child(&self, k: Option<&str>) -> Set {
        match k {
            Some(c) => {
                let mut p = self.path.clone();
                p.push(c.to_string());
                self.with(p, false)
            }
            None => self.with(self.path.clone(), true),
        }
    }

    /// The level of the communities that partition this set.
    fn next_level(&self) -> u32 {
        self.level + self.path.len() as u32
    }

    /// The pattern that binds `?v` to the members of the set.
    fn pattern(&self, v: &str) -> String {
        let g = &self.graph;
        let member = gr("member");
        let mut q = match self.path.last() {
            None => format!(
                "GRAPH <{g}> {{ ?{v}_top <{}> {} ; <{member}> ?{v} }}\n",
                gr("level"),
                self.level
            ),
            Some(c) => format!("GRAPH <{g}> {{ <{c}> <{member}> ?{v} }}\n"),
        };
        if self.folded
            && let Some(c) = self.path.last()
        {
            q.push_str(&format!(
                "FILTER NOT EXISTS {{ GRAPH <{g}> {{ ?{v}_sub <{SKOS_BROADER}> <{c}> ; <{member}> ?{v} }} }}\n"
            ));
        }
        q.push_str(&format!("FILTER(isIRI(?{v}))\n"));
        q
    }

    /// An `OPTIONAL` that binds `out` to node `?v`'s community in partition `i` of the
    /// path: 0 is the start level, and `i` above 0 the children of the path's community
    /// `i - 1`. Partition `path.len()` is the one that splits this set.
    fn key(&self, i: usize, v: &str, out: &str) -> String {
        let g = &self.graph;
        let member = gr("member");
        if i == 0 {
            format!(
                "OPTIONAL {{ GRAPH <{g}> {{ {out} <{}> {} ; <{member}> ?{v} }} }}\n",
                gr("level"),
                self.level
            )
        } else {
            format!(
                "OPTIONAL {{ GRAPH <{g}> {{ {out} <{SKOS_BROADER}> <{}> ; <{member}> ?{v} }} }}\n",
                self.path[i - 1]
            )
        }
    }

    /// Where a node lies relative to this set, from its keys in partitions 0 to
    /// `path.len()`.
    fn owner(&self, keys: &[Option<&str>]) -> Owner {
        let n = self.path.len();
        for i in 0..n {
            match keys.get(i).copied().flatten() {
                None if i == 0 => return Owner::Outside,
                None => return Owner::Sibling(self.with(self.path[..i].to_vec(), true)),
                Some(c) if c != self.path[i] => {
                    let mut p = self.path[..i].to_vec();
                    p.push(c.to_string());
                    return Owner::Sibling(self.with(p, false));
                }
                Some(_) => {}
            }
        }
        let k = keys.get(n).copied().flatten();
        if n == 0 {
            return match k {
                None => Owner::Outside,
                Some(c) => Owner::Inside(Some(c.to_string())),
            };
        }
        if self.folded {
            return match k {
                Some(c) => Owner::Sibling(self.child(Some(c))),
                None => Owner::Inside(None),
            };
        }
        Owner::Inside(k.map(str::to_string))
    }
}

/// Where a node lies relative to a set.
enum Owner {
    /// in no community of the path's first level, so in nothing the picture draws
    Outside,
    /// in a sibling group of a community on the path
    Sibling(Set),
    /// in the set, with its key in the set's partition
    Inside(Option<String>),
}

/// The report of a community, as far as the caller reads it.
struct Report {
    iri: String,
    title: Option<String>,
    status: Option<String>,
}

/// One group of a partition: a community, or the folded members when `community` is
/// `None`.
struct Group {
    community: Option<String>,
    size: u64,
    children: u64,
    report: Option<Report>,
}

/// The links of the data between `?a` and `?b`, in either graph form, outside the
/// guarded graphs, and only between resources.
fn link(outgoing: bool) -> String {
    let t = if outgoing { "?a ?p ?b" } else { "?b ?p ?a" };
    format!(
        "{{ {t} }} UNION {{ GRAPH ?lg {{ {t} }} FILTER(!STRSTARTS(STR(?lg), \"{GUARDED_PREFIX}\")) }}\nFILTER(isIRI(?b) && ?p != <{RDF_TYPE}>)\n"
    )
}

/// An `OPTIONAL` that binds `?label` candidates of `?v` from the data graphs.
fn labels(v: &str) -> String {
    let path = format!(
        "({})",
        LABELS
            .iter()
            .map(|l| format!("<{l}>"))
            .collect::<Vec<_>>()
            .join("|")
    );
    format!(
        "OPTIONAL {{ {{ ?{v} {path} ?l }} UNION {{ GRAPH ?llg {{ ?{v} {path} ?l }} FILTER(!STRSTARTS(STR(?llg), \"{GUARDED_PREFIX}\")) }} FILTER(isLiteral(?l)) }}\n"
    )
}

fn lit(t: Option<&Term>) -> Option<String> {
    match t {
        Some(Term::Literal(l)) => Some(l.value().to_string()),
        _ => None,
    }
}

/// The answer when the caller may not read the reports graph or it holds no
/// communities: nothing of the graph but the reason.
fn unavailable(graph: &str, level: u32, access: bool) -> J {
    let (code, reason) = if access {
        (
            "access",
            "The GraphRAG communities are not available to you, because your access does not cover everything their build read.",
        )
    } else {
        (
            "none",
            "No GraphRAG communities were built for this dataset at this state.",
        )
    };
    json!({
        "available": false,
        "unavailable": code,
        "reason": reason,
        "within": { "reports": { "graph": graph, "level": level }, "size": 0 },
        "grouping": { "kind": "reports", "predicate": null, "level": level },
        "communities": false,
        "groups": [],
        "edges": [],
        "truncated": false,
    })
}

/// The answer to a request that reads reports.
pub(super) fn answer(run: &Run<'_>, req: &GroupsRequest) -> Result<J> {
    let (set, group) = match (&req.within, &req.by) {
        (Within::Dataset, By::Reports(r)) => (
            Set {
                graph: r
                    .graph
                    .clone()
                    .unwrap_or_else(|| DEFAULT_REPORTS_GRAPH.to_string()),
                level: r.level.unwrap_or(0),
                path: Vec::new(),
                folded: false,
            },
            None,
        ),
        (Within::Group(id), by) => {
            let set = Set::decode(id).map_err(Error::invalid)?;
            if let By::Reports(r) = by
                && let Some(g) = &r.graph
                && *g != set.graph
            {
                return Err(Error::invalid(
                    "by.reports.graph names another reports graph than the group's",
                ));
            }
            (set, Some(id.clone()))
        }
        _ => return Err(Error::invalid("this request does not read reports")),
    };
    // the guard decides against the snapshot the queries read (C22 §6.3): an unproven
    // reader learns nothing of the graph, not even whether the group it named exists
    let readable = match &run.opts.graphs {
        None => true,
        Some(v) => v.bound(&run.snap).read.allows_iri(&set.graph),
    };
    if !readable {
        return Ok(unavailable(&set.graph, set.level, true));
    }
    let levels = run.levels(&set.graph)?;
    if levels.is_empty() {
        return Ok(unavailable(&set.graph, set.level, false));
    }
    let a = Answer { run, set, levels };
    a.answer(req, group)
}

struct Answer<'r, 'a> {
    run: &'r Run<'a>,
    set: Set,
    /// the communities at each level of the reports graph
    levels: Vec<(u32, u64)>,
}

impl Run<'_> {
    /// The levels of a reports graph and the number of communities at each.
    fn levels(&self, graph: &str) -> Result<Vec<(u32, u64)>> {
        let q = format!(
            "SELECT ?l (COUNT(DISTINCT ?c) AS ?n) WHERE {{ GRAPH <{graph}> {{ ?c a <{}> ; <{}> ?l }} }} GROUP BY ?l ORDER BY ?l",
            gr("Community"),
            gr("level")
        );
        let r = self.select(&q)?;
        let (l, n) = (r.col("l"), r.col("n"));
        Ok(r.rows
            .iter()
            .filter_map(|row| {
                let lv = lit(get(row, l))?.parse::<u32>().ok()?;
                Some((lv, count(get(row, n))))
            })
            .collect())
    }
}

impl Answer<'_, '_> {
    fn answer(&self, req: &GroupsRequest, group: Option<String>) -> Result<J> {
        let set = self.set.clone();
        let mut within = json!({
            "reports": { "graph": set.graph, "level": set.level },
            "size": 0,
        });
        if let Some(g) = &group {
            within["group"] = J::from(g.clone());
        }
        if let Some(c) = set.path.last() {
            let title = self.check_path()?;
            within["community"] = J::from(c.clone());
            within["level"] = J::from(set.next_level() - 1);
            if let Some(t) = title {
                within["title"] = J::from(t);
            }
            if set.folded {
                within["folded"] = J::from(true);
            }
        } else if !self.levels.iter().any(|(l, _)| *l == set.level) {
            let known: Vec<String> = self.levels.iter().map(|(l, _)| l.to_string()).collect();
            return Err(Error::invalid(format!(
                "the reports hold no communities at level {}; their levels are {}",
                set.level,
                known.join(", ")
            )));
        }
        let size = self.size(&set)?;
        within["size"] = J::from(size);
        let members_only = matches!(req.by, By::Members) || set.folded;
        let groups = if members_only {
            None
        } else {
            Some(self.groups(req.limit)?).filter(|(g, _)| !g.is_empty())
        };
        let mut answer = match groups {
            Some((groups, truncated)) => self.groups_answer(groups, truncated)?,
            None => self.members_answer(req.limit, size)?,
        };
        if let Some(o) = answer.as_object_mut() {
            o.insert("available".into(), true.into());
            o.insert("within".into(), within);
            o.insert(
                "levels".into(),
                self.levels
                    .iter()
                    .map(|(l, n)| json!({ "level": l, "communities": n }))
                    .collect(),
            );
        }
        Ok(answer)
    }

    /// Check that the path is a chain of communities from the start level at this
    /// snapshot, and return the title of its last community's report.
    fn check_path(&self) -> Result<Option<String>> {
        let s = &self.set;
        let values: Vec<String> = s.path.iter().map(|c| format!("<{c}>")).collect();
        let q = format!(
            "SELECT ?c ?l ?up ?title WHERE {{ VALUES ?c {{ {} }} GRAPH <{}> {{ ?c a <{}> ; <{}> ?l . OPTIONAL {{ ?c <{SKOS_BROADER}> ?up }} OPTIONAL {{ ?c <{}> ?r . ?r <{RDFS_LABEL}> ?title }} }} }}",
            values.join(" "),
            s.graph,
            gr("Community"),
            gr("level"),
            gr("report"),
        );
        let r = self.run.select(&q)?;
        let (cc, lc, uc, tc) = (r.col("c"), r.col("l"), r.col("up"), r.col("title"));
        let mut level: HashMap<&str, HashSet<u32>> = HashMap::new();
        let mut up: HashMap<&str, HashSet<&str>> = HashMap::new();
        let mut title = None;
        let last = s.path.last().map(String::as_str);
        for row in &r.rows {
            let Some(c) = iri_of(get(row, cc)) else {
                continue;
            };
            if let Some(l) = lit(get(row, lc)).and_then(|v| v.parse::<u32>().ok()) {
                level.entry(c).or_default().insert(l);
            }
            if let Some(u) = iri_of(get(row, uc)) {
                up.entry(c).or_default().insert(u);
            }
            if Some(c) == last && title.is_none() {
                title = lit(get(row, tc));
            }
        }
        for (i, c) in s.path.iter().enumerate() {
            let at = s.level + i as u32;
            let ok_level = level.get(c.as_str()).is_some_and(|l| l.contains(&at));
            let ok_up = i == 0
                || up
                    .get(c.as_str())
                    .is_some_and(|u| u.contains(s.path[i - 1].as_str()));
            if !ok_level || !ok_up {
                return Err(Error::invalid(
                    "the group names communities that the reports at this state do not hold as one path; open the communities again from the top",
                ));
            }
        }
        Ok(title)
    }

    /// The distinct members of a set.
    fn size(&self, set: &Set) -> Result<u64> {
        let q = format!(
            "SELECT (COUNT(DISTINCT ?m) AS ?n) WHERE {{\n{}}}",
            set.pattern("m")
        );
        let r = self.run.select(&q)?;
        Ok(r.rows.first().map_or(0, |row| count(get(row, r.col("n")))))
    }

    /// The groups that split the set, largest first, at most `limit`, and whether more
    /// were left out. Empty when no community splits it.
    fn groups(&self, limit: usize) -> Result<(Vec<Group>, bool)> {
        let s = &self.set;
        let g = &s.graph;
        let cond = match s.path.last() {
            None => format!("<{}> {}", gr("level"), s.level),
            Some(c) => format!("<{SKOS_BROADER}> <{c}>"),
        };
        let q = format!(
            "SELECT ?c ?n ?r ?title ?status WHERE {{ {{ SELECT ?c (SAMPLE(?k) AS ?n) WHERE {{ GRAPH <{g}> {{ ?c a <{community}> ; {cond} ; <{mc}> ?k }} }} GROUP BY ?c ORDER BY DESC(?n) ?c LIMIT {} }} OPTIONAL {{ GRAPH <{g}> {{ ?c <{report}> ?r OPTIONAL {{ ?r <{RDFS_LABEL}> ?title }} OPTIONAL {{ ?r <{status}> ?status }} }} }} }}",
            limit + 1,
            community = gr("Community"),
            mc = gr("memberCount"),
            report = gr("report"),
            status = gr("status"),
        );
        let r = self.run.select(&q)?;
        let (cc, nc, rc, tc, sc) = (
            r.col("c"),
            r.col("n"),
            r.col("r"),
            r.col("title"),
            r.col("status"),
        );
        let mut out: Vec<Group> = Vec::new();
        for row in &r.rows {
            let Some(c) = iri_of(get(row, cc)) else {
                continue;
            };
            if out.iter().any(|x| x.community.as_deref() == Some(c)) {
                continue;
            }
            out.push(Group {
                community: Some(c.to_string()),
                size: count(get(row, nc)),
                children: 0,
                report: iri_of(get(row, rc)).map(|iri| Report {
                    iri: iri.to_string(),
                    title: lit(get(row, tc)),
                    status: lit(get(row, sc)),
                }),
            });
        }
        out.sort_by(|a, b| {
            b.size
                .cmp(&a.size)
                .then_with(|| a.community.cmp(&b.community))
        });
        let truncated = out.len() > limit;
        out.truncate(limit);
        if out.is_empty() {
            return Ok((out, false));
        }
        let values: Vec<String> = out
            .iter()
            .filter_map(|x| x.community.as_ref())
            .map(|c| format!("<{c}>"))
            .collect();
        let q = format!(
            "SELECT ?c (COUNT(DISTINCT ?k) AS ?n) WHERE {{ VALUES ?c {{ {} }} GRAPH <{g}> {{ ?k <{SKOS_BROADER}> ?c }} }} GROUP BY ?c",
            values.join(" ")
        );
        let r = self.run.select(&q)?;
        let (cc, nc) = (r.col("c"), r.col("n"));
        let children: HashMap<&str, u64> = r
            .rows
            .iter()
            .filter_map(|row| Some((iri_of(get(row, cc))?, count(get(row, nc)))))
            .collect();
        for x in &mut out {
            if let Some(c) = &x.community {
                x.children = children.get(c.as_str()).copied().unwrap_or(0);
            }
        }
        // the members a folded child left with this community
        if !s.path.is_empty() && out.len() < limit {
            let folded = self.size(&s.child(None))?;
            if folded > 0 {
                out.push(Group {
                    community: None,
                    size: folded,
                    children: 0,
                    report: None,
                });
            }
        }
        Ok((out, truncated))
    }

    /// The first members of a set, with their labels.
    fn top(&self, set: &Set) -> Result<Vec<J>> {
        let q = format!(
            "SELECT ?m (SAMPLE(?l) AS ?label) WHERE {{ {{ SELECT DISTINCT ?m WHERE {{\n{}}} ORDER BY ?m LIMIT {TOP_MEMBERS} }}\n{}}} GROUP BY ?m",
            set.pattern("m"),
            labels("m")
        );
        let r = self.run.select(&q)?;
        let (mc, lc) = (r.col("m"), r.col("label"));
        let mut out: Vec<J> = r
            .rows
            .iter()
            .filter_map(|row| {
                let iri = iri_of(get(row, mc))?;
                let mut m = json!({ "iri": iri });
                if let Some(l) = lit(get(row, lc)) {
                    m["label"] = l.into();
                }
                Some(m)
            })
            .collect();
        out.sort_by(|a, b| a["iri"].as_str().cmp(&b["iri"].as_str()));
        Ok(out)
    }

    fn groups_answer(&self, groups: Vec<Group>, truncated: bool) -> Result<J> {
        let s = &self.set;
        let level = s.next_level();
        let mut out = Vec::with_capacity(groups.len());
        for (i, g) in groups.iter().enumerate() {
            let child = s.child(g.community.as_deref());
            let top = if i < TOP_GROUPS {
                self.top(&child)?
            } else {
                Vec::new()
            };
            let label = match (
                &g.community,
                g.report.as_ref().and_then(|r| r.title.clone()),
            ) {
                (_, Some(t)) => t,
                (Some(c), None) => format!("Community {}", short(c)),
                (None, None) => "Members of no community below".to_string(),
            };
            let mut j = json!({
                "id": child.encode(),
                "label": label,
                "size": g.size,
                "top": top,
                "community": g.community,
                "level": level,
                "children": g.children,
                "report": g.report.as_ref().map(|r| json!({
                    "iri": r.iri,
                    "title": r.title,
                    "status": r.status,
                })),
            });
            if g.community.is_none() {
                j["folded"] = true.into();
            }
            out.push(j);
        }
        let mut edges = Edges::default();
        for outgoing in [true, false] {
            self.outward(&mut edges, outgoing, None)?;
        }
        let (edges, edges_truncated) = edges.json();
        Ok(json!({
            "grouping": { "kind": "reports", "predicate": null, "level": level },
            "communities": false,
            "groups": out,
            "edges": edges,
            "truncated": truncated || edges_truncated,
        }))
    }

    /// The counted links from the set's groups, or from `members` when given, to the
    /// other nodes of the picture, in one direction.
    fn outward(&self, edges: &mut Edges, outgoing: bool, members: Option<&[String]>) -> Result<()> {
        let s = &self.set;
        let n = s.path.len();
        let mut keys = String::new();
        let mut vars = Vec::new();
        for i in 0..=n {
            let v = format!("?k{i}");
            keys.push_str(&s.key(i, "b", &v));
            vars.push(v);
        }
        let (head, side) = match members {
            Some(m) => (
                format!(
                    "VALUES ?a {{ {} }}\n",
                    m.iter()
                        .map(|i| format!("<{i}>"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                "?a".to_string(),
            ),
            None => {
                keys.push_str(&s.key(n, "a", "?ka"));
                (s.pattern("a"), "?ka".to_string())
            }
        };
        let q = format!(
            "SELECT {side} ?p {vars} (COUNT(*) AS ?cnt) WHERE {{\n{{ SELECT DISTINCT ?a ?p ?b WHERE {{\n{head}{link}}} }}\n{keys}}} GROUP BY {side} ?p {vars} LIMIT {EDGE_ROWS}",
            vars = vars.join(" "),
            link = link(outgoing),
        );
        let r = self.run.select(&q)?;
        if r.rows.len() >= EDGE_ROWS {
            edges.truncated = true;
        }
        let sc = r.col(&side[1..]);
        let pc = r.col("p");
        let nc = r.col("cnt");
        let kc: Vec<usize> = vars.iter().map(|v| r.col(&v[1..])).collect();
        for row in &r.rows {
            let me = match members {
                Some(_) => match iri_of(get(row, sc)) {
                    Some(i) => format!("m:{i}"),
                    None => continue,
                },
                None => format!("g:{}", s.child(iri_of(get(row, sc))).encode()),
            };
            let ks: Vec<Option<&str>> = kc.iter().map(|c| iri_of(get(row, *c))).collect();
            let other = match s.owner(&ks) {
                Owner::Outside => continue,
                Owner::Sibling(set) => format!("g:{}", set.encode()),
                // a link inside the set is counted once, outgoing, and the members
                // among themselves on their own
                Owner::Inside(_) if !outgoing || members.is_some() => continue,
                Owner::Inside(k) => format!("g:{}", s.child(k.as_deref()).encode()),
            };
            let p = iri_of(get(row, pc));
            let c = count(get(row, nc));
            if outgoing {
                edges.add(me, other, p, c);
            } else {
                edges.add(other, me, p, c);
            }
        }
        Ok(())
    }

    fn members_answer(&self, limit: usize, size: u64) -> Result<J> {
        let s = &self.set;
        let q = format!(
            "SELECT ?m (SAMPLE(?l) AS ?label) WHERE {{ {{ SELECT DISTINCT ?m WHERE {{\n{}}} ORDER BY ?m LIMIT {} }}\n{}}} GROUP BY ?m",
            s.pattern("m"),
            limit + 1,
            labels("m")
        );
        let r = self.run.select(&q)?;
        let (mc, lc) = (r.col("m"), r.col("label"));
        let mut members: Vec<(String, Option<String>)> = r
            .rows
            .iter()
            .filter_map(|row| Some((iri_of(get(row, mc))?.to_string(), lit(get(row, lc)))))
            .collect();
        members.sort();
        let mut truncated = members.len() > limit || size > limit as u64;
        members.truncate(limit);
        let iris: Vec<String> = members.iter().map(|(i, _)| i.clone()).collect();
        let mut edges = Edges::default();
        if !iris.is_empty() {
            // the members among themselves
            let values = iris
                .iter()
                .map(|i| format!("<{i}>"))
                .collect::<Vec<_>>()
                .join(" ");
            let q = format!(
                "SELECT DISTINCT ?a ?p ?b WHERE {{\nVALUES ?a {{ {values} }}\n{}{}}} LIMIT {}",
                link(true),
                s.pattern("b"),
                MAX_EDGES + 1
            );
            let r = self.run.select(&q)?;
            if r.rows.len() > MAX_EDGES {
                truncated = true;
            }
            let listed: HashSet<&str> = iris.iter().map(String::as_str).collect();
            let (ac, pc, bc) = (r.col("a"), r.col("p"), r.col("b"));
            for row in &r.rows {
                let (Some(a), Some(b)) = (iri_of(get(row, ac)), iri_of(get(row, bc))) else {
                    continue;
                };
                if listed.contains(a) && listed.contains(b) {
                    edges.add(format!("m:{a}"), format!("m:{b}"), iri_of(get(row, pc)), 1);
                }
            }
            for outgoing in [true, false] {
                self.outward(&mut edges, outgoing, Some(&iris))?;
            }
        }
        let (edges, edges_truncated) = edges.json();
        let members: Vec<J> = members
            .into_iter()
            .map(|(iri, label)| {
                let mut m = json!({ "iri": iri });
                if let Some(l) = label {
                    m["label"] = l.into();
                }
                m
            })
            .collect();
        Ok(json!({
            "grouping": { "kind": "members", "predicate": null },
            "communities": false,
            "groups": [],
            "members": members,
            "edges": edges,
            "truncated": truncated || edges_truncated,
        }))
    }
}

/// The last part of a community IRI, for a group without a report title.
fn short(iri: &str) -> String {
    let tail = iri.rsplit([':', '/', '#']).next().unwrap_or(iri);
    tail.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Set {
        Set {
            graph: DEFAULT_REPORTS_GRAPH.into(),
            level: 0,
            path: vec!["urn:c:a".into(), "urn:c:a1".into()],
            folded: false,
        }
    }

    #[test]
    fn ids_round_trip() {
        let s = sample();
        let id = s.encode();
        assert!(is_reports_id(&id));
        assert_eq!(Set::decode(&id).unwrap(), s);
        let f = s.with(s.path.clone(), true);
        assert_eq!(Set::decode(&f.encode()).unwrap(), f);
    }

    #[test]
    fn made_up_ids_are_refused() {
        let enc = |j: J| format!("r1.{}", URL_SAFE_NO_PAD.encode(j.to_string()));
        for bad in [
            "r1.",
            "r1.!!",
            &enc(json!({ "g": DEFAULT_REPORTS_GRAPH, "l": 0, "p": [] })),
            &enc(json!({ "g": "http://ex/g", "l": 0, "p": ["urn:c"] })),
            &enc(json!({ "g": DISCLOSURE_GRAPH, "l": 0, "p": ["urn:c"] })),
            &enc(json!({ "g": DEFAULT_REPORTS_GRAPH, "l": 0, "p": ["urn:c> } DROP ALL { <x"] })),
            &enc(json!({ "g": DEFAULT_REPORTS_GRAPH, "l": 15, "p": ["urn:a", "urn:b"] })),
            &enc(json!({ "g": DEFAULT_REPORTS_GRAPH, "l": 0, "p": ["urn:a"], "x": 1 })),
        ] {
            assert!(Set::decode(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn owners_follow_the_path() {
        let s = sample();
        // outside the start level
        assert!(matches!(s.owner(&[None, None, None]), Owner::Outside));
        // a sibling at the start level
        match s.owner(&[Some("urn:c:b"), None, None]) {
            Owner::Sibling(x) => assert_eq!(x.path, ["urn:c:b"]),
            _ => panic!(),
        }
        // in the first community but folded there
        match s.owner(&[Some("urn:c:a"), None, None]) {
            Owner::Sibling(x) => assert!(x.folded && x.path == ["urn:c:a"]),
            _ => panic!(),
        }
        // inside, in a child or folded
        assert!(matches!(
            s.owner(&[Some("urn:c:a"), Some("urn:c:a1"), Some("urn:c:k")]),
            Owner::Inside(Some(k)) if k == "urn:c:k"
        ));
        assert!(matches!(
            s.owner(&[Some("urn:c:a"), Some("urn:c:a1"), None]),
            Owner::Inside(None)
        ));
        // the folded members' set has the children as siblings
        let f = s.with(s.path.clone(), true);
        match f.owner(&[Some("urn:c:a"), Some("urn:c:a1"), Some("urn:c:k")]) {
            Owner::Sibling(x) => assert_eq!(x.path, ["urn:c:a", "urn:c:a1", "urn:c:k"]),
            _ => panic!(),
        }
    }
}
