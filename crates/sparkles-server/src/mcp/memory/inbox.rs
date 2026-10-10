//! The review inbox of C18 §8.9 and the review of one branch of §7.10: what waits for a
//! person, with the signals the server checks itself, and the actions a reviewer takes.
//!
//! Everything is read through reifiers (C17 §3.2) over the caller's view. A session
//! fact is unreviewed when it is asserted on `main` only in graphs that `agentGraphs`
//! matches; a fact of a branch is proposed when the branch asserts it and `main` does
//! not, and a retraction is proposed when a reifier on the branch is invalidated while
//! `main` still asserts its triple. Facts that came in without a reifier are left to the
//! merge page, which shows every changed quad.

use super::ingest::{Renditions, SpanCheck, check_span, parse_span, read_rendition};
use super::link::{Mention, default_labels};
use super::{PROV, RDF_REIFIES, RDF_TYPE, Reader, SPK, iri};
use crate::auth::on_branch;
use crate::mcp::errors::{ErrorContext, ToolError};
use crate::mcp::render::{LABEL_PREDICATES, Prefixes, Terms};
use crate::mcp::tools::{Tools, dataset_prefixes};
use crate::state::Dataset;
use oxrdf::{NamedNode, Term, Triple};
use serde_json::{Map, Value, json};
use sparkles::error::Error;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The most facts the inbox lists.
pub(crate) const MAX_INBOX_FACTS: usize = 500;
/// The most review branches the inbox summarizes.
const MAX_BRANCHES: usize = 30;
/// The most entities whose links one listing checks.
const MAX_LINK_CHECKS: usize = 150;
/// The most source text a branch review carries, in bytes.
const MAX_REVIEW_TEXT: usize = 2 << 20;

/// Branch name prefixes of review work.
pub(crate) fn review_kind(name: &str) -> Option<&'static str> {
    if name.starts_with("ingest.") || name.starts_with("ingest-") {
        Some("ingest")
    } else if name.starts_with("review.") {
        Some("review")
    } else if name.starts_with("proposals.") || name.starts_with("proposals-") {
        if name.ends_with(".inbox") {
            Some("inbox")
        } else if name.contains("consolidat") {
            Some("consolidation")
        } else {
            Some("proposal")
        }
    } else {
        None
    }
}

/// The guard's dry run: whether it found violations, and the (focus, path) pairs it
/// reported.
type GuardVerdict = (bool, HashSet<(String, Option<String>)>);

/// A session graph of the inbox: its facts, first and last times, and principals.
type SessionGroup = (
    NamedNode,
    Vec<Value>,
    Option<String>,
    Option<String>,
    BTreeSet<String>,
);

/// One fact read through a reifier.
#[derive(Clone)]
pub(crate) struct ReifiedFact {
    pub s: Term,
    pub p: NamedNode,
    pub o: Term,
    pub g: NamedNode,
    pub reifiers: Vec<NamedNode>,
    pub time: Option<String>,
    pub confidence: Option<String>,
    pub quote: Option<String>,
    pub span: Option<NamedNode>,
    pub by: Option<String>,
    pub agent: Option<String>,
}

impl ReifiedFact {
    fn key(&self) -> String {
        format!("{} {} {} {}", self.s, self.p, self.o, self.g)
    }

    fn triple_key(&self) -> String {
        format!("{} {} {}", self.s, self.p, self.o)
    }
}

/// Memory bookkeeping that is never a fact to review.
fn bookkeeping(p: &NamedNode) -> bool {
    let p = p.as_str();
    p.starts_with(PROV) || p.starts_with(SPK) || p == RDF_REIFIES
}

fn lit(t: &Option<Term>) -> Option<String> {
    match t {
        Some(Term::Literal(l)) => Some(l.value().to_string()),
        _ => None,
    }
}

