//! The JSON forms of the ontology handle's answers, which the HTTP routes, the MCP tool
//! and the Python, Node.js and JVM bindings share, and [`call`], which reads a request
//! in the same form and answers it.
//!
//! A request is a JSON object. `graph` names the graph read, as a string or a list
//! (`"default"` or an IRI), `imports` is a boolean, `mode` is `lenient` or `strict`,
//! `at` names a state as queries do (`commit:N`, a snapshot or a time), and
//! `inferences` reads the materialized inferences with the ontology. The other members
//! depend on the operation and are listed on [`call`]. An axiom is given in its JSON form
//! ([`sparkles_owl::json`]) or as a string in functional-style syntax, with the dataset's
//! prefixes and the standard ones.

use super::{
    CheckRequest, EditOutcome, EditRequest, OntologyDiff, OntologyRequest, Opened, check_opened,
    parse_axiom_types, parse_profiles, report_json,
};
use crate::Dataset;
use crate::error::{Error, Result};
use crate::history::At;
use crate::sparql::QueryOptions;
use serde_json::{Map, Value as J, json};
use sparkles_owl::ofn::Renderer;
use sparkles_owl::profiles::{Profile, Report};
use sparkles_owl::{Axiom, AxiomType, read};
use sparkles_reasoner::inputs::{GraphRef, ImportMode};
use std::collections::BTreeSet;

/// The most axioms one page lists.
pub const MAX_LIMIT: usize = 10_000;
/// The axioms a page lists by default.
pub const DEFAULT_LIMIT: usize = 100;
/// The diagnostics a summary lists.
pub const SUMMARY_DIAGNOSTICS: usize = 100;
/// The axioms each side of a diff lists by default.
pub const DIFF_LIMIT: usize = 1000;

/// A renderer with the dataset's prefixes, for functional-style output.
pub fn renderer(ds: &Dataset) -> Renderer {
    Renderer::new(ds.prefixes())
}

fn header_json(h: &sparkles_owl::Header, r: &Renderer) -> J {
    json!({
        "iri": h.iri.as_ref().map(|i| i.as_str()),
        "versionIri": h.version_iri.as_ref().map(|i| i.as_str()),
        "imports": h.imports.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
        "annotations": h.annotations.iter().map(|a| r.annotation(a)).collect::<Vec<_>>(),
    })
}

/// The members every answer about an open ontology carries: the dataset, the commit,
/// the mode, the graphs read, the imports, the warnings of the read and, for an inferred
/// view, the inferences read.
pub fn opened_json(name: &str, o: &Opened, req: &OntologyRequest) -> J {
    let mut j = json!({
        "dataset": name,
        "commit": o.snapshot.commit,
        "mode": req.mode.as_str(),
        "graphs": o.graphs.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
        "imports": o.imports.iter().map(|i| json!({"iri": i.iri, "graph": i.graph})).collect::<Vec<_>>(),
        "warnings": o.warnings,
    });
    if req.inferences {
        j["inferences"] = match &o.inferences {
            None => json!({ "included": false }),
            Some(i) => json!({
                "included": true,
                "graph": o.graph_name(i.graph),
                "profile": i.profile,
                "stale": i.stale,
                "commitsSince": i.commits_since,
                "reason": i.reason,
            }),
        };
    }
    j
}

/// The JSON of the summary of an open ontology, as `GET /$/ontology/{ds}` answers it.
pub fn summary_body(name: &str, o: &Opened, req: &OntologyRequest, r: &Renderer) -> J {
    let s = o.summary();
    let mut j = opened_json(name, o, req);
    j["header"] = header_json(&s.header, r);
    j["axioms"] = json!(
        s.axioms
            .iter()
            .map(|(t, n)| (t.name().to_string(), json!(*n)))
            .collect::<Map<String, J>>()
    );
    j["axiomCount"] = json!(s.axioms.values().sum::<usize>());
    j["entities"] = json!(
        s.entities
            .iter()
            .map(|(k, n)| (k.name().to_string(), json!(*n)))
            .collect::<Map<String, J>>()
    );
    j["triples"] = json!(s.triples);
    j["unmapped"] = json!(s.unmapped);
    j["diagnostics"] = json!({
        "warnings": s.warnings,
        "errors": s.errors,
        "list": o.ontology.diagnostics.iter().take(SUMMARY_DIAGNOSTICS).map(|d| json!({
            "severity": d.severity.as_str(),
            "code": d.code,
            "message": d.message,
        })).collect::<Vec<_>>(),
    });
    j["cached"] = json!(o.cached);
    j
}

