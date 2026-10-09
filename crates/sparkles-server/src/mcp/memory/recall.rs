//! `recall` (C17 §5.5): the facts around a question or a set of entities, as compact
//! text or JSON, with a citation for each fact.
//!
//! The text format follows C11 §4.10: every term is one escaped line, and structural
//! lines begin with `#`, which no rendered term can, so a value in the data can neither
//! start a line of its own nor forge an entity header, a citation or a status line.

use super::link::{default_labels, literals_of, unavailable};
use super::text::plain_query;
use super::{
    DEFAULT_GRAPH, PROV, RDF_REIFIES, RDF_TYPE, RRF_K, Reader, SPK, graphs_arg, iri, iri_arg,
    is_vector, values_term,
};
use crate::mcp::Outcome;
use crate::mcp::errors::{ErrorContext, ToolError};
use crate::mcp::render::{self, Prefixes, Terms, escape_into};
use crate::mcp::tools::{Tools, bounded, dataset_prefixes, parse};
use oxrdf::{Literal, NamedNode, Term};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::error::Error;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

/// Outgoing facts kept per entity.
const OUT_PER_ENTITY: usize = 20;
/// Incoming facts kept per entity.
const IN_PER_ENTITY: usize = 10;
/// An entity with more incoming triples than this is not expanded.
const HUB: usize = 1000;
/// Outgoing rows read per entity to sample from.
const OUT_ROWS: usize = 2000;
/// Literals are cut to this many characters.
const LITERAL_CHARS: usize = 300;
/// Quotes in citations are cut to this many characters.
const QUOTE_CHARS: usize = 300;
/// Literals longer than this are cited by their graph alone (their reifiers are not
/// looked up).
const CITED_LITERAL_BYTES: usize = 4096;
/// Superseded reifiers read at most.
const SUPERSEDED_ROWS: usize = 10_000;

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Format {
    Text,
    Json,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RecallArgs {
    dataset: Option<String>,
    query: Option<String>,
    seeds: Option<Vec<String>>,
    types: Option<Vec<String>>,
    graphs: Option<Vec<String>>,
    hops: Option<u64>,
    seed_limit: Option<u64>,
    max_triples: Option<u64>,
    max_bytes: Option<u64>,
    include_superseded: Option<bool>,
    format: Option<Format>,
    reasoning: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
    timeout_seconds: Option<f64>,
}

/// One quad of the view.
#[derive(Clone)]
struct Fact {
    s: Term,
    p: NamedNode,
    o: Term,
    g: NamedNode,
}

impl Fact {
    fn key(&self) -> String {
        format!("{} {} {} {}", self.s, self.p, self.o, self.g)
    }
}

/// An entity of the result.
struct Entity {
    term: Term,
    seed: Option<usize>,
    hop: usize,
    types: Vec<NamedNode>,
    label: Option<String>,
    /// indexes into the facts, in order
    facts: Vec<usize>,
}

/// The provenance of a fact, from one of its reifiers.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
struct Provenance {
    reifier: Option<Term>,
    source: Option<Term>,
    at: Option<Term>,
    by: Option<Term>,
    confidence: Option<Term>,
    quote: Option<Term>,
}

/// A citation: a graph and the provenance its facts share.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Citation {
    graph: NamedNode,
    source: Option<Term>,
    at: Option<Term>,
    by: Option<Term>,
    confidence: Option<Term>,
    quote: Option<Term>,
}

/// A superseded or retracted fact.
struct Superseded {
    triple: Term,
    graph: NamedNode,
    reifier: Term,
    at: Option<Term>,
    invalidated: Option<Term>,
}