/// The reified facts of `r`'s named graphs (`graphs`, or all when empty): live
/// reifiers when `live`, invalidated ones otherwise, grouped by fact.
pub(crate) fn reified_facts(
    r: &Reader,
    graphs: &[NamedNode],
    live: bool,
    limit: usize,
) -> Result<Vec<ReifiedFact>, Error> {
    let values = if graphs.is_empty() {
        String::new()
    } else {
        super::values_iris("g", graphs)
    };
    let state = if live {
        format!("FILTER NOT EXISTS {{ ?r <{PROV}wasInvalidatedBy> ?x }}")
    } else {
        format!("?r <{PROV}wasInvalidatedBy> ?x")
    };
    let q = format!(
        "SELECT ?g ?r ?t ?time ?conf ?quote ?span ?who ?agent WHERE {{ {values} GRAPH ?g {{
          ?r <{RDF_REIFIES}> ?t . {state}
          OPTIONAL {{ ?r <{PROV}generatedAtTime> ?time }}
          OPTIONAL {{ ?r <{SPK}confidence> ?conf }}
          OPTIONAL {{ ?r <{SPK}quote> ?quote }}
          OPTIONAL {{ ?r <{PROV}wasDerivedFrom> ?span FILTER(isIRI(?span) && CONTAINS(STR(?span), \"#char=\")) }}
          OPTIONAL {{ ?r <{PROV}wasGeneratedBy> ?act . ?act <{PROV}wasAssociatedWith> ?who FILTER(STRSTARTS(STR(?who), \"urn:x-sparkles:principal:\")) }}
          OPTIONAL {{ ?r <{PROV}wasGeneratedBy> ?act2 . ?act2 <{PROV}wasAssociatedWith> ?sa . ?sa a <{PROV}SoftwareAgent> ; <http://www.w3.org/2000/01/rdf-schema#label> ?agent }}
        }} }} ORDER BY ?time LIMIT {}",
        limit * 4
    );
    let mut out: Vec<ReifiedFact> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for row in r.rows(&q, Vec::new())? {
        let [
            Some(Term::NamedNode(g)),
            Some(Term::NamedNode(rf)),
            Some(Term::Triple(t)),
            time,
            conf,
            quote,
            span,
            who,
            agent,
        ] = row.as_slice()
        else {
            continue;
        };
        let Triple {
            subject,
            predicate,
            object,
        } = (**t).clone();
        if bookkeeping(&predicate) || matches!(object, Term::Triple(_)) {
            continue;
        }
        let f = ReifiedFact {
            s: subject.into(),
            p: predicate,
            o: object,
            g: g.clone(),
            reifiers: vec![rf.clone()],
            time: lit(time),
            confidence: lit(conf),
            quote: lit(quote),
            span: match span {
                Some(Term::NamedNode(n)) => Some(n.clone()),
                _ => None,
            },
            by: match who {
                Some(Term::NamedNode(n)) => Some(crate::mcp::render::label_text(
                    n.as_str().trim_start_matches("urn:x-sparkles:principal:"),
                )),
                _ => None,
            },
            agent: lit(agent),
        };
        match at.get(&f.key()) {
            Some(&i) => {
                let e = &mut out[i];
                if !e.reifiers.contains(rf) {
                    e.reifiers.push(rf.clone());
                }
                if e.span.is_none() && f.span.is_some() {
                    e.span = f.span;
                    e.quote = f.quote;
                }
                e.time = e.time.clone().max(f.time);
            }
            None => {
                if out.len() >= limit {
                    continue;
                }
                at.insert(f.key(), out.len());
                out.push(f);
            }
        }
    }
    Ok(out)
}

/// The facts of agent graphs that no reifier describes, such as the facts of an import
/// of harness memory (§8.10): every triple of `graphs` except memory bookkeeping, the
/// description of the graph itself, and the triples about reifiers and activities.
pub(crate) fn plain_facts(
    r: &Reader,
    graphs: &[NamedNode],
    limit: usize,
) -> Result<Vec<ReifiedFact>, Error> {
    if graphs.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let q = format!(
        "SELECT ?g ?s ?p ?o WHERE {{ {} GRAPH ?g {{ ?s ?p ?o
          FILTER(?s != ?g && !isTRIPLE(?o)
            && !STRSTARTS(STR(?p), \"{PROV}\") && !STRSTARTS(STR(?p), \"{SPK}\")
            && ?p != <{RDF_REIFIES}> && ?p != <{DCT}title> && ?p != <{DCT}format> && ?p != <{DCT}modified>)
          FILTER NOT EXISTS {{ ?s <{RDF_REIFIES}> ?any }}
          FILTER NOT EXISTS {{ ?s a <{PROV}Activity> }}
          FILTER NOT EXISTS {{ ?s a <{PROV}SoftwareAgent> }}
          FILTER NOT EXISTS {{ ?rf <{RDF_REIFIES}> <<( ?s ?p ?o )>> }}
        }} }} LIMIT {limit}",
        super::values_iris("g", graphs)
    );
    let mut out = Vec::new();
    for row in r.rows(&q, Vec::new())? {
        let [
            Some(Term::NamedNode(g)),
            Some(s),
            Some(Term::NamedNode(p)),
            Some(o),
        ] = row.as_slice()
        else {
            continue;
        };
        if matches!(s, Term::Literal(_)) {
            continue;
        }
        out.push(ReifiedFact {
            s: s.clone(),
            p: p.clone(),
            o: o.clone(),
            g: g.clone(),
            reifiers: Vec::new(),
            time: None,
            confidence: None,
            quote: None,
            span: None,
            by: None,
            agent: None,
        });
    }
    Ok(out)
}

const DCT: &str = "http://purl.org/dc/terms/";

/// The graphs of `r` that assert each triple, by triple key (`s p o`), for the triples
/// of `facts`.
fn asserting_graphs(
    r: &Reader,
    facts: &[ReifiedFact],
) -> Result<HashMap<String, BTreeSet<String>>, Error> {
    let mut out: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut rows: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for f in facts {
        if let (Some(s), Some(o)) = (super::values_term(&f.s), super::values_term(&f.o))
            && seen.insert(f.triple_key())
        {
            rows.push(format!("({s} {} {o})", f.p));
        }
    }
    for chunk in rows.chunks(200) {
        let q = format!(
            "SELECT DISTINCT ?s ?p ?o ?g WHERE {{ VALUES (?s ?p ?o) {{ {} }} {} }}",
            chunk.join(" "),
            r.quads("?s ?p ?o", &[])
        );
        for row in r.rows(&q, Vec::new())? {
            if let [Some(s), Some(p), Some(o), Some(Term::NamedNode(g))] = row.as_slice() {
                out.entry(format!("{s} {p} {o}"))
                    .or_default()
                    .insert(g.as_str().to_string());
            }
        }
    }
    Ok(out)
}

/// The reifiers among `iris` that `r` holds, each with whether it is live.
fn reifiers_on<'a>(
    r: &Reader,
    iris: impl Iterator<Item = &'a NamedNode>,
) -> Result<HashMap<String, bool>, Error> {
    let list: Vec<NamedNode> = iris.cloned().collect::<BTreeSet<_>>().into_iter().collect();
    let mut out = HashMap::new();
    for chunk in list.chunks(200) {
        let q = format!(
            "SELECT ?r (COUNT(?inv) AS ?n) WHERE {{ {} GRAPH ?g {{ ?r <{RDF_REIFIES}> ?t OPTIONAL {{ ?r <{PROV}wasInvalidatedBy> ?inv }} }} }} GROUP BY ?r",
            super::values_iris("r", chunk)
        );
        for row in r.rows(&q, Vec::new())? {
            if let [Some(Term::NamedNode(n)), Some(Term::Literal(c))] = row.as_slice() {
                out.insert(n.as_str().to_string(), c.value() == "0");
            }
        }
    }
    Ok(out)
}

/// The signals of one fact (§8.9): `pass`, `fail`, `none` or `unchecked`.
#[derive(Default, Clone)]
struct Signals {
    span: &'static str,
    link: &'static str,
    guard: &'static str,
    corroboration: &'static str,
    candidates: Vec<String>,
    notes: Vec<String>,
}

impl Signals {
    fn passes(&self) -> bool {
        self.span == "pass" && self.link == "pass" && self.guard == "pass"
    }
}

/// What the checks of one listing share: renditions, link verdicts and labels.
struct Checker<'a> {
    t: &'a Tools<'a>,
    ds: &'a Dataset,
    r: &'a Reader,
    ctx: &'a ErrorContext<'a>,
    rends: Renditions,
    setup: Option<super::link::LinkSetup>,
    /// entity → other entities a new mention of its label would link to
    links: HashMap<String, Option<Vec<String>>>,
    labels: HashMap<String, Option<String>>,
    checks: usize,
}

impl<'a> Checker<'a> {
    fn new(t: &'a Tools<'a>, ds: &'a Dataset, r: &'a Reader, ctx: &'a ErrorContext<'a>) -> Self {
        Checker {
            t,
            ds,
            r,
            ctx,
            rends: Renditions::default(),
            setup: None,
            links: HashMap::new(),
            labels: HashMap::new(),
            checks: 0,
        }
    }

    fn eng(&self) -> impl Fn(Error) -> ToolError + '_ {
        |e| self.ctx.engine(e)
    }

    /// The label of an IRI in the view, by the label predicates in order.
    fn label(&mut self, n: &NamedNode) -> Result<Option<String>, ToolError> {
        if let Some(l) = self.labels.get(n.as_str()) {
            return Ok(l.clone());
        }
        let preds: Vec<NamedNode> = LABEL_PREDICATES.iter().map(|p| iri(p)).collect();
        let q = format!(
            "SELECT ?lp ?l WHERE {{ {} {} FILTER(isLiteral(?l)) }} LIMIT 20",
            super::values_iris("lp", &preds),
            self.r.quads("?e ?lp ?l", &[])
        );
        let rows = self
            .r
            .rows(&q, vec![("e".into(), n.clone().into())])
            .map_err(self.eng())?;
        let mut best: Option<(usize, String)> = None;
        for row in rows {
            if let [Some(Term::NamedNode(lp)), Some(Term::Literal(l))] = row.as_slice() {
                let rank = LABEL_PREDICATES
                    .iter()
                    .position(|p| *p == lp.as_str())
                    .unwrap_or(99);
                if best.as_ref().is_none_or(|(r, _)| rank < *r) {
                    best = Some((rank, l.value().to_string()));
                }
            }
        }
        let l = best.map(|(_, l)| crate::mcp::render::label_text(&l));
        self.labels.insert(n.as_str().to_string(), l.clone());
        Ok(l)
    }

    /// Other entities of the view that a mention of `n`'s label and types links to with
    /// an exact or normalized label of a matching type; `None` when not checked.
    fn duplicates(&mut self, n: &NamedNode) -> Result<Option<Vec<String>>, ToolError> {
        if let Some(v) = self.links.get(n.as_str()) {
            return Ok(v.clone());
        }
        if self.checks >= MAX_LINK_CHECKS {
            return Ok(None);
        }
        self.checks += 1;
        let Some(label) = self.label(n)? else {
            // an entity without a label was named by its IRI: linked exact
            self.links.insert(n.as_str().to_string(), Some(Vec::new()));
            return Ok(Some(Vec::new()));
        };
        let q = format!(
            "SELECT DISTINCT ?c WHERE {{ {} }} LIMIT 10",
            self.r
                .quads(&format!("?e <{RDF_TYPE}> ?c FILTER(isIRI(?c))"), &[])
        );
        let types: Vec<NamedNode> = self
            .r
            .rows(&q, vec![("e".into(), n.clone().into())])
            .map_err(self.eng())?
            .into_iter()
            .filter_map(|row| match row.into_iter().next() {
                Some(Some(Term::NamedNode(c))) => Some(c),
                _ => None,
            })
            .collect();
        if self.setup.is_none() {
            self.setup =
                Some(
                    self.t
                        .link_setup(self.ds, self.r, default_labels(), Vec::new(), self.ctx)?,
                );
        }
        let m = Mention {
            text: label,
            types,
            context: None,
        };
        let (linked, _, _) =
            self.t
                .link(self.r, self.setup.as_ref().expect("set"), &m, 6, self.ctx)?;
        let others: Vec<String> = linked
            .candidates
            .iter()
            .filter(|c| c.iri != *n && c.type_match && (c.exact || c.normalized))
            .map(|c| c.iri.as_str().to_string())
            .collect();
        self.links
            .insert(n.as_str().to_string(), Some(others.clone()));
        Ok(Some(others))
    }

    /// The span and link signals of a fact; guard and corroboration are set by the
    /// caller.
    fn signals(&mut self, f: &ReifiedFact) -> Result<Signals, ToolError> {
        let mut s = Signals {
            span: "none",
            link: "pass",
            guard: "unchecked",
            corroboration: "none",
            ..Signals::default()
        };
        if let Some(span) = &f.span
            && let Some((rend, a, b)) = parse_span(span.as_str())
        {
            let rend = iri(rend);
            let out = check_span(
                &mut self.rends,
                self.r,
                &rend,
                a,
                b,
                f.quote.as_deref(),
                &f.g,
                usize::MAX,
            )
            .map_err(self.eng())?;
            s.span = match out {
                SpanCheck::Ok { .. } => "pass",
                SpanCheck::Failed { message, .. } => {
                    s.notes.push(message);
                    "fail"
                }
            };
        }
        let mut nodes: Vec<&NamedNode> = Vec::new();
        if let Term::NamedNode(n) = &f.s {
            nodes.push(n);
        }
        if let Term::NamedNode(n) = &f.o
            && f.p.as_str() != RDF_TYPE
        {
            nodes.push(n);
        }
        for n in nodes {
            match self.duplicates(n)? {
                None => {
                    if s.link == "pass" {
                        s.link = "unchecked";
                    }
                }
                Some(d) if !d.is_empty() => {
                    s.link = "fail";
                    for c in d {
                        if !s.candidates.contains(&c) {
                            s.candidates.push(c);
                        }
                    }
                }
                Some(_) => {}
            }
        }
        Ok(s)
    }
}