/// What a listing of axioms keeps.
#[derive(Clone, Debug)]
pub struct AxiomQuery {
    pub types: Option<BTreeSet<AxiomType>>,
    pub about: Option<String>,
    pub assertions: bool,
    pub limit: usize,
    pub offset: usize,
}

impl Default for AxiomQuery {
    fn default() -> AxiomQuery {
        AxiomQuery {
            types: None,
            about: None,
            assertions: false,
            limit: DEFAULT_LIMIT,
            offset: 0,
        }
    }
}

/// One page of axioms, with the graph each was read from, and the total.
pub struct Page {
    pub items: Vec<(Axiom, Option<u32>)>,
    pub total: usize,
    pub offset: usize,
}

/// The page `q` asks for: the terminology's axioms in order, then the assertions the
/// store holds when `q.assertions` is set.
pub fn axiom_page(o: &Opened, q: &AxiomQuery, opts: &QueryOptions) -> Result<Page> {
    let keep = |ax: &Axiom| {
        q.types
            .as_ref()
            .is_none_or(|t| t.contains(&ax.axiom_type()))
            && q.about.as_deref().is_none_or(|a| ax.mentions(a))
    };
    let mut items: Vec<(Axiom, Option<u32>)> = Vec::new();
    let mut total = 0usize;
    for (ax, origins) in &o.ontology.axioms {
        if keep(ax) {
            if total >= q.offset && items.len() < q.limit {
                items.push((ax.clone(), origins.first().map(|x| x.graph)));
            }
            total += 1;
        }
    }
    let wants_assertions = q
        .types
        .as_ref()
        .is_none_or(|t| t.iter().any(|t| t.is_assertion()));
    if q.assertions && wants_assertions {
        o.assertions(opts, |ax, _, g| {
            if let Some(ax) = ax
                && keep(&ax)
            {
                if total >= q.offset && items.len() < q.limit {
                    items.push((ax, Some(g)));
                }
                total += 1;
            }
            Ok(true)
        })?;
    }
    Ok(Page {
        items,
        total,
        offset: q.offset,
    })
}

/// The JSON of one axiom of a listing or a diff: its type, its functional-style form,
/// its JSON form and the graph it is in.
pub fn axiom_json(ax: &Axiom, graph: Option<&str>, r: &Renderer) -> J {
    let mut j = json!({
        "type": ax.axiom_type().name(),
        "ofn": r.axiom(ax),
        "axiom": sparkles_owl::json::axiom(ax),
    });
    if let Some(g) = graph {
        j["graph"] = g.into();
    }
    j
}

/// The JSON of a page of axioms.
pub fn page_body(name: &str, o: &Opened, req: &OntologyRequest, r: &Renderer, page: &Page) -> J {
    let next = page.offset + page.items.len();
    let list: Vec<J> = page
        .items
        .iter()
        .map(|(ax, g)| axiom_json(ax, g.map(|g| o.graph_name(g)), r))
        .collect();
    let mut j = opened_json(name, o, req);
    j["axioms"] = J::Array(list);
    j["total"] = json!(page.total);
    j["offset"] = json!(page.offset);
    j["next"] = if next < page.total {
        json!(next)
    } else {
        J::Null
    };
    j
}

/// The JSON of the reports of a check.
pub fn check_body(
    name: &str,
    o: &Opened,
    req: &OntologyRequest,
    r: &Renderer,
    reports: &[Report],
    assertions: bool,
) -> J {
    let mut j = opened_json(name, o, req);
    j["conforms"] = json!(reports.iter().all(|r| r.conforms));
    j["assertions"] = json!(assertions);
    j["reports"] = J::Array(reports.iter().map(|x| report_json(o, x, r)).collect());
    j
}

/// The JSON of what an edit did.
pub fn edit_body(e: &EditOutcome, r: &Renderer) -> J {
    json!({
        "added": e.added,
        "present": e.present,
        "removed": e.removed,
        "absent": e.absent.iter().map(|a| r.axiom(a)).collect::<Vec<_>>(),
        "declared": e.declared.iter().map(|d| r.entity(d)).collect::<Vec<_>>(),
        "inserted": e.inserted,
        "deleted": e.deleted,
        "kept": e.kept,
        "committed": e.receipt.committed,
    })
}