impl Tools<'_> {
    pub(crate) fn recall(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: RecallArgs = parse(args)?;
        let query = a.query.as_deref().map(str::trim).filter(|q| !q.is_empty());
        if a.query.as_ref().is_some_and(|q| q.chars().count() > 2000) {
            return Err(ToolError::bad_argument(
                "query must be at most 2000 characters",
            ));
        }
        let seed_args = a.seeds.clone().unwrap_or_default();
        if seed_args.len() > 20 {
            return Err(ToolError::bad_argument("at most 20 seeds"));
        }
        if query.is_none() && seed_args.is_empty() {
            return Err(ToolError::bad_argument("give query, seeds or both"));
        }
        let type_args = a.types.clone().unwrap_or_default();
        if type_args.len() > 5 {
            return Err(ToolError::bad_argument("at most 5 types"));
        }
        let hops = bounded("hops", a.hops, 1, 0, 2)? as usize;
        let seed_limit = bounded("seedLimit", a.seed_limit, 10, 1, 50)? as usize;
        let max_triples = bounded("maxTriples", a.max_triples, 150, 1, 1000)? as usize;
        let max_bytes = bounded(
            "maxBytes",
            a.max_bytes,
            32768.min(self.cfg().max_bytes as u64),
            1024,
            self.cfg().max_bytes as u64,
        )? as usize;
        let format = a.format.unwrap_or(Format::Text);
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let seeds_given: Vec<NamedNode> = seed_args
            .iter()
            .map(|s| iri_arg(s, &prefix_map, "seeds"))
            .collect::<Result<_, _>>()?;
        let types: Vec<NamedNode> = type_args
            .iter()
            .map(|t| iri_arg(t, &prefix_map, "types"))
            .collect::<Result<_, _>>()?;
        let graphs = graphs_arg(a.graphs.as_deref(), &prefix_map)?;
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let r = self.reader(&ds, a.at_commit, a.at.as_ref(), a.reasoning, deadline, &ctx)?;
        let eng = |e| ctx.engine(e);

        // the seeds: the given ones first, then the search's best subjects
        let mut seeds: Vec<(Term, Option<Fact>)> = seeds_given
            .iter()
            .map(|s| (Term::NamedNode(s.clone()), None))
            .collect();
        if let Some(q) = query {
            for found in self.search_seeds(
                &ds,
                &r,
                q,
                &types,
                &graphs,
                seed_limit,
                !seeds_given.is_empty(),
                &ctx,
            )? {
                if !seeds.iter().any(|(t, _)| *t == found.0) {
                    seeds.push(found);
                }
            }
        }

        // the facts around each seed, breadth first, seed by seed
        let mut facts: Vec<Fact> = Vec::new();
        let mut keys: HashSet<String> = HashSet::new();
        let mut entities: Vec<Entity> = Vec::new();
        let mut visited: HashMap<String, usize> = HashMap::new();
        let mut complete = true;
        let budget = max_triples + 1;
        'seeds: for (rank, (seed, fact)) in seeds.iter().enumerate() {
            let mut queue: VecDeque<(Term, usize)> = VecDeque::from([(seed.clone(), 0)]);
            let mut first = fact.clone();
            while let Some((e, hop)) = queue.pop_front() {
                if facts.len() >= budget {
                    complete = false;
                    break 'seeds;
                }
                let key = e.to_string();
                if let Some(&i) = visited.get(&key) {
                    if hop == 0 && entities[i].seed.is_none() {
                        entities[i].seed = Some(rank + 1);
                    }
                    continue;
                }
                let (types_of, out, inc, hub) = self.around(&r, &e, &graphs).map_err(eng)?;
                let mut entity = Entity {
                    term: e.clone(),
                    seed: (hop == 0).then_some(rank + 1),
                    hop,
                    types: types_of,
                    label: None,
                    facts: Vec::new(),
                };
                let mut add = |f: Fact, entity: &mut Entity, facts: &mut Vec<Fact>| {
                    if keys.insert(f.key()) {
                        entity.facts.push(facts.len());
                        facts.push(f);
                    }
                };
                if let Some(f) = first.take() {
                    add(f, &mut entity, &mut facts);
                }
                let mut next: Vec<Term> = Vec::new();
                for f in out {
                    if matches!(f.o, Term::NamedNode(_) | Term::BlankNode(_)) {
                        next.push(f.o.clone());
                    }
                    add(f, &mut entity, &mut facts);
                }
                for f in inc {
                    next.push(f.s.clone());
                    add(f, &mut entity, &mut facts);
                }
                visited.insert(key, entities.len());
                entities.push(entity);
                if !hub && hop < hops {
                    for n in next {
                        if !visited.contains_key(&n.to_string()) {
                            queue.push_back((n, hop + 1));
                        }
                    }
                }
            }
        }
        if facts.len() > max_triples {
            complete = false;
        }

        // labels of the entities
        let iris: Vec<NamedNode> = entities
            .iter()
            .filter_map(|e| match &e.term {
                Term::NamedNode(n) => Some(n.clone()),
                _ => None,
            })
            .collect();
        let labels = literals_of(&r, &iris, &default_labels(), &graphs).map_err(eng)?;
        for e in &mut entities {
            if let Term::NamedNode(n) = &e.term
                && let Some(v) = labels.get(n.as_str())
            {
                let refs: Vec<(usize, &Literal)> = v.iter().map(|(r, l)| (*r, l)).collect();
                e.label = render::choose_ranked(&refs, "en");
            }
        }
        // the provenance of each fact
        let provenance = self.provenance(&r, &facts).map_err(eng)?;
        let superseded = if a.include_superseded.unwrap_or(false) {
            let subjects: HashSet<String> = entities.iter().map(|e| e.term.to_string()).collect();
            self.superseded(&r, &subjects, &graphs, max_triples)
                .map_err(eng)?
        } else {
            Vec::new()
        };
        let conflicts = self.conflicts(&ds, &r, &entities, &facts, &ctx)?;
        let doc = Doc {
            dataset: &ds.name,
            commit: r.snap.commit,
            seeds: seeds.len(),
            entities: &entities,
            facts: &facts,
            provenance: &provenance,
            superseded: &superseded,
            conflicts: &conflicts,
            prefixes: &prefixes,
        };
        // the most facts that fit in maxTriples and maxBytes
        let total = facts.len().min(max_triples);
        let fits = |n: usize| {
            let truncated = n < facts.len() || !complete;
            let s = match format {
                Format::Text => doc.text(n, truncated),
                Format::Json => doc.json(n, truncated),
            };
            (s.len() <= max_bytes).then_some(s)
        };
        let best = match fits(total) {
            Some(s) => Some(s),
            None => {
                let (mut lo, mut hi) = (0usize, total);
                let mut best = fits(0);
                while lo + 1 < hi {
                    let mid = (lo + hi) / 2;
                    match fits(mid) {
                        Some(s) => {
                            best = Some(s);
                            lo = mid;
                        }
                        None => hi = mid,
                    }
                }
                best
            }
        };
        let text = best.ok_or_else(|| {
            ToolError::bad_argument(format!(
                "even without facts the result is larger than maxBytes ({max_bytes}); raise maxBytes or narrow the call"
            ))
        })?;
        Ok(Outcome::Text(text))
    }

    /// The seeds a search finds for `q`: the best rows of the full-text index and of
    /// the embedding index, fused by reciprocal rank as `spk:hybridSearch` fuses them,
    /// one per subject. A hit on a reifier counts as a hit on its triple's subject, and
    /// brings that fact along.
    #[allow(clippy::too_many_arguments)]
    fn search_seeds(
        &self,
        ds: &crate::state::Dataset,
        r: &Reader,
        q: &str,
        types: &[NamedNode],
        graphs: &[NamedNode],
        limit: usize,
        have_seeds: bool,
        ctx: &ErrorContext,
    ) -> Result<Vec<(Term, Option<Fact>)>, ToolError> {
        let eng = |e| ctx.engine(e);
        let text = if ds.dataset.indexes().text().enabled() {
            Some(
                match ds
                    .dataset
                    .indexes()
                    .text()
                    .status()
                    .map(|s| s.config.predicates)
                {
                    Some(sparkles::text::PredicateSet::Only(p)) => {
                        p.iter().map(|p| iri(p)).collect()
                    }
                    _ => Vec::new(),
                },
            )
        } else {
            None
        };
        let vector = self.first_embedding(ds);
        if text.is_none() && vector.is_none() {
            if have_seeds {
                return Ok(Vec::new());
            }
            return Err(ToolError::new(
                "no-search-index",
                400,
                format!(
                    "dataset {} has no full-text index and no embedding index, so recall cannot find entities for a query",
                    ds.name
                ),
            )
            .hint("pass seeds (entity IRIs, for example from link_entities), or ask an administrator to enable full-text search"));
        }
        let mut lists: Vec<HashMap<String, usize>> = Vec::new();
        let mut failure: Option<Error> = None;
        if let (Some(preds), Some(plain)) = (&text, plain_query(q)) {
            match self.text_hits(r, preds, &plain, graphs) {
                Ok(hits) => lists.push(rank_terms(hits.into_iter().map(|(s, sc, _)| (s, sc)))),
                Err(e) if unavailable(&e) => failure = Some(e),
                Err(e) => return Err(eng(e)),
            }
        }
        if let Some(pred) = &vector {
            match self.vector_hits(r, pred, q, graphs) {
                Ok(hits) => lists.push(rank_terms(hits.into_iter())),
                Err(e) if unavailable(&e) => {
                    failure.get_or_insert(e);
                }
                Err(e) => return Err(eng(e)),
            }
        }
        if lists.is_empty()
            && let Some(e) = failure
            && !have_seeds
        {
            return Err(eng(e));
        }
        let mut fused: HashMap<String, f64> = HashMap::new();
        for l in &lists {
            for (s, rank) in l {
                *fused.entry(s.clone()).or_default() += 1.0 / (RRF_K + *rank as f64);
            }
        }
        let mut hits: Vec<(String, f64)> = fused.into_iter().collect();
        hits.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        // reifiers stand for their triple's subject
        let hit_iris: Vec<NamedNode> = hits.iter().map(|(s, _)| iri(s)).collect();
        let reified = reified_by(r, &hit_iris, graphs).map_err(eng)?;
        let mut out: Vec<(Term, Option<Fact>)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (s, _) in &hits {
            let (subject, fact) = match reified.get(s) {
                Some(f) => (f.s.clone(), Some(f.clone())),
                None => (Term::NamedNode(iri(s)), None),
            };
            if seen.insert(subject.to_string()) {
                out.push((subject, fact));
            }
        }
        // the types the seeds must have
        if !types.is_empty() {
            let allowed: BTreeSet<String> = {
                let mut a: BTreeSet<String> =
                    types.iter().map(|t| t.as_str().to_string()).collect();
                let q = format!(
                    "SELECT DISTINCT ?sub WHERE {{ {} {} }} LIMIT 10000",
                    super::values_iris("c", types),
                    r.quads(&format!("?sub <{}>+ ?c", super::RDFS_SUBCLASS), &[])
                );
                for row in r.rows(&q, Vec::new()).map_err(eng)? {
                    if let Some(Some(Term::NamedNode(s))) = row.first() {
                        a.insert(s.as_str().to_string());
                    }
                }
                a
            };
            let named: Vec<NamedNode> = out
                .iter()
                .filter_map(|(t, _)| match t {
                    Term::NamedNode(n) => Some(n.clone()),
                    _ => None,
                })
                .collect();
            let have = super::link::types_of(r, &named, graphs).map_err(eng)?;
            out.retain(|(t, _)| match t {
                Term::NamedNode(n) => have
                    .get(n.as_str())
                    .is_some_and(|ts| ts.iter().any(|t| allowed.contains(t.as_str()))),
                _ => false,
            });
        }
        out.truncate(limit);
        Ok(out)
    }

    /// The predicate of the first vector index, by name, whose provider embeds query
    /// text.
    fn first_embedding(&self, ds: &crate::state::Dataset) -> Option<NamedNode> {
        let mut list = ds.dataset.indexes().vector().list();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list.into_iter()
            .find(|s| s.embedding.as_ref().is_some_and(|e| e.config.query_text))
            .map(|s| iri(&s.predicate))
    }

    /// An entity's types, its sampled outgoing and incoming facts, and whether it is a
    /// hub (more than [`HUB`] incoming triples).
    #[allow(clippy::type_complexity)]
    fn around(
        &self,
        r: &Reader,
        e: &Term,
        graphs: &[NamedNode],
    ) -> Result<(Vec<NamedNode>, Vec<Fact>, Vec<Fact>, bool), Error> {
        let bind = vec![("r".to_string(), e.clone())];
        let rows = r.rows(
            &format!(
                "SELECT ?p ?o ?g WHERE {{ {} }} LIMIT {OUT_ROWS}",
                r.quads("?r ?p ?o", graphs)
            ),
            bind.clone(),
        )?;
        let mut types: Vec<NamedNode> = Vec::new();
        let mut out: Vec<Fact> = Vec::new();
        for row in rows {
            let [Some(Term::NamedNode(p)), Some(o), Some(Term::NamedNode(g))] = row.as_slice()
            else {
                continue;
            };
            if p.as_str() == RDF_TYPE {
                if let Term::NamedNode(t) = o
                    && !types.contains(t)
                    && types.len() < 3
                {
                    types.push(t.clone());
                }
                continue;
            }
            if is_vector(o) || p.as_str() == RDF_REIFIES {
                continue;
            }
            out.push(Fact {
                s: e.clone(),
                p: p.clone(),
                o: o.clone(),
                g: g.clone(),
            });
        }
        let rows = r.rows(
            &format!(
                "SELECT ?s ?p ?g WHERE {{ {} }} LIMIT {}",
                r.quads("?s ?p ?r", graphs),
                HUB + 1
            ),
            bind,
        )?;
        let hub = rows.len() > HUB;
        let mut inc: Vec<Fact> = Vec::new();
        for row in rows {
            let [Some(s), Some(Term::NamedNode(p)), Some(Term::NamedNode(g))] = row.as_slice()
            else {
                continue;
            };
            if p.as_str() == RDF_TYPE || matches!(s, Term::Literal(_)) {
                continue;
            }
            inc.push(Fact {
                s: s.clone(),
                p: p.clone(),
                o: e.clone(),
                g: g.clone(),
            });
        }
        Ok((
            types,
            round_robin(out, OUT_PER_ENTITY),
            round_robin(inc, IN_PER_ENTITY),
            hub,
        ))
    }

    /// The provenance of the facts from the reifiers that reify each in its own graph
    /// and have not been invalidated: one entry per fact key, from its first reifier.
    fn provenance(&self, r: &Reader, facts: &[Fact]) -> Result<HashMap<String, Provenance>, Error> {
        let mut out: HashMap<String, Provenance> = HashMap::new();
        let mut rows: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for f in facts {
            let (Some(s), Some(o)) = (values_term(&f.s), values_term(&f.o)) else {
                continue;
            };
            if o.len() > CITED_LITERAL_BYTES {
                continue;
            }
            let row = format!("({s} {} {o})", f.p);
            if seen.insert(row.clone()) {
                rows.push(row);
            }
        }
        for chunk in rows.chunks(200) {
            let pattern = format!(
                "?r <{RDF_REIFIES}> <<( ?s ?p ?o )>> . \
                 FILTER NOT EXISTS {{ ?r <{PROV}wasInvalidatedBy> ?x }} \
                 OPTIONAL {{ ?r <{PROV}wasDerivedFrom> ?src }} \
                 OPTIONAL {{ ?r <{PROV}generatedAtTime> ?at }} \
                 OPTIONAL {{ ?r <{PROV}wasGeneratedBy> ?act OPTIONAL {{ ?act <{PROV}wasAssociatedWith> ?by }} }} \
                 OPTIONAL {{ ?r <{SPK}confidence> ?conf }} \
                 OPTIONAL {{ ?r <{SPK}quote> ?quote }}"
            );
            let q = format!(
                "SELECT ?s ?p ?o ?g ?r ?src ?at ?by ?conf ?quote WHERE {{ VALUES (?s ?p ?o) {{ {} }} {} }} LIMIT {}",
                chunk.join(" "),
                r.quads(&pattern, &[]),
                chunk.len() * 20
            );
            for row in r.rows(&q, Vec::new())? {
                let [
                    Some(s),
                    Some(Term::NamedNode(p)),
                    Some(o),
                    Some(Term::NamedNode(g)),
                    Some(reifier),
                    src,
                    at,
                    by,
                    conf,
                    quote,
                ] = row.as_slice()
                else {
                    continue;
                };
                let key = Fact {
                    s: s.clone(),
                    p: p.clone(),
                    o: o.clone(),
                    g: g.clone(),
                }
                .key();
                out.entry(key).or_insert_with(|| Provenance {
                    reifier: Some(reifier.clone()),
                    source: src.clone(),
                    at: at.clone(),
                    by: by.clone(),
                    confidence: conf.clone(),
                    quote: quote.clone(),
                });
            }
        }
        Ok(out)
    }

    /// The superseded and retracted facts of the given subjects: reifiers in the view
    /// with `prov:wasInvalidatedBy`.
    fn superseded(
        &self,
        r: &Reader,
        subjects: &HashSet<String>,
        graphs: &[NamedNode],
        max: usize,
    ) -> Result<Vec<Superseded>, Error> {
        let q = format!(
            "SELECT ?r ?tt ?g ?at ?inv WHERE {{ {} }} LIMIT {SUPERSEDED_ROWS}",
            r.quads(
                &format!(
                    "?r <{PROV}wasInvalidatedBy> ?x ; <{RDF_REIFIES}> ?tt . \
                     OPTIONAL {{ ?r <{PROV}generatedAtTime> ?at }} \
                     OPTIONAL {{ ?r <{PROV}invalidatedAtTime> ?inv }}"
                ),
                graphs
            )
        );
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for row in r.rows(&q, Vec::new())? {
            let [
                Some(reifier),
                Some(Term::Triple(t)),
                Some(Term::NamedNode(g)),
                at,
                inv,
            ] = row.as_slice()
            else {
                continue;
            };
            if !subjects.contains(&Term::from(t.subject.clone()).to_string()) {
                continue;
            }
            if !seen.insert(format!("{reifier} {g}")) {
                continue;
            }
            out.push(Superseded {
                triple: Term::Triple(t.clone()),
                graph: g.clone(),
                reifier: reifier.clone(),
                at: at.clone(),
                invalidated: inv.clone(),
            });
            if out.len() >= max {
                break;
            }
        }
        Ok(out)
    }

    /// The facts in conflict: values of a predicate that the constraints layer limits to
    /// one for a class of the entity, coming from several graphs. Each conflict lists
    /// the facts' indexes. Without the `info` endpoint the layer is not read.
    fn conflicts(
        &self,
        ds: &crate::state::Dataset,
        r: &Reader,
        entities: &[Entity],
        facts: &[Fact],
        ctx: &ErrorContext,
    ) -> Result<Vec<Vec<usize>>, ToolError> {
        if self.info_endpoint(&ds.name).is_err() || facts.is_empty() {
            return Ok(Vec::new());
        }
        let view = self
            .call
            .principal
            .view(&ds.name, crate::auth::Endpoint::Info);
        let layer = match ds.dataset.schema().constraints_at(
            &r.snap,
            &sparkles::handles::ShapesRequest::default(),
            view.as_deref(),
        ) {
            Ok(Some(l)) => l,
            Ok(None) => return Ok(Vec::new()),
            Err(Error::Unsupported(_)) | Err(Error::NotFound(_)) | Err(Error::Invalid(_)) => {
                return Ok(Vec::new());
            }
            Err(e) => return Err(ctx.engine(e)),
        };
        let mut single: HashMap<&str, BTreeSet<&str>> = HashMap::new();
        for src in &layer.sources {
            for c in &src.classes {
                for p in &c.properties {
                    if p.max_count == Some(1) {
                        single
                            .entry(c.class.as_str())
                            .or_default()
                            .insert(p.path.as_str());
                    }
                }
            }
        }
        let mut out = Vec::new();
        for e in entities {
            let preds: BTreeSet<&str> = e
                .types
                .iter()
                .filter_map(|t| single.get(t.as_str()))
                .flatten()
                .copied()
                .collect();
            for p in preds {
                let idx: Vec<usize> = facts
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.s == e.term && f.p.as_str() == p)
                    .map(|(i, _)| i)
                    .collect();
                let values: BTreeSet<String> =
                    idx.iter().map(|&i| facts[i].o.to_string()).collect();
                let graphs: BTreeSet<&str> = idx.iter().map(|&i| facts[i].g.as_str()).collect();
                if values.len() > 1 && graphs.len() > 1 {
                    out.push(idx);
                }
            }
        }
        Ok(out)
    }
}