/// The JSON of one fact of a listing.
fn fact_json(
    f: &ReifiedFact,
    sig: Option<&Signals>,
    status: &str,
    labels: &HashMap<String, Option<String>>,
    terms: &mut Terms,
) -> Value {
    let mut j = json!({
        "s": f.s.to_string(),
        "p": f.p.to_string(),
        "o": f.o.to_string(),
        "graph": f.g.to_string(),
        "shown": {
            "s": terms.term(&f.s),
            "p": terms.iri(f.p.as_str()),
            "o": terms.term(&f.o),
            "graph": terms.iri(f.g.as_str()),
        },
        "status": status,
        "reifiers": f.reifiers.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
    });
    for (k, t) in [("sLabel", &f.s), ("oLabel", &f.o)] {
        if let Term::NamedNode(n) = t
            && let Some(Some(l)) = labels.get(n.as_str())
        {
            j[k] = l.clone().into();
        }
    }
    for (k, v) in [
        ("time", &f.time),
        ("confidence", &f.confidence),
        ("quote", &f.quote),
        ("by", &f.by),
        ("agent", &f.agent),
    ] {
        if let Some(v) = v {
            j[k] = v.clone().into();
        }
    }
    if let Some(sp) = &f.span
        && let Some((rend, a, b)) = parse_span(sp.as_str())
    {
        j["span"] = json!({"rendition": rend, "start": a, "end": b});
    }
    if let Some(s) = sig {
        j["signals"] = json!({
            "span": s.span, "link": s.link, "guard": s.guard, "corroboration": s.corroboration,
        });
        j["passes"] = s.passes().into();
        if !s.candidates.is_empty() {
            j["candidates"] = s
                .candidates
                .iter()
                .map(|c| {
                    let mut x = json!({"iri": c, "shown": terms.iri(c)});
                    if let Some(Some(l)) = labels.get(c.as_str()) {
                        x["label"] = l.clone().into();
                    }
                    x
                })
                .collect::<Vec<_>>()
                .into();
        }
        if !s.notes.is_empty() {
            j["notes"] = s.notes.clone().into();
        }
    }
    j
}