/// Whether the ontology's terminology declares or uses `iri`.
pub fn mentioned(o: &Opened, iri: &str) -> bool {
    o.ontology.axiom_set().any(|a| a.mentions(iri))
}

/// The JSON of an ontology diff, with at most `limit` axioms on each side.
pub fn diff_body(
    name: &str,
    d: &OntologyDiff,
    req: &OntologyRequest,
    assertions: bool,
    limit: usize,
    r: &Renderer,
) -> J {
    let side = |v: &[super::AxiomChange]| -> J {
        J::Array(
            v.iter()
                .take(limit)
                .map(|c| axiom_json(&c.axiom, Some(&c.graph), r))
                .collect(),
        )
    };
    json!({
        "dataset": name,
        "from": d.from,
        "to": d.to,
        "graphs": req.graphs.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
        "mode": req.mode.as_str(),
        "assertions": assertions,
        "added": side(&d.added),
        "removed": side(&d.removed),
        "addedCount": d.added.len(),
        "removedCount": d.removed.len(),
        "truncated": d.added.len() > limit || d.removed.len() > limit,
        "changedQuads": d.changed_quads,
        "method": d.method.as_str(),
    })
}

/// The diff as lines: `+ axiom` for each added axiom, then `- axiom` for each removed
/// one, in functional-style syntax with the dataset's prefixes.
pub fn diff_lines(d: &OntologyDiff, limit: usize, r: &Renderer) -> String {
    let mut s = String::new();
    for c in d.added.iter().take(limit) {
        s.push_str("+ ");
        s.push_str(&r.axiom(&c.axiom));
        s.push('\n');
    }
    for c in d.removed.iter().take(limit) {
        s.push_str("- ");
        s.push_str(&r.axiom(&c.axiom));
        s.push('\n');
    }
    s
}

// ------------------------------------------------------------- requests ------

/// The operations [`call`] answers, in the names of the bindings' surface keys.
pub const OPERATIONS: [&str; 6] = ["summary", "axioms", "check", "edit", "removeEntity", "diff"];

/// The members each operation reads besides those that name the ontology.
fn members(op: &str) -> &'static [&'static str] {
    match op {
        "summary" => &[],
        "axioms" => &["type", "about", "assertions", "limit", "offset"],
        "check" => &["profiles", "assertions", "maxViolations"],
        "edit" => &["add", "remove", "declare", "target", "message"],
        "removeEntity" => &["iri", "message"],
        "diff" => &["from", "to", "assertions", "limit"],
        _ => &[],
    }
}

const ONTOLOGY_MEMBERS: [&str; 5] = ["graph", "imports", "mode", "at", "inferences"];

fn bad(m: impl Into<String>) -> Error {
    Error::invalid(m.into())
}

fn graph_ref(s: &str) -> Result<GraphRef> {
    GraphRef::parse(s.trim_start_matches('<').trim_end_matches('>')).map_err(bad)
}

fn boolean(a: &Map<String, J>, k: &str) -> Result<Option<bool>> {
    match a.get(k) {
        None | Some(J::Null) => Ok(None),
        Some(J::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(bad(format!("{k}: true or false"))),
    }
}

fn string<'a>(a: &'a Map<String, J>, k: &str) -> Result<Option<&'a str>> {
    match a.get(k) {
        None | Some(J::Null) => Ok(None),
        Some(J::String(s)) => Ok(Some(s)),
        Some(_) => Err(bad(format!("{k}: a string"))),
    }
}

fn number(a: &Map<String, J>, k: &str, min: usize, max: usize) -> Result<Option<usize>> {
    match a.get(k) {
        None | Some(J::Null) => Ok(None),
        Some(v) => {
            // the bindings send 64-bit integers as strings or numbers
            let n = match v {
                J::Number(n) => n.as_u64(),
                J::String(s) => s.parse::<u64>().ok(),
                _ => None,
            };
            n.filter(|n| (min as u64..=max as u64).contains(n))
                .map(|n| n as usize)
                .map(Some)
                .ok_or_else(|| bad(format!("{k}: an integer from {min} to {max}")))
        }
    }
}

fn at_of(v: &J, k: &str) -> Result<At> {
    match v {
        J::String(s) => s.parse::<At>().map_err(|e| bad(format!("{k}: {e}"))),
        J::Number(n) => n
            .as_u64()
            .map(At::Commit)
            .ok_or_else(|| bad(format!("{k}: a commit number or a selector"))),
        _ => Err(bad(format!("{k}: a commit number or a selector"))),
    }
}