/// One rank per subject term, best score first, ties sharing a rank.
fn rank_terms(hits: impl Iterator<Item = (NamedNode, f64)>) -> HashMap<String, usize> {
    let mut best: HashMap<String, f64> = HashMap::new();
    for (s, score) in hits {
        let e = best.entry(s.as_str().to_string()).or_insert(score);
        if score > *e {
            *e = score;
        }
    }
    let scores: Vec<f64> = best.values().copied().collect();
    best.iter()
        .map(|(s, &v)| (s.clone(), 1 + scores.iter().filter(|&&o| o > v).count()))
        .collect()
}

/// The triples `?r rdf:reifies <<( s p o )>>` of the hit subjects that are reifiers,
/// when the triple is asserted in the reifier's graph: hit → that fact.
fn reified_by(
    r: &Reader,
    hits: &[NamedNode],
    graphs: &[NamedNode],
) -> Result<HashMap<String, Fact>, Error> {
    let mut out = HashMap::new();
    if hits.is_empty() {
        return Ok(out);
    }
    let q = format!(
        "SELECT ?r ?s ?p ?o ?g WHERE {{ {} {} }} LIMIT {}",
        super::values_iris("r", hits),
        r.quads(
            &format!("?r <{RDF_REIFIES}> <<( ?s ?p ?o )>> . ?s ?p ?o ."),
            graphs
        ),
        hits.len() * 4
    );
    for row in r.rows(&q, Vec::new())? {
        if let [
            Some(Term::NamedNode(reifier)),
            Some(s),
            Some(Term::NamedNode(p)),
            Some(o),
            Some(Term::NamedNode(g)),
        ] = row.as_slice()
            && !matches!(s, Term::Literal(_))
        {
            out.entry(reifier.as_str().to_string())
                .or_insert_with(|| Fact {
                    s: s.clone(),
                    p: p.clone(),
                    o: o.clone(),
                    g: g.clone(),
                });
        }
    }
    Ok(out)
}