/// The guard's verdict on writing `facts` into `target` on `ds` as the caller: the
/// subjects (and predicates) of the blocking results, or `None` when the dry run could
/// not tell (no target, a graph the caller may not write, a timeout).
fn guard_failures(
    t: &Tools,
    ds: &Arc<Dataset>,
    target: &NamedNode,
    facts: &[&ReifiedFact],
    deadline: Instant,
) -> Option<GuardVerdict> {
    if facts.is_empty() {
        return Some((false, HashSet::new()));
    }
    let mut opts = t
        .query_options(
            &ds.name,
            crate::auth::Endpoint::Update,
            false,
            deadline,
            &BTreeMap::new(),
        )
        .ok()?;
    if opts
        .graphs
        .as_ref()
        .is_some_and(|v| !v.writable_iri(target.as_str()))
    {
        return None;
    }
    let mut body = String::new();
    for f in facts {
        body.push_str(&format!("{} {} {} .\n", f.s, f.p, f.o));
    }
    let text = format!("INSERT DATA {{ GRAPH {target} {{\n{body}}} }}");
    opts.write = sparkles::guard::WriteOptions {
        bypass_validation: false,
        deadline: Some(deadline),
        cancel: Some(t.call.cancel.clone()),
        report_limit: Some(500),
        message: None,
        author: None,
        precondition: None,
        no_wait: false,
        graphs: opts.graphs.clone(),
        dry_run: Some(sparkles::preview::DryRun {
            changes: 0,
            all_changes: false,
            max_changes: 0,
        }),
    };
    match sparkles::sparql::update::update_as(
        &ds.store,
        &text,
        &opts,
        sparkles::commit::CommitKind::Update,
    ) {
        Err(Error::DryRun(pv)) => {
            let Some(v) = &pv.validation else {
                return Some((false, HashSet::new()));
            };
            let mut out = HashSet::new();
            for r in &v.results {
                let sev = r["severity"]["value"].as_str().unwrap_or("");
                if !sev.ends_with("Violation") && !r.get("shape").is_some() {
                    continue;
                }
                let focus = r["focusNode"]["value"]
                    .as_str()
                    .or_else(|| r["node"].as_str())
                    .map(str::to_string);
                let path = r["resultPath"]["value"].as_str().map(str::to_string);
                if let Some(f) = focus {
                    out.insert((f, path));
                }
            }
            Some((v.blocks(), out))
        }
        _ => None,
    }
}

fn guard_signal(f: &ReifiedFact, verdict: &Option<GuardVerdict>) -> &'static str {
    match verdict {
        None => "unchecked",
        Some((_, fails)) => {
            let s = match &f.s {
                Term::NamedNode(n) => n.as_str().to_string(),
                t => t.to_string(),
            };
            let hit = fails.iter().any(|(focus, path)| {
                *focus == s && path.as_deref().is_none_or(|p| p == f.p.as_str())
            });
            if hit { "fail" } else { "pass" }
        }
    }
}