/// The ontology the members of `a` name: `graph`, `imports`, `mode`, `at` and
/// `inferences`.
pub fn request_of(a: &Map<String, J>) -> Result<OntologyRequest> {
    let mut req = OntologyRequest::default();
    match a.get("graph") {
        None | Some(J::Null) => {}
        Some(J::String(g)) => req.graphs = vec![graph_ref(g)?],
        Some(J::Array(v)) => {
            req.graphs = v
                .iter()
                .map(|g| {
                    g.as_str()
                        .ok_or_else(|| bad("graph: a string or a list of strings"))
                        .and_then(graph_ref)
                })
                .collect::<Result<_>>()?;
        }
        Some(_) => return Err(bad("graph: a string or a list of strings")),
    }
    if let Some(b) = boolean(a, "imports")? {
        req.imports = if b {
            ImportMode::Dataset
        } else {
            ImportMode::None
        };
    }
    if let Some(m) = string(a, "mode")? {
        req.mode = read::Mode::parse(m)
            .ok_or_else(|| bad(format!("mode must be lenient or strict, not '{m}'")))?;
    }
    if let Some(v) = a.get("at").filter(|v| !v.is_null()) {
        req.at = Some(at_of(v, "at")?);
    }
    req.inferences = boolean(a, "inferences")?.unwrap_or(false);
    Ok(req)
}

/// An axiom in its JSON form or in functional-style syntax with `prefixes`.
pub fn axiom_of(v: &J, at: &str, prefixes: &[(String, String)]) -> Result<Axiom> {
    match v {
        J::String(s) => {
            sparkles_owl::ofn_read::parse_axiom(s, prefixes).map_err(|e| bad(format!("{at}: {e}")))
        }
        J::Object(_) => sparkles_owl::json::read_axiom(v).map_err(|e| bad(format!("{at}: {e}"))),
        _ => Err(bad(format!(
            "{at}: an axiom in functional-style syntax or in its JSON form"
        ))),
    }
}

fn axioms_of(a: &Map<String, J>, k: &str, prefixes: &[(String, String)]) -> Result<Vec<Axiom>> {
    match a.get(k) {
        None | Some(J::Null) => Ok(Vec::new()),
        Some(J::Array(v)) => v
            .iter()
            .enumerate()
            .map(|(i, x)| axiom_of(x, &format!("{k}[{i}]"), prefixes))
            .collect(),
        Some(_) => Err(bad(format!("{k}: a list of axioms"))),
    }
}

fn profiles_of(a: &Map<String, J>) -> Result<Vec<Profile>> {
    let ps = match a.get("profiles") {
        None | Some(J::Null) => Vec::new(),
        Some(J::String(s)) => parse_profiles(s)?,
        Some(J::Array(v)) => {
            let mut ps = Vec::new();
            for x in v {
                let s = x
                    .as_str()
                    .ok_or_else(|| bad("profiles: a list of el, ql, rl and dl"))?;
                ps.extend(parse_profiles(s)?);
            }
            ps
        }
        Some(_) => return Err(bad("profiles: a list of el, ql, rl and dl")),
    };
    Ok(if ps.is_empty() { vec![Profile::Dl] } else { ps })
}

