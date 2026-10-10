//! The reads behind the maintenance tasks of C18 Phase 5, as `Tools` methods that MCP
//! does not offer as tools. The consolidation task and the retention task of
//! `ingest::maintain` run them as their caller, so they see only the caller's view.
//!
//! - `memory_consolidation_scan` reads the live facts of every agent graph and returns
//!   those that at least `minSources` distinct sources assert and no reviewed graph
//!   asserts yet, with the reifiers a consolidated fact derives from (§8.3). A graph
//!   that is `mem:copyOf` another graph asserting the same fact counts once. It also
//!   returns the labelled entities and the subjects of agent memory, which the task
//!   checks for duplicates and conflicts.
//! - `memory_retention_scan` reads the session graphs of agent memory with the time of
//!   their newest fact and the facts that no reviewed graph asserts, and says which
//!   graphs the retention of §8.4 deletes.

use super::inbox::{ReifiedFact, asserting_graphs, copy_links, plain_facts, reified_facts};
use super::{PROV, RDF_TYPE, Reader, SPK, values_iris};
use crate::mcp::Outcome;
use crate::mcp::errors::ToolError;
use crate::mcp::tools::{Tools, bounded, parse};
use oxrdf::{NamedNode, Term};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::error::Error;
use std::collections::{BTreeMap, BTreeSet, HashMap};