/// At most `n` facts, sampled round-robin by predicate (in order of first appearance),
/// so that one large predicate does not hide the others.
fn round_robin(facts: Vec<Fact>, n: usize) -> Vec<Fact> {
    let mut order: Vec<String> = Vec::new();
    let mut by: HashMap<String, Vec<Fact>> = HashMap::new();
    for f in facts {
        let k = f.p.as_str().to_string();
        if !by.contains_key(&k) {
            order.push(k.clone());
        }
        by.entry(k).or_default().push(f);
    }
    let mut out = Vec::new();
    let mut i = 0;
    while out.len() < n {
        let mut any = false;
        for k in &order {
            if let Some(f) = by[k].get(i) {
                any = true;
                if out.len() < n {
                    out.push(f.clone());
                }
            }
        }
        if !any {
            break;
        }
        i += 1;
    }
    out
}

/// What a result renders.
struct Doc<'a> {
    dataset: &'a str,
    commit: u64,
    seeds: usize,
    entities: &'a [Entity],
    facts: &'a [Fact],
    provenance: &'a HashMap<String, Provenance>,
    superseded: &'a [Superseded],
    conflicts: &'a [Vec<usize>],
    prefixes: &'a Prefixes,
}

/// A time or number as it is when its lexical form is plain, else as a quoted term.
fn plain(t: &Term, terms: &mut Terms, allowed: fn(char) -> bool) -> String {
    match t {
        Term::Literal(l) if !l.value().is_empty() && l.value().chars().all(allowed) => {
            l.value().to_string()
        }
        t => terms.term(t),
    }
}