impl Tools<'_> {
    /// The review inbox of `ds` (`main`) for the caller (§8.9).
    pub(crate) fn inbox(
        &self,
        ds: &Arc<Dataset>,
        limit: usize,
        timeout: Duration,
    ) -> Result<Value, ToolError> {
        let limit = limit.clamp(1, MAX_INBOX_FACTS);
        let prefix_map = dataset_prefixes(ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let r = self.reader(ds, None, None, Some(false), deadline, &ctx)?;
        let eng = |e: Error| ctx.engine(e);
        let memory = crate::assist::memory_settings(&self.server.state, ds);
        let mut terms = Terms::new(&prefixes, 500);
        let target = memory.consolidated_graph.as_deref().map(iri);
        // the agent graphs of the view
        let mut agent_graphs: Vec<NamedNode> = Vec::new();
        if !memory.agent_graphs.is_empty() {
            let q = "SELECT DISTINCT ?g WHERE { GRAPH ?g { ?s ?p ?o } } LIMIT 5000";
            for row in r.rows(q, Vec::new()).map_err(eng)? {
                if let Some(Some(Term::NamedNode(g))) = row.first()
                    && memory.is_agent_graph(g.as_str())
                {
                    agent_graphs.push(g.clone());
                }
            }
        }
        let mut facts: Vec<ReifiedFact> = Vec::new();
        let mut truncated = false;
        for chunk in agent_graphs.chunks(100) {
            let room = limit + 1 - facts.len().min(limit + 1);
            if room == 0 {
                break;
            }
            facts.extend(reified_facts(&r, chunk, true, room).map_err(eng)?);
        }
        // facts without a reifier, such as imported harness memory, after the others
        for chunk in agent_graphs.chunks(100) {
            let room = limit + 1 - facts.len().min(limit + 1);
            if room == 0 {
                break;
            }
            facts.extend(plain_facts(&r, chunk, room).map_err(eng)?);
        }
        if facts.len() > limit {
            truncated = true;
            facts.truncate(limit);
        }
        // asserted on main only in agent graphs, and in its own graph
        let graphs_of = asserting_graphs(&r, &facts).map_err(eng)?;
        facts.retain(|f| {
            let gs = graphs_of.get(&f.triple_key());
            gs.is_some_and(|gs| {
                gs.contains(f.g.as_str()) && gs.iter().all(|g| memory.is_agent_graph(g))
            })
        });
        // the signals
        let mut ck = Checker::new(self, ds, &r, &ctx);
        let mut sigs: Vec<Signals> = Vec::with_capacity(facts.len());
        for f in &facts {
            let mut s = ck.signals(f)?;
            let others = graphs_of
                .get(&f.triple_key())
                .map_or(0, |gs| gs.iter().filter(|g| *g != f.g.as_str()).count());
            s.corroboration = if others > 0 { "pass" } else { "none" };
            sigs.push(s);
        }
        let verdict = match &target {
            Some(t) => guard_failures(self, ds, t, &facts.iter().collect::<Vec<_>>(), deadline),
            None => None,
        };
        for (f, s) in facts.iter().zip(sigs.iter_mut()) {
            s.guard = guard_signal(f, &verdict);
        }
        for f in &facts {
            for t in [&f.s, &f.o] {
                if let Term::NamedNode(n) = t {
                    ck.label(n)?;
                }
            }
        }
        for s in &sigs {
            for c in &s.candidates {
                ck.label(&iri(c))?;
            }
        }
        let labels = std::mem::take(&mut ck.labels);
        // grouped by session graph, oldest first
        let mut groups: Vec<SessionGroup> = Vec::new();
        for (f, s) in facts.iter().zip(&sigs) {
            let i = match groups.iter().position(|(g, ..)| *g == f.g) {
                Some(i) => i,
                None => {
                    groups.push((f.g.clone(), Vec::new(), None, None, BTreeSet::new()));
                    groups.len() - 1
                }
            };
            let e = &mut groups[i];
            e.1.push(fact_json(f, Some(s), "unreviewed", &labels, &mut terms));
            if let Some(t) = &f.time {
                if e.2.as_ref().is_none_or(|x| t < x) {
                    e.2 = Some(t.clone());
                }
                if e.3.as_ref().is_none_or(|x| t > x) {
                    e.3 = Some(t.clone());
                }
            }
            for who in [&f.by, &f.agent].into_iter().flatten() {
                e.4.insert(who.clone());
            }
        }
        let sessions: Vec<Value> = groups
            .into_iter()
            .map(|(g, facts, first, last, by)| {
                json!({
                    "graph": g.as_str(),
                    "shown": terms.iri(g.as_str()),
                    "by": by.into_iter().collect::<Vec<_>>(),
                    "first": first, "last": last,
                    "facts": facts,
                })
            })
            .collect();
        // the review branches
        let branches = self.review_branches(ds, deadline, &ctx)?;
        let open: usize = sessions
            .iter()
            .map(|s| s["facts"].as_array().map_or(0, Vec::len))
            .sum::<usize>()
            + branches.len();
        let mut out = json!({
            "dataset": ds.name,
            "commit": r.snap.commit,
            "agentGraphs": memory.agent_graphs,
            "sessions": sessions,
            "branches": branches,
            "open": open,
            "truncated": truncated,
            "prefixes": terms.used(),
        });
        if let Some(t) = &memory.consolidated_graph {
            out["target"] = t.clone().into();
        }
        Ok(out)
    }

    /// The open review branches the caller may read, newest first, each with its counts.
    fn review_branches(
        &self,
        ds: &Arc<Dataset>,
        deadline: Instant,
        ctx: &ErrorContext,
    ) -> Result<Vec<Value>, ToolError> {
        let p = self.call.principal.clone().on_branch(None);
        let Ok(infos) = ds.store.branches() else {
            return Ok(Vec::new());
        };
        let mut list: Vec<_> = infos
            .into_iter()
            // a branch without commits of its own (merged, or not written yet) has
            // nothing to review
            .filter(|b| review_kind(&b.name).is_some() && b.ahead > 0)
            .filter(|b| p.level(&on_branch(&ds.name, &b.name)).is_some())
            .collect();
        list.sort_by_key(|b| {
            std::cmp::Reverse(b.head.as_ref().map_or(b.created_ms, |h| h.timestamp_ms))
        });
        let mut out = Vec::new();
        for b in list.into_iter().take(MAX_BRANCHES) {
            let mut j = json!({
                "name": b.name,
                "kind": review_kind(&b.name),
                "ahead": b.ahead,
                "behind": b.behind,
                "created": sparkles::commit::rfc3339_ms(b.created_ms),
            });
            if let Some(h) = &b.head {
                j["modified"] = sparkles::commit::rfc3339_ms(h.timestamp_ms).into();
            }
            if let Some(n) = &b.note {
                j["note"] = crate::mcp::render::label_text(n).into();
            }
            if let Some(s) = &b.scratch {
                j["creator"] = s.creator.clone().into();
            }
            if Instant::now() < deadline
                && let Ok((proposed, retracts)) = self.branch_counts(ds, &b.name, deadline, ctx)
            {
                j["facts"] = proposed.into();
                j["retracts"] = retracts.into();
            }
            out.push(j);
        }
        Ok(out)
    }

    /// The readers of `main` and of branch `b` as the caller.
    fn branch_readers(
        &self,
        ds: &Arc<Dataset>,
        b: &str,
        deadline: Instant,
        ctx: &ErrorContext,
    ) -> Result<(Reader, Reader, Arc<Dataset>), ToolError> {
        let p = self.call.principal.clone().on_branch(Some(b));
        if p.level(&ds.name).is_none() {
            return Err(ToolError::new(
                "no-such-branch",
                404,
                format!("no such branch: {b}"),
            ));
        }
        let bds =
            self.server.state.branch_dataset(ds, b).map_err(|_| {
                ToolError::new("no-such-branch", 404, format!("no such branch: {b}"))
            })?;
        let main = self.reader(ds, None, None, Some(false), deadline, ctx)?;
        let call = crate::mcp::Call {
            arrived: self.call.arrived,
            cancel: self.call.cancel.clone(),
            request_id: self.call.request_id.clone(),
            principal: p,
            headers: None,
            held: None,
        };
        let t = Tools {
            server: self.server,
            call: &call,
        };
        let br = t.reader(&bds, None, None, Some(false), deadline, ctx)?;
        Ok((main, br, bds))
    }

    /// The facts a branch proposes and the facts of `main` it retracts.
    fn branch_changes(
        &self,
        main: &Reader,
        br: &Reader,
        limit: usize,
    ) -> Result<(Vec<ReifiedFact>, Vec<ReifiedFact>, usize), Error> {
        // a reifier main knows came from main, so main's own later changes (a rejection,
        // a supersession) never read as the branch's proposals
        let mut added = reified_facts(br, &[], true, limit)?;
        let known = reifiers_on(main, added.iter().flat_map(|f| &f.reifiers))?;
        added.retain(|f| f.reifiers.iter().all(|r| !known.contains_key(r.as_str())));
        let on_branch = asserting_graphs(br, &added)?;
        let on_main = asserting_graphs(main, &added)?;
        added.retain(|f| {
            let k = f.triple_key();
            on_branch.get(&k).is_some_and(|g| g.contains(f.g.as_str()))
                && !on_main.get(&k).is_some_and(|g| g.contains(f.g.as_str()))
        });
        let gone = reified_facts(br, &[], false, limit)?;
        let known = reifiers_on(main, gone.iter().flat_map(|f| &f.reifiers))?;
        let gone_branch = asserting_graphs(br, &gone)?;
        let gone_main = asserting_graphs(main, &gone)?;
        let mut retracts = Vec::new();
        let mut rejected = 0usize;
        for f in gone {
            let k = f.triple_key();
            let still = gone_branch
                .get(&k)
                .is_some_and(|g| g.contains(f.g.as_str()));
            if still {
                continue;
            }
            let on_m = gone_main.get(&k).is_some_and(|g| g.contains(f.g.as_str()));
            // live on main: the branch retracts it; unknown to main: made and rejected
            // on the branch
            let live_on_main = f
                .reifiers
                .iter()
                .any(|r| known.get(r.as_str()) == Some(&true));
            let new = f.reifiers.iter().all(|r| !known.contains_key(r.as_str()));
            if on_m && live_on_main {
                retracts.push(f);
            } else if new {
                rejected += 1;
            }
        }
        Ok((added, retracts, rejected))
    }

    fn branch_counts(
        &self,
        ds: &Arc<Dataset>,
        b: &str,
        deadline: Instant,
        ctx: &ErrorContext,
    ) -> Result<(usize, usize), ToolError> {
        let (main, br, _) = self.branch_readers(ds, b, deadline, ctx)?;
        let (a, r, _) = self
            .branch_changes(&main, &br, MAX_INBOX_FACTS)
            .map_err(|e| ctx.engine(e))?;
        Ok((a.len(), r.len()))
    }

    /// The review of one branch (§7.10): its proposed facts with their signals and
    /// passages, the facts of `main` it retracts, its new entities with possible
    /// duplicates, and the text of the sources the facts cite.
    pub(crate) fn review_branch(
        &self,
        ds: &Arc<Dataset>,
        b: &str,
        limit: usize,
        timeout: Duration,
    ) -> Result<Value, ToolError> {
        let limit = limit.clamp(1, 2000);
        let prefix_map = dataset_prefixes(ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let (main, br, bds) = self.branch_readers(ds, b, deadline, &ctx)?;
        let eng = |e: Error| ctx.engine(e);
        let (added, retracts, rejected) = self.branch_changes(&main, &br, limit).map_err(eng)?;
        let mut terms = Terms::new(&prefixes, 500);
        // signals over the branch, links and corroboration against main
        let on_main = asserting_graphs(&main, &added).map_err(eng)?;
        let mut ck = Checker::new(self, &bds, &br, &ctx);
        let mut sigs = Vec::new();
        for f in &added {
            let mut s = ck.signals(f)?;
            s.corroboration = if on_main.contains_key(&f.triple_key()) {
                "pass"
            } else {
                "none"
            };
            sigs.push(s);
        }
        // new entities: subjects typed on the branch that main does not know
        let mut entities: Vec<Value> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for f in &added {
            let (Term::NamedNode(e), true) = (&f.s, f.p.as_str() == RDF_TYPE) else {
                continue;
            };
            if !seen.insert(e.as_str().to_string()) {
                continue;
            }
            let known = main
                .ask(
                    &super::exists_term(&main),
                    vec![("t".into(), e.clone().into())],
                )
                .map_err(eng)?;
            if known {
                continue;
            }
            let label = ck.label(e)?;
            let dups = ck.duplicates(e)?.unwrap_or_default();
            let types: Vec<String> = added
                .iter()
                .filter(|x| x.s == f.s && x.p.as_str() == RDF_TYPE)
                .map(|x| terms.term(&x.o))
                .collect();
            let mut cands = Vec::new();
            for c in &dups {
                let l = ck.label(&iri(c))?;
                let mut x = json!({"iri": c, "shown": terms.iri(c)});
                if let Some(l) = l {
                    x["label"] = l.into();
                }
                cands.push(x);
            }
            entities.push(json!({
                "iri": e.as_str(),
                "shown": terms.iri(e.as_str()),
                "label": label,
                "types": types,
                "candidates": cands,
            }));
        }
        for f in added.iter().chain(&retracts) {
            for t in [&f.s, &f.o] {
                if let Term::NamedNode(n) = t {
                    ck.label(n)?;
                }
            }
        }
        // the sources the facts cite, and those registered on the branch
        let mut rend_iris: BTreeSet<String> = BTreeSet::new();
        for f in &added {
            if let Some(sp) = &f.span
                && let Some((r, _, _)) = parse_span(sp.as_str())
            {
                rend_iris.insert(r.to_string());
            }
        }
        for s in super::ingest::list_sources(&br, &[], 50).map_err(eng)? {
            let fresh = !main
                .ask(
                    &format!(
                        "ASK {{ {} }}",
                        main.quads(&format!("?s <{SPK}rendition> ?r"), &[])
                    ),
                    vec![
                        ("s".into(), s.source.clone().into()),
                        ("r".into(), s.rendition.clone().into()),
                    ],
                )
                .map_err(eng)?;
            if fresh {
                rend_iris.insert(s.rendition.as_str().to_string());
            }
        }
        let mut sources: Vec<Value> = Vec::new();
        let mut budget = MAX_REVIEW_TEXT;
        for ri in &rend_iris {
            let Some(rd) = read_rendition(&br, &iri(ri)).map_err(eng)? else {
                continue;
            };
            let src = rd.sources.first().map(|(_, s)| s.clone());
            let mut j = json!({
                "rendition": ri,
                "length": rd.length,
            });
            if let Some(s) = &src {
                j["source"] = s.as_str().into();
                let q = format!(
                    "SELECT ?title ?fmt WHERE {{ {} }} LIMIT 1",
                    br.quads(
                        &format!(
                            "OPTIONAL {{ ?s <{}> ?title }} OPTIONAL {{ ?s <{}> ?fmt }}",
                            super::ingest::DCT_TITLE,
                            super::ingest::DCT_FORMAT
                        ),
                        &[]
                    )
                );
                if let Some(row) = br
                    .rows(&q, vec![("s".into(), s.clone().into())])
                    .map_err(eng)?
                    .into_iter()
                    .next()
                {
                    if let Some(t) = lit(&row[0]) {
                        j["title"] = t.into();
                    }
                    if let Some(f) = lit(&row[1]) {
                        j["format"] = f.into();
                    }
                }
            }
            if !rd.chunks.is_empty() {
                let text: String = rd.chunks.iter().map(|(_, _, t)| t.as_str()).collect();
                if text.len() <= budget {
                    budget -= text.len();
                    j["text"] = text.into();
                } else {
                    j["textOmitted"] = true.into();
                }
            }
            sources.push(j);
        }
        let labels = std::mem::take(&mut ck.labels);
        let facts: Vec<Value> = added
            .iter()
            .zip(&sigs)
            .map(|(f, s)| fact_json(f, Some(s), "proposed", &labels, &mut terms))
            .collect();
        let retracts: Vec<Value> = retracts
            .iter()
            .map(|f| fact_json(f, None, "reviewed", &labels, &mut terms))
            .collect();
        let info = ds
            .store
            .branches()
            .ok()
            .and_then(|l| l.into_iter().find(|x| x.name == b));
        let mut out = json!({
            "dataset": ds.name,
            "branch": b,
            "kind": review_kind(b),
            "base": main.snap.commit,
            "head": br.snap.commit,
            "facts": facts,
            "retracts": retracts,
            "rejected": rejected,
            "entities": entities,
            "sources": sources,
            "prefixes": terms.used(),
        });
        if let Some(i) = info {
            out["ahead"] = i.ahead.into();
            out["behind"] = i.behind.into();
            if let Some(n) = &i.note {
                out["note"] = crate::mcp::render::label_text(n).into();
            }
            if let Some(s) = &i.scratch {
                out["creator"] = s.creator.clone().into();
            }
        }
        Ok(out)
    }

    /// "Use existing" (§7.10): on the branch the call works on, every triple and reifier
    /// that names `from` names `to` instead, and `from`'s own types and labels go, in
    /// one commit.
    pub(crate) fn relink(
        &self,
        ds: &Arc<Dataset>,
        from: &NamedNode,
        to: &NamedNode,
        message: Option<Arc<str>>,
        timeout: Duration,
    ) -> Result<Value, ToolError> {
        let prefix_map = dataset_prefixes(ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let r = self.reader(ds, None, None, Some(false), deadline, &ctx)?;
        let eng = |e: Error| ctx.engine(e);
        if !r
            .ask(
                &super::exists_term(&r),
                vec![("t".into(), to.clone().into())],
            )
            .map_err(eng)?
        {
            return Err(ToolError::new(
                "unknown-entity",
                404,
                format!("<{}> occurs nowhere you can read", to.as_str()),
            ));
        }
        let own = [
            RDF_TYPE,
            "http://www.w3.org/2000/01/rdf-schema#label",
            super::SKOS_ALT,
        ];
        let q = format!(
            "SELECT ?g ?s ?p ?o WHERE {{ {{ {} }} UNION {{ {} }} }} LIMIT 10000",
            r.quads("?s ?p ?o FILTER(?s = ?x)", &[]),
            r.quads("?s ?p ?o FILTER(?o = ?x)", &[]),
        );
        let mut del: BTreeMap<String, String> = BTreeMap::new();
        let mut ins: BTreeMap<String, String> = BTreeMap::new();
        let swap = |t: &Term| -> Term {
            match t {
                Term::NamedNode(n) if n == from => to.clone().into(),
                t => t.clone(),
            }
        };
        let nt = super::assert::nt;
        for row in r
            .rows(&q, vec![("x".into(), from.clone().into())])
            .map_err(eng)?
        {
            let [
                Some(Term::NamedNode(g)),
                Some(s),
                Some(Term::NamedNode(p)),
                Some(o),
            ] = row.as_slice()
            else {
                continue;
            };
            let line = format!("{} {p} {} .\n", nt(s), nt(o));
            del.entry(g.to_string()).or_default().push_str(&line);
            let is_own = matches!(s, Term::NamedNode(n) if n == from) && own.contains(&p.as_str());
            if !is_own {
                ins.entry(g.to_string()).or_default().push_str(&format!(
                    "{} {p} {} .\n",
                    nt(&swap(s)),
                    nt(&swap(o))
                ));
            }
        }
        // reifiers of triples that name it
        let q = format!(
            "SELECT ?g ?r ?t WHERE {{ {} }} LIMIT 10000",
            r.quads(&format!("?r <{RDF_REIFIES}> ?t"), &[])
        );
        for row in r.rows(&q, Vec::new()).map_err(eng)? {
            let [Some(Term::NamedNode(g)), Some(rf), Some(Term::Triple(t))] = row.as_slice() else {
                continue;
            };
            let s: Term = t.subject.clone().into();
            let names_it =
                s == Term::NamedNode(from.clone()) || t.object == Term::NamedNode(from.clone());
            if !names_it {
                continue;
            }
            let old = format!(
                "{} <{RDF_REIFIES}> {} .\n",
                nt(rf),
                nt(&Term::Triple(t.clone()))
            );
            del.entry(g.to_string()).or_default().push_str(&old);
            let own_fact =
                s == Term::NamedNode(from.clone()) && own.contains(&t.predicate.as_str());
            if !own_fact && let Term::NamedNode(ns) = swap(&s) {
                let nt3 = Triple::new(ns, t.predicate.clone(), swap(&t.object));
                ins.entry(g.to_string()).or_default().push_str(&format!(
                    "{} <{RDF_REIFIES}> {} .\n",
                    nt(rf),
                    nt(&Term::Triple(Box::new(nt3)))
                ));
            }
        }
        if del.is_empty() {
            return Err(ToolError::new(
                "unknown-entity",
                404,
                format!("<{}> occurs nowhere you can write", from.as_str()),
            ));
        }
        let block = |m: &BTreeMap<String, String>| -> String {
            m.iter()
                .map(|(g, body)| {
                    super::assert::graph_block(
                        &iri(g.trim_start_matches('<').trim_end_matches('>')),
                        body,
                    )
                })
                .collect()
        };
        let mut text = format!("DELETE DATA {{\n{}}}", block(&del));
        if !ins.is_empty() {
            text.push_str(&format!(" ;\nINSERT DATA {{\n{}}}", block(&ins)));
        }
        self.write_update(ds, &text, message, deadline, &ctx)
    }

    /// Run a SPARQL Update as the caller on the dataset the call works on, under its
    /// write view and the guard, and answer the commit.
    pub(crate) fn write_update(
        &self,
        ds: &Arc<Dataset>,
        text: &str,
        message: Option<Arc<str>>,
        deadline: Instant,
        ctx: &ErrorContext,
    ) -> Result<Value, ToolError> {
        let p = &self.call.principal;
        if !p.can_at(
            &ds.name,
            crate::auth::Endpoint::Update,
            crate::auth::Level::Write,
        ) {
            return Err(ToolError::new(
                "forbidden",
                403,
                format!("write access to dataset {} required", ds.name),
            ));
        }
        let mut opts = self
            .query_options(
                &ds.name,
                crate::auth::Endpoint::Update,
                false,
                deadline,
                &BTreeMap::new(),
            )
            .map_err(|e| ctx.engine(e))?;
        opts.forbid_remote_load = true;
        opts.forbid_file_load = true;
        opts.write = sparkles::guard::WriteOptions {
            bypass_validation: false,
            deadline: Some(deadline),
            cancel: Some(self.call.cancel.clone()),
            report_limit: None,
            message,
            author: crate::http::author(p),
            precondition: None,
            no_wait: false,
            graphs: opts.graphs.clone(),
            dry_run: None,
        };
        match sparkles::sparql::update::update_as(
            &ds.store,
            text,
            &opts,
            sparkles::commit::CommitKind::Update,
        ) {
            Ok(stats) => {
                let commit = stats.commit.as_ref().map(|c| c.commit.seq);
                Ok(json!({
                    "dataset": ds.name,
                    "committed": stats.commit.as_ref().is_some_and(|c| c.committed),
                    "commit": commit,
                    "inserted": stats.inserted,
                    "deleted": stats.deleted,
                }))
            }
            Err(Error::Rejected(_)) => Err(ToolError::new(
                "validation-failed",
                422,
                "the change does not conform to the dataset's validation guard; nothing was written",
            )),
            Err(e) => Err(ctx.engine(e)),
        }
    }
}

/// A fact named by a request: N-Triples terms of its subject, predicate, object and
/// graph, as the inbox and the review list them.
#[derive(serde::Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct FactRef {
    pub s: String,
    pub p: String,
    pub o: String,
    pub graph: String,
}

impl FactRef {
    /// The `retract` entry of `assert_facts`.
    pub fn retraction(&self) -> Value {
        json!({"s": self.s, "p": self.p, "o": self.o, "graph": self.graph})
    }
}

/// The live reifiers of facts on the reader's dataset, by fact.
pub(crate) fn live_reifiers_of(
    r: &Reader,
    facts: &[FactRef],
) -> Result<HashMap<usize, Vec<String>>, Error> {
    let mut out = HashMap::new();
    for (i, f) in facts.iter().enumerate() {
        let g = f.graph.trim_start_matches('<').trim_end_matches('>');
        let q = format!(
            "SELECT DISTINCT ?r WHERE {{ GRAPH <{g}> {{ ?r <{RDF_REIFIES}> <<( {} {} {} )>> FILTER NOT EXISTS {{ ?r <{PROV}wasInvalidatedBy> ?x }} FILTER(isIRI(?r)) }} }} LIMIT 20",
            f.s, f.p, f.o
        );
        let rows = r.rows(&q, Vec::new())?;
        out.insert(
            i,
            rows.into_iter()
                .filter_map(|row| match row.into_iter().next() {
                    Some(Some(Term::NamedNode(n))) => Some(n.as_str().to_string()),
                    _ => None,
                })
                .collect(),
        );
    }
    Ok(out)
}

/// The arguments of an `assert_facts` call that writes `facts` into `target` with
/// reifiers derived from `derived` (by fact).
pub(crate) fn promotion_args(
    dataset: &str,
    target: &str,
    facts: &[FactRef],
    derived: &HashMap<usize, Vec<String>>,
    message: &str,
) -> Map<String, Value> {
    let list: Vec<Value> = facts
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let mut j = json!({"s": f.s, "p": f.p, "o": f.o});
            if let Some(d) = derived.get(&i).filter(|d| !d.is_empty()) {
                j["derivedFrom"] = d
                    .iter()
                    .map(|r| format!("<{r}>"))
                    .collect::<Vec<_>>()
                    .into();
            }
            j
        })
        .collect();
    let mut a = Map::new();
    a.insert("dataset".into(), dataset.into());
    a.insert("graph".into(), format!("<{target}>").into());
    a.insert("facts".into(), list.into());
    a.insert("allowUnknownIris".into(), true.into());
    a.insert("message".into(), message.into());
    a
}