/// Answer the ontology operation `op` with the arguments `args`, a JSON object, in the
/// JSON forms of the HTTP routes. The operations and their members:
///
/// - `summary`: the summary of `GET /$/ontology/{ds}`.
/// - `axioms`: a page of `GET /$/ontology/{ds}/axioms`, with `type` (a list such as
///   `SubClassOf,ClassAssertion`), `about` (an IRI), `assertions`, `limit` and
///   `offset`. `about` an IRI the ontology neither declares nor uses is `NotFound`.
/// - `check`: the reports of `POST /$/ontology/{ds}/check`, with `profiles`,
///   `assertions` and `maxViolations`.
/// - `edit`: what `POST /$/ontology/{ds}/axioms` did, with `add`, `remove`, `declare`,
///   `target` and `message`. `opts.write` carries the commit's other options.
/// - `removeEntity`: an edit that removes the entity `iri`.
/// - `diff`: the diff of `GET /$/ontology/{ds}/diff`, with `from`, `to` (the head by
///   default), `assertions` and `limit`.
pub fn call(ds: &Dataset, op: &str, args: &J, opts: &QueryOptions) -> Result<J> {
    let empty = Map::new();
    let a = match args {
        J::Null => &empty,
        J::Object(o) => o,
        _ => return Err(bad("the arguments are a JSON object")),
    };
    if !OPERATIONS.contains(&op) {
        return Err(bad(format!(
            "unknown ontology operation '{op}': {}",
            OPERATIONS.join(", ")
        )));
    }
    let allowed = members(op);
    if let Some(k) = a
        .keys()
        .find(|k| !ONTOLOGY_MEMBERS.contains(&k.as_str()) && !allowed.contains(&k.as_str()))
    {
        return Err(bad(format!("unknown member '{k}' of ontology.{op}")));
    }
    let req = request_of(a)?;
    let name = ds.name().unwrap_or("");
    let handle = ds.ontology();
    let r = renderer(ds);
    let prefixes: Vec<(String, String)> = ds.prefixes().into_iter().collect();
    match op {
        "summary" => {
            let opts = handle.anchored(opts)?;
            let o = handle.open(&req, &opts)?;
            Ok(summary_body(name, &o, &req, &r))
        }
        "axioms" => {
            let q = AxiomQuery {
                types: string(a, "type")?.map(parse_axiom_types).transpose()?,
                about: string(a, "about")?
                    .map(|s| s.trim_start_matches('<').trim_end_matches('>').to_string()),
                assertions: boolean(a, "assertions")?.unwrap_or(false),
                limit: number(a, "limit", 1, MAX_LIMIT)?.unwrap_or(DEFAULT_LIMIT),
                offset: number(a, "offset", 0, usize::MAX)?.unwrap_or(0),
            };
            let opts = handle.anchored(opts)?;
            let o = handle.open(&req, &opts)?;
            let page = axiom_page(&o, &q, &opts)?;
            if let Some(about) = &q.about
                && page.total == 0
                && !mentioned(&o, about)
            {
                return Err(Error::NotFound(format!(
                    "the ontology neither declares nor uses <{about}>"
                )));
            }
            Ok(page_body(name, &o, &req, &r, &page))
        }
        "check" => {
            let check = CheckRequest {
                profiles: profiles_of(a)?,
                assertions: boolean(a, "assertions")?.unwrap_or(true),
                max_violations: number(a, "maxViolations", 1, MAX_LIMIT)?
                    .unwrap_or(CheckRequest::default().max_violations),
            };
            let opts = handle.anchored(opts)?;
            let o = handle.open(&req, &opts)?;
            let reports = check_opened(&o, &check, &opts)?;
            Ok(check_body(name, &o, &req, &r, &reports, check.assertions))
        }
        "edit" => {
            let edit = EditRequest {
                add: axioms_of(a, "add", &prefixes)?,
                remove: axioms_of(a, "remove", &prefixes)?,
                declare: boolean(a, "declare")?,
                graph: string(a, "target")?.map(graph_ref).transpose()?,
                message: string(a, "message")?.map(str::to_string),
            };
            if edit.add.is_empty() && edit.remove.is_empty() {
                return Err(bad("name axioms to add or remove"));
            }
            let e = handle.edit(&req, &edit, opts)?;
            Ok(edit_body(&e, &r))
        }
        "removeEntity" => {
            let iri = string(a, "iri")?
                .ok_or_else(|| bad("iri names the entity to remove"))?
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string();
            let mut opts = opts.clone();
            if let Some(m) = string(a, "message")? {
                opts.write.message = Some(std::sync::Arc::from(m));
            }
            let e = handle.remove_entity(&req, &iri, &opts)?;
            Ok(edit_body(&e, &r))
        }
        "diff" => {
            if req.at.is_some() {
                return Err(bad("a diff takes from and to, not at"));
            }
            let from = at_of(
                a.get("from")
                    .filter(|v| !v.is_null())
                    .ok_or_else(|| bad("from names the earlier commit"))?,
                "from",
            )?;
            let to = match a.get("to").filter(|v| !v.is_null()) {
                None => At::Head,
                Some(v) => at_of(v, "to")?,
            };
            let assertions = boolean(a, "assertions")?.unwrap_or(false);
            let limit = number(a, "limit", 1, MAX_LIMIT)?.unwrap_or(DIFF_LIMIT);
            let d = handle.diff(&req, &from, &to, assertions, opts)?;
            Ok(diff_body(name, &d, &req, assertions, limit, &r))
        }
        _ => unreachable!("checked above"),
    }
}