const MEM: &str = "urn:x-sparkles:mem:";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const DCT: &str = "http://purl.org/dc/terms/";
/// The agent graphs a scan reads at most.
const MAX_GRAPHS: usize = 5000;
/// The facts a consolidation scan reads by default and at most.
const SCAN_FACTS: u64 = 20_000;
const MAX_SCAN_FACTS: u64 = 100_000;
/// The repeated facts one scan returns at most.
const MAX_REPEATED: usize = 2000;
/// The entities and subjects a scan returns for the duplicate and conflict checks.
const MAX_ENTITIES: usize = 200;
/// The facts of one session graph that retention checks at most; a larger graph is
/// kept.
const RETENTION_FACTS: usize = 5000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConsolidationArgs {
    dataset: Option<String>,
    min_sources: Option<u64>,
    limit: Option<u64>,
    timeout_seconds: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RetentionArgs {
    dataset: Option<String>,
    /// the age after which a session graph is deleted, such as `365d`
    after: String,
    graphs: Option<Vec<String>>,
    require_consolidated: Option<bool>,
    timeout_seconds: Option<f64>,
}

/// One triple of agent memory with the graphs and reifiers that assert it.
struct Repeated {
    fact: ReifiedFact,
    graphs: BTreeSet<String>,
    reifiers: BTreeSet<String>,
}

/// The agent graphs of the reader's view.
fn agent_graphs(
    r: &Reader,
    memory: &crate::assist::MemorySettings,
) -> Result<Vec<NamedNode>, Error> {
    let mut out = Vec::new();
    if memory.agent_graphs.is_empty() {
        return Ok(out);
    }
    let q = format!("SELECT DISTINCT ?g WHERE {{ GRAPH ?g {{ ?s ?p ?o }} }} LIMIT {MAX_GRAPHS}");
    for row in r.rows(&q, Vec::new())? {
        if let Some(Some(Term::NamedNode(g))) = row.first()
            && memory.is_agent_graph(g.as_str())
        {
            out.push(g.clone());
        }
    }
    Ok(out)
}

/// Whether a triple describes memory itself (a session, a source, a chunk) rather than
/// a fact the agent learned.
fn structural(f: &ReifiedFact) -> bool {
    let p = f.p.as_str();
    if p.starts_with(MEM) || p.starts_with(SPK) || p.starts_with(PROV) {
        return true;
    }
    p == RDF_TYPE
        && matches!(&f.o, Term::NamedNode(o)
            if o.as_str().starts_with(MEM) || o.as_str().starts_with(SPK) || o.as_str().starts_with(PROV))
}

/// The number of distinct sources among `graphs`: a graph that is a copy of another
/// graph of the set does not count.
fn distinct_sources(
    graphs: &BTreeSet<String>,
    copies: &HashMap<String, std::collections::HashSet<String>>,
) -> usize {
    graphs
        .iter()
        .filter(|g| {
            !copies
                .get(*g)
                .is_some_and(|o| graphs.iter().any(|h| h != *g && o.contains(h)))
        })
        .count()
}

impl Tools<'_> {
    pub(crate) fn memory_consolidation_scan(
        &self,
        args: Map<String, Value>,
    ) -> Result<Outcome, ToolError> {
        let a: ConsolidationArgs = parse(args)?;
        let min = bounded("minSources", a.min_sources, 2, 2, 100)? as usize;
        let limit = bounded("limit", a.limit, SCAN_FACTS, 1, MAX_SCAN_FACTS)? as usize;
        let timeout = self.timeout(a.timeout_seconds)?;
        let p = self.call.principal.clone().on_branch(None);
        let ds = self.server.main_dataset(&p, a.dataset.as_deref())?;
        let ctx = self.ctx(&[], timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let r = self.reader(&ds, None, None, Some(false), deadline, &ctx)?;
        let eng = |e: Error| ctx.engine(e);
        let memory = crate::assist::memory_settings(&self.server.state, &ds);
        let graphs = agent_graphs(&r, &memory).map_err(eng)?;
        // the live facts of agent memory, reified ones first
        let mut facts: Vec<ReifiedFact> = Vec::new();
        for chunk in graphs.chunks(100) {
            let room = (limit + 1).saturating_sub(facts.len());
            if room == 0 {
                break;
            }
            facts.extend(reified_facts(&r, chunk, true, room).map_err(eng)?);
        }
        for chunk in graphs.chunks(100) {
            let room = (limit + 1).saturating_sub(facts.len());
            if room == 0 {
                break;
            }
            facts.extend(plain_facts(&r, chunk, room).map_err(eng)?);
        }
        let truncated = facts.len() > limit;
        facts.truncate(limit);
        let scanned = facts.len();
        facts.retain(|f| {
            !structural(f)
                && !matches!(f.s, Term::BlankNode(_))
                && !matches!(f.o, Term::BlankNode(_) | Term::Triple(_))
        });
        // the subjects and labelled entities, for the duplicate and conflict checks
        let mut subjects: Vec<NamedNode> = Vec::new();
        for f in &facts {
            if let Term::NamedNode(n) = &f.s
                && !subjects.contains(n)
                && subjects.len() < MAX_ENTITIES
            {
                subjects.push(n.clone());
            }
        }
        // by triple: its graphs and reifiers
        let mut by_triple: BTreeMap<String, Repeated> = BTreeMap::new();
        for f in facts {
            let key = format!("{} {} {}", f.s, f.p, f.o);
            let e = by_triple.entry(key).or_insert_with(|| Repeated {
                fact: f.clone(),
                graphs: BTreeSet::new(),
                reifiers: BTreeSet::new(),
            });
            e.graphs.insert(f.g.as_str().to_string());
            for rf in &f.reifiers {
                e.reifiers.insert(rf.as_str().to_string());
            }
        }
        let copies = copy_links(&r, &graphs).map_err(eng)?;
        let mut repeated: Vec<Repeated> = by_triple
            .into_values()
            .filter(|x| distinct_sources(&x.graphs, &copies) >= min)
            .collect();
        // a triple a reviewed graph already asserts is consolidated
        let reps: Vec<ReifiedFact> = repeated.iter().map(|x| x.fact.clone()).collect();
        let asserted = asserting_graphs(&r, &reps).map_err(eng)?;
        repeated.retain(|x| {
            let key = format!("{} {} {}", x.fact.s, x.fact.p, x.fact.o);
            asserted
                .get(&key)
                .is_none_or(|gs| gs.iter().all(|g| memory.is_agent_graph(g)))
        });
        repeated.sort_by(|a, b| {
            distinct_sources(&b.graphs, &copies)
                .cmp(&distinct_sources(&a.graphs, &copies))
                .then_with(|| a.graphs.cmp(&b.graphs))
        });
        let more = repeated.len() > MAX_REPEATED;
        repeated.truncate(MAX_REPEATED);
        let list: Vec<Value> = repeated
            .iter()
            .map(|x| {
                json!({
                    "s": x.fact.s.to_string(),
                    "p": format!("<{}>", x.fact.p.as_str()),
                    "o": x.fact.o.to_string(),
                    "graphs": x.graphs,
                    "sources": distinct_sources(&x.graphs, &copies),
                    "derivedFrom": x.reifiers,
                })
            })
            .collect();
        let entities = labelled(&r, &subjects).map_err(eng)?;
        let mut out = json!({
            "dataset": ds.name,
            "commit": r.snap.commit,
            "agentGraphs": graphs.len(),
            "scanned": scanned,
            "truncated": truncated || more || graphs.len() >= MAX_GRAPHS,
            "minSources": min,
            "repeated": list,
            "entities": entities,
            "subjects": subjects.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        });
        if let Some(t) = &memory.consolidated_graph {
            out["target"] = t.clone().into();
        }
        Ok(Outcome::Structured(out))
    }

    pub(crate) fn memory_retention_scan(
        &self,
        args: Map<String, Value>,
    ) -> Result<Outcome, ToolError> {
        let a: RetentionArgs = parse(args)?;
        let after = crate::assist::duration_days(&a.after)
            .ok_or_else(|| ToolError::bad_argument("after is a duration such as 365d"))?;
        let timeout = self.timeout(a.timeout_seconds)?;
        let p = self.call.principal.clone().on_branch(None);
        let ds = self.server.main_dataset(&p, a.dataset.as_deref())?;
        let ctx = self.ctx(&[], timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let r = self.reader(&ds, None, None, Some(false), deadline, &ctx)?;
        let eng = |e: Error| ctx.engine(e);
        let memory = crate::assist::memory_settings(&self.server.state, &ds);
        let rule = crate::assist::Retention {
            after: a.after.clone(),
            graphs: a.graphs.clone().unwrap_or_default(),
            require_consolidated: a.require_consolidated.unwrap_or(true),
            every: None,
        };
        let sessions: Vec<NamedNode> = agent_graphs(&r, &memory)
            .map_err(eng)?
            .into_iter()
            .filter(|g| rule.covers(g.as_str()))
            .collect();
        // the newest time each graph records
        let mut newest: HashMap<String, String> = HashMap::new();
        for chunk in sessions.chunks(100) {
            let q = format!(
                "SELECT ?g (MAX(?t) AS ?at) WHERE {{ {} GRAPH ?g {{ ?x ?p ?t \
                 FILTER(isLiteral(?t) && DATATYPE(?t) = <http://www.w3.org/2001/XMLSchema#dateTime> \
                 && (STRSTARTS(STR(?p), \"{PROV}\") || STRSTARTS(STR(?p), \"{MEM}\") || ?p = <{DCT}modified> || ?p = <{DCT}created>)) }} }} GROUP BY ?g",
                values_iris("g", chunk)
            );
            for row in r.rows(&q, Vec::new()).map_err(eng)? {
                if let [Some(Term::NamedNode(g)), Some(Term::Literal(t))] = row.as_slice() {
                    newest.insert(g.as_str().to_string(), t.value().to_string());
                }
            }
        }
        let now = chrono::Utc::now();
        let mut graphs: Vec<Value> = Vec::new();
        let mut delete: Vec<String> = Vec::new();
        for g in &sessions {
            let at = newest.get(g.as_str());
            let age = at.and_then(|t| super::age_days(t, now));
            let mut j = json!({ "graph": g.as_str(), "newest": at });
            if let Some(a) = age {
                j["ageDays"] = ((a * 10.0).round() / 10.0).into();
            }
            let old = age.is_some_and(|a| a > after);
            if !old {
                j["kept"] = if age.is_none() { "no-time" } else { "recent" }.into();
                graphs.push(j);
                continue;
            }
            if rule.require_consolidated {
                let gs = std::slice::from_ref(g);
                let mut facts = reified_facts(&r, gs, true, RETENTION_FACTS + 1).map_err(eng)?;
                let room = (RETENTION_FACTS + 1).saturating_sub(facts.len());
                facts.extend(plain_facts(&r, gs, room).map_err(eng)?);
                if facts.len() > RETENTION_FACTS {
                    j["kept"] = "too-many-facts".into();
                    graphs.push(j);
                    continue;
                }
                facts.retain(|f| !structural(f));
                let asserted = asserting_graphs(&r, &facts).map_err(eng)?;
                let open = facts
                    .iter()
                    .filter(|f| {
                        let key = format!("{} {} {}", f.s, f.p, f.o);
                        asserted
                            .get(&key)
                            .is_none_or(|gs| gs.iter().all(|x| memory.is_agent_graph(x)))
                    })
                    .count();
                j["facts"] = facts.len().into();
                if open > 0 {
                    j["unconsolidated"] = open.into();
                    j["kept"] = "unconsolidated".into();
                    graphs.push(j);
                    continue;
                }
            }
            j["delete"] = true.into();
            delete.push(g.as_str().to_string());
            graphs.push(j);
        }
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "commit": r.snap.commit,
            "after": a.after,
            "afterDays": after,
            "requireConsolidated": rule.require_consolidated,
            "graphs": graphs,
            "delete": delete,
        })))
    }
}

/// The label and a type of each subject that has both, for the duplicate check.
fn labelled(r: &Reader, subjects: &[NamedNode]) -> Result<Vec<Value>, Error> {
    let mut out = Vec::new();
    for chunk in subjects.chunks(100) {
        let q = format!(
            "SELECT ?e (SAMPLE(?l) AS ?label) (SAMPLE(?t) AS ?type) WHERE {{ {} {} {} FILTER(isLiteral(?l) && isIRI(?t)) }} GROUP BY ?e",
            values_iris("e", chunk),
            r.quads(&format!("?e <{RDFS_LABEL}> ?l"), &[]),
            r.quads_in(&format!("?e <{RDF_TYPE}> ?t"), &[], "g2"),
        );
        for row in r.rows(&q, Vec::new())? {
            if let [
                Some(Term::NamedNode(e)),
                Some(Term::Literal(l)),
                Some(Term::NamedNode(t)),
            ] = row.as_slice()
            {
                out.push(json!({ "iri": e.as_str(), "label": l.value(), "type": t.as_str() }));
            }
        }
    }
    Ok(out)
}