fn time_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '-' | ':' | 'T' | 'Z' | '.' | '+')
}

fn number_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '-' | '.' | 'e' | 'E' | '+')
}

fn quote_term(t: &Term) -> Term {
    match t {
        Term::Literal(l) => {
            let v = l.value();
            match v.char_indices().nth(QUOTE_CHARS) {
                Some((i, _)) => Term::Literal(Literal::new_simple_literal(format!("{}…", &v[..i]))),
                None => Term::Literal(Literal::new_simple_literal(v)),
            }
        }
        t => t.clone(),
    }
}

impl Doc<'_> {
    /// The citations of the first `n` facts: the citation number of each fact, and the
    /// citations in order of first use.
    fn citations(&self, n: usize) -> (Vec<usize>, Vec<(Citation, Option<Term>)>) {
        let mut ids: HashMap<Citation, usize> = HashMap::new();
        let mut list: Vec<(Citation, Option<Term>)> = Vec::new();
        let mut of = Vec::with_capacity(n);
        for f in &self.facts[..n] {
            let p = self.provenance.get(&f.key()).cloned().unwrap_or_default();
            let c = Citation {
                graph: f.g.clone(),
                source: p.source,
                at: p.at,
                by: p.by,
                confidence: p.confidence,
                quote: p.quote,
            };
            let id = match ids.get(&c) {
                Some(&id) => {
                    // a citation names its reifier only while one reifier stands behind it
                    if list[id - 1].1 != p.reifier {
                        list[id - 1].1 = None;
                    }
                    id
                }
                None => {
                    list.push((c.clone(), p.reifier));
                    ids.insert(c, list.len());
                    list.len()
                }
            };
            of.push(id);
        }
        (of, list)
    }

    /// The entities shown with the first `n` facts: every seed, and every entity with a
    /// fact among them.
    fn shown(&self, n: usize) -> Vec<(&Entity, Vec<usize>)> {
        self.entities
            .iter()
            .filter_map(|e| {
                let f: Vec<usize> = e.facts.iter().copied().filter(|&i| i < n).collect();
                (e.seed.is_some() || !f.is_empty()).then_some((e, f))
            })
            .collect()
    }

    fn graph(&self, g: &NamedNode, terms: &mut Terms) -> String {
        if g.as_str() == DEFAULT_GRAPH {
            "default".into()
        } else {
            terms.iri(g.as_str())
        }
    }

    fn text(&self, n: usize, truncated: bool) -> String {
        let mut terms = Terms::new(self.prefixes, LITERAL_CHARS);
        let (cite, list) = self.citations(n);
        let shown = self.shown(n);
        let conflicting: HashSet<usize> = self.conflicts.iter().flatten().copied().collect();
        let mut body = String::new();
        for (e, facts) in &shown {
            body.push_str("## ");
            body.push_str(&terms.term(&e.term));
            if let Some(l) = &e.label {
                body.push_str(&format!(" \"{l}\""));
            }
            if !e.types.is_empty() {
                let t: Vec<String> = e.types.iter().map(|t| terms.iri(t.as_str())).collect();
                body.push_str(&format!(" ({})", t.join(" ")));
            }
            match e.seed {
                Some(s) => body.push_str(&format!(" seed={s}")),
                None => body.push_str(&format!(" hop={}", e.hop)),
            }
            body.push('\n');
            for &i in facts {
                let f = &self.facts[i];
                body.push_str(&format!(
                    "{} {} {} [{}]",
                    terms.term(&f.s),
                    terms.iri(f.p.as_str()),
                    terms.term(&f.o),
                    cite[i]
                ));
                if conflicting.contains(&i) {
                    body.push_str(" conflict");
                }
                body.push('\n');
            }
        }
        body.push_str("# citations\n");
        for (i, (c, reifier)) in list.iter().enumerate() {
            body.push_str(&format!(
                "[{}] graph={}",
                i + 1,
                self.graph(&c.graph, &mut terms)
            ));
            if let Some(r) = reifier {
                body.push_str(&format!(" reifier={}", terms.term(r)));
            }
            if let Some(s) = &c.source {
                body.push_str(&format!(" source={}", terms.term(s)));
            }
            if let Some(t) = &c.at {
                body.push_str(&format!(" at={}", plain(t, &mut terms, time_char)));
            }
            if let Some(b) = &c.by {
                body.push_str(&format!(" by={}", terms.term(b)));
            }
            if let Some(x) = &c.confidence {
                body.push_str(&format!(
                    " confidence={}",
                    plain(x, &mut terms, number_char)
                ));
            }
            if let Some(q) = &c.quote {
                body.push_str(&format!(" quote={}", terms.term(&quote_term(q))));
            }
            body.push('\n');
        }
        if !self.superseded.is_empty() {
            body.push_str("# superseded\n");
            for s in self.superseded {
                let Term::Triple(t) = &s.triple else { continue };
                body.push_str(&format!(
                    "{} {} {} graph={} reifier={}",
                    terms.term(&t.subject.clone().into()),
                    terms.iri(t.predicate.as_str()),
                    terms.term(&t.object),
                    self.graph(&s.graph, &mut terms),
                    terms.term(&s.reifier)
                ));
                if let Some(a) = &s.at {
                    body.push_str(&format!(" at={}", plain(a, &mut terms, time_char)));
                }
                if let Some(i) = &s.invalidated {
                    body.push_str(&format!(" invalidated={}", plain(i, &mut terms, time_char)));
                }
                body.push('\n');
            }
        }
        let shown_facts: usize = shown.iter().map(|(_, f)| f.len()).sum();
        let mut out = format!(
            "# dataset={} commit={} seeds={} facts={} truncated={}\n",
            self.dataset, self.commit, self.seeds, shown_facts, truncated
        );
        out.push_str(&body);
        let used = terms.used();
        if !used.is_empty() {
            out.push_str("# prefixes");
            for (name, ns) in &used {
                out.push_str(&format!(" {name}: <"));
                escape_into(&mut out, ns);
                out.push('>');
            }
            out.push('\n');
        }
        out
    }

    fn json(&self, n: usize, truncated: bool) -> String {
        let mut terms = Terms::new(self.prefixes, LITERAL_CHARS);
        let (cite, list) = self.citations(n);
        let shown = self.shown(n);
        let entities: Vec<Value> = shown
            .iter()
            .map(|(e, facts)| {
                let mut j = json!({
                    "iri": terms.term(&e.term),
                    "types": e.types.iter().map(|t| terms.iri(t.as_str())).collect::<Vec<_>>(),
                    "hop": e.hop,
                    "facts": facts.iter().map(|&i| {
                        let f = &self.facts[i];
                        json!({
                            "s": terms.term(&f.s),
                            "p": terms.iri(f.p.as_str()),
                            "o": terms.term(&f.o),
                            "citation": cite[i],
                        })
                    }).collect::<Vec<_>>(),
                });
                if let Some(l) = &e.label {
                    j["label"] = l.clone().into();
                }
                if let Some(s) = e.seed {
                    j["seed"] = s.into();
                }
                j
            })
            .collect();
        let citations: Vec<Value> = list
            .iter()
            .enumerate()
            .map(|(i, (c, reifier))| {
                let mut j = json!({ "id": i + 1, "graph": self.graph(&c.graph, &mut terms) });
                if let Some(r) = reifier {
                    j["reifier"] = terms.term(r).into();
                }
                if let Some(s) = &c.source {
                    j["source"] = terms.term(s).into();
                }
                if let Some(t) = &c.at {
                    j["at"] = plain(t, &mut terms, time_char).into();
                }
                if let Some(b) = &c.by {
                    j["by"] = terms.term(b).into();
                }
                if let Some(x) = &c.confidence {
                    j["confidence"] = plain(x, &mut terms, number_char).into();
                }
                if let Some(q) = &c.quote {
                    j["quote"] = terms.term(&quote_term(q)).into();
                }
                j
            })
            .collect();
        let conflicts: Vec<Value> = self
            .conflicts
            .iter()
            .filter_map(|idx| {
                let idx: Vec<usize> = idx.iter().copied().filter(|&i| i < n).collect();
                (idx.len() > 1).then(|| {
                    let f = &self.facts[idx[0]];
                    json!({
                        "s": terms.term(&f.s),
                        "p": terms.iri(f.p.as_str()),
                        "values": idx.iter().map(|&i| json!({
                            "o": terms.term(&self.facts[i].o),
                            "citation": cite[i],
                        })).collect::<Vec<_>>(),
                    })
                })
            })
            .collect();
        let mut out = json!({
            "dataset": self.dataset,
            "commit": self.commit,
            "entities": entities,
            "citations": citations,
            "conflicts": conflicts,
            "truncated": truncated,
        });
        if !self.superseded.is_empty() {
            out["superseded"] = self
                .superseded
                .iter()
                .filter_map(|s| {
                    let Term::Triple(t) = &s.triple else {
                        return None;
                    };
                    let mut j = json!({
                        "s": terms.term(&t.subject.clone().into()),
                        "p": terms.iri(t.predicate.as_str()),
                        "o": terms.term(&t.object),
                        "graph": self.graph(&s.graph, &mut terms),
                        "reifier": terms.term(&s.reifier),
                    });
                    if let Some(a) = &s.at {
                        j["at"] = plain(a, &mut terms, time_char).into();
                    }
                    if let Some(i) = &s.invalidated {
                        j["invalidatedAt"] = plain(i, &mut terms, time_char).into();
                    }
                    Some(j)
                })
                .collect::<Vec<_>>()
                .into();
        }
        out["prefixes"] = json!(terms.used());
        out.to_string()
    }
}
