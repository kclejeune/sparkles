//! `link_entities` (C17 §5.4): the entities of the caller's view that a mention may
//! name, by exact label, normalized label, BM25 and vector similarity.

use super::text::{normalize, phrase_query, plain_query, words};
use super::{
    OWL_SAME_AS, RDF_TYPE, RDFS_SUBCLASS, RRF_K, Reader, SKOS_ALT, SKOS_EXACT, graphs_arg, iri,
    iri_arg, is_vector, score_json, values_iris,
};
use crate::mcp::Outcome;
use crate::mcp::errors::{ErrorContext, ToolError};
use crate::mcp::render::{self, Prefixes, Terms};
use crate::mcp::tools::{Tools, bounded, dataset_prefixes, parse};
use crate::state::Dataset;
use oxrdf::{Literal, NamedNode, Term};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::error::Error;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Hits each search list keeps per mention.
pub(crate) const LIST_DEPTH: usize = 50;
/// Sample triples per candidate.
const SAMPLE_TRIPLES: usize = 5;
/// Rows read to sample a candidate's triples from.
const SAMPLE_ROWS: usize = 200;

/// The predicates whose text describes an entity, which an embedding index may cover
/// besides the labels.
const DESCRIPTIONS: [&str; 4] = [
    "http://www.w3.org/2000/01/rdf-schema#comment",
    "http://schema.org/description",
    "http://www.w3.org/2004/02/skos/core#definition",
    "http://purl.org/dc/terms/description",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LinkArgs {
    dataset: Option<String>,
    mentions: Vec<MentionArg>,
    k: Option<u64>,
    label_predicates: Option<Vec<String>>,
    graphs: Option<Vec<String>>,
    reasoning: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
    timeout_seconds: Option<f64>,
    /// false leaves the vector index out, whatever indexes the dataset has
    vectors: Option<bool>,
    /// Include scoped, versioned evidence beyond the bounded display lists.
    evidence: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MentionArg {
    text: String,
    types: Option<Vec<String>>,
    context: Option<String>,
}

/// What every mention of a call searches with.
pub(crate) struct LinkSetup {
    /// the label predicates, in priority order
    pub labels: Vec<NamedNode>,
    /// the graphs to search (empty: every graph of the view)
    pub graphs: Vec<NamedNode>,
    /// the language tags of the label predicates' literals
    pub tags: Vec<String>,
    /// the label predicates the full-text index covers (`Some(empty)`: every predicate)
    pub text: Option<Vec<NamedNode>>,
    /// the predicate of the embedding index that covers labels or descriptions
    pub vector: Option<NamedNode>,
    /// The language inventory covers the same caller view as the entity queries.
    pub languages_complete: bool,
    /// The known label literals use the simple or language-tagged forms looked up.
    pub label_forms_complete: bool,
}

/// One mention to link.
pub(crate) struct Mention {
    pub text: String,
    pub types: Vec<NamedNode>,
    pub context: Option<String>,
}

/// A candidate entity of a mention.
pub(crate) struct Candidate {
    pub iri: NamedNode,
    pub exact: bool,
    pub normalized: bool,
    pub text_rank: Option<usize>,
    pub vector_rank: Option<usize>,
    /// every word of the mention is a word of one of its labels
    pub words: bool,
    pub types: Vec<NamedNode>,
    /// `(label predicate rank, literal)`
    pub labels: Vec<(usize, Literal)>,
    pub type_match: bool,
    pub score: f64,
    pub same_as: Vec<NamedNode>,
    pub triples: Vec<(NamedNode, NamedNode, Term)>,
    /// Proof applies to named types and literal labels in this call's query view.
    pub evidence: Option<DetailEvidence>,
}

pub(crate) struct DetailEvidence {
    pub types_complete: bool,
    pub labels_complete: bool,
}

pub(crate) struct LinkEvidence {
    pub observed_candidates: usize,
    pub pool_truncated: bool,
    pub display_truncated: bool,
    pub exact_labels_complete: bool,
    pub type_hierarchy_complete: bool,
}

impl Candidate {
    fn new(iri: NamedNode) -> Candidate {
        Candidate {
            iri,
            exact: false,
            normalized: false,
            text_rank: None,
            vector_rank: None,
            words: false,
            types: Vec::new(),
            labels: Vec::new(),
            type_match: true,
            score: 0.0,
            same_as: Vec::new(),
            triples: Vec::new(),
            evidence: None,
        }
    }

    /// 0 for an exact label, 1 for a normalized one, 2 for the rest.
    fn tier(&self) -> u8 {
        if self.exact {
            0
        } else if self.normalized {
            1
        } else {
            2
        }
    }

    fn label_match(&self) -> bool {
        self.exact || self.normalized
    }
}

/// The candidates of one mention, best first, with its verdict.
pub(crate) struct Linked {
    pub verdict: &'static str,
    pub candidates: Vec<Candidate>,
    pub evidence: Option<LinkEvidence>,
}

/// Ranks of a list of subjects in score order (best first, ties share a rank), one
/// entry per subject: its best.
fn ranked(rows: Vec<(NamedNode, f64)>, higher_better: bool) -> HashMap<String, usize> {
    let mut best: HashMap<String, f64> = HashMap::new();
    for (s, score) in rows {
        let e = best.entry(s.as_str().to_string()).or_insert(score);
        if (higher_better && score > *e) || (!higher_better && score < *e) {
            *e = score;
        }
    }
    let scores: Vec<f64> = best.values().copied().collect();
    best.iter()
        .map(|(s, &v)| {
            let better = scores
                .iter()
                .filter(|&&o| if higher_better { o > v } else { o < v })
                .count();
            (s.clone(), better + 1)
        })
        .collect()
}

fn score_of(t: Option<&Term>) -> f64 {
    match t {
        Some(Term::Literal(l)) => l.value().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Whether a search failed because the index cannot answer (no index, a past commit,
/// an index behind the store, a provider that is down or refused): the tools then go
/// on without that list.
pub(crate) fn unavailable(e: &Error) -> bool {
    matches!(
        e,
        Error::Invalid(_)
            | Error::Unsupported(_)
            | Error::TextUnavailable(_)
            | Error::Service(_)
            | Error::NotPermitted(_)
            | Error::HistoryUnsupported(_)
    )
}

impl Tools<'_> {
    pub(crate) fn link_entities(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: LinkArgs = parse(args)?;
        if a.mentions.is_empty() || a.mentions.len() > 20 {
            return Err(ToolError::bad_argument(
                "mentions must hold 1 to 20 mentions",
            ));
        }
        let k = bounded("k", a.k, 5, 1, 20)? as usize;
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let mut mentions = Vec::with_capacity(a.mentions.len());
        for m in &a.mentions {
            if m.text.trim().is_empty() || m.text.chars().count() > 200 {
                return Err(ToolError::bad_argument(
                    "a mention's text has 1 to 200 characters",
                ));
            }
            if m.context.as_ref().is_some_and(|c| c.chars().count() > 500) {
                return Err(ToolError::bad_argument(
                    "a mention's context has at most 500 characters",
                ));
            }
            let types = m.types.as_deref().unwrap_or_default();
            if types.len() > 5 {
                return Err(ToolError::bad_argument("a mention has at most 5 types"));
            }
            mentions.push(Mention {
                text: m.text.clone(),
                types: types
                    .iter()
                    .map(|t| iri_arg(t, &prefix_map, "types"))
                    .collect::<Result<_, _>>()?,
                context: m.context.clone(),
            });
        }
        let labels = match &a.label_predicates {
            None => default_labels(),
            Some(v) if v.is_empty() || v.len() > 20 => {
                return Err(ToolError::bad_argument(
                    "labelPredicates must hold 1 to 20 IRIs",
                ));
            }
            Some(v) => v
                .iter()
                .map(|p| iri_arg(p, &prefix_map, "labelPredicates"))
                .collect::<Result<_, _>>()?,
        };
        let graphs = graphs_arg(a.graphs.as_deref(), &prefix_map)?;
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let r = self.reader(&ds, a.at_commit, a.at.as_ref(), a.reasoning, deadline, &ctx)?;
        let mut setup = self.link_setup(&ds, &r, labels, graphs, &ctx)?;
        if a.vectors == Some(false) {
            setup.vector = None;
        }
        let mut text_used = false;
        let mut vector_used = false;
        let mut terms = Terms::new(&prefixes, 300);
        let mut out_mentions = Vec::with_capacity(mentions.len());
        for m in &mentions {
            let (linked, t, v) =
                self.link_with_evidence(&r, &setup, m, k, a.evidence == Some(true), &ctx)?;
            text_used |= t;
            vector_used |= v;
            let mut mention = json!({
                "text": render::label_text(&m.text),
                "verdict": linked.verdict,
                "candidates": linked
                    .candidates
                    .iter()
                    .map(|c| candidate_json(c, &mut terms, &setup.labels))
                    .collect::<Vec<_>>(),
            });
            if let Some(evidence) = &linked.evidence {
                mention["evidence"] = json!({
                    "version": 1,
                    "observedCandidates": evidence.observed_candidates,
                    "poolTruncated": evidence.pool_truncated,
                    "displayTruncated": evidence.display_truncated,
                    "exactStringLabelsComplete": evidence.exact_labels_complete,
                    "typeHintLookupComplete": evidence.type_hierarchy_complete,
                    // Ranked text/vector search is bounded, not an exhaustive universe.
                    "normalizedLabelsComplete": false,
                    "candidateUniverseComplete": false,
                });
            }
            out_mentions.push(mention);
        }
        let value = json!({
            "dataset": ds.name,
            "commit": r.snap.commit,
            "mentions": out_mentions,
            "search": { "text": text_used, "vector": vector_used },
            "prefixes": terms.used(),
        });
        if a.evidence == Some(true) {
            evidence_bytes(&value, self.cfg().max_bytes, &ctx)?;
        }
        Ok(Outcome::Structured(value))
    }

    /// The label predicates' language tags (from the schema report of the view when
    /// the caller may read it, else from the labels themselves), the label predicates
    /// the full-text index covers, and the embedding index that covers labels or
    /// descriptions.
    pub(crate) fn link_setup(
        &self,
        ds: &Dataset,
        r: &Reader,
        labels: Vec<NamedNode>,
        graphs: Vec<NamedNode>,
        ctx: &ErrorContext,
    ) -> Result<LinkSetup, ToolError> {
        let mut tags: BTreeSet<String> = BTreeSet::new();
        let same_scope = r.opts.rdfs.is_none()
            && self
                .call
                .principal
                .view(&ds.name, crate::auth::Endpoint::Info)
                .as_deref()
                == r.opts.graphs.as_deref();
        let mut languages_complete = same_scope;
        let mut label_forms_complete = same_scope;
        match self.view_report(ds, r, ctx)? {
            Some(rep) => {
                // A report that does not enumerate every predicate cannot establish
                // absence of another label form.
                languages_complete &= rep.totals.predicates == rep.predicates.len();
                label_forms_complete &= rep.totals.predicates == rep.predicates.len();
                for p in &labels {
                    let Ok(i) = rep
                        .predicates
                        .binary_search_by(|e| e.iri.as_str().cmp(p.as_str()))
                    else {
                        continue;
                    };
                    let (objects, sampled) =
                        self.entry_objects(ds, &r.snap, &rep, &rep.predicates[i], r.deadline, ctx)?;
                    languages_complete &= sampled.is_none()
                        && objects.literals.iter().all(|group| {
                            group.datatype
                                != "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString"
                                || group.triples == 0
                                || group
                                    .languages
                                    .as_ref()
                                    .is_some_and(|languages| !languages.is_empty())
                        });
                    label_forms_complete &= sampled.is_none()
                        && objects.literals.iter().all(|group| {
                            matches!(
                                group.datatype.as_str(),
                                "http://www.w3.org/2001/XMLSchema#string"
                                    | "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString"
                            )
                        });
                    for g in &objects.literals {
                        for l in g.languages.iter().flatten() {
                            tags.insert(l.lang.clone());
                        }
                    }
                }
            }
            None => {
                let q = format!(
                    "SELECT DISTINCT ?t WHERE {{ {} {} BIND(LANG(?l) AS ?t) FILTER(?t != \"\") }} LIMIT 100",
                    values_iris("lp", &labels),
                    r.quads("?s ?lp ?l", &graphs)
                );
                let rows = r.rows(&q, Vec::new()).map_err(|e| ctx.engine(e))?;
                languages_complete = rows.len() < 100;
                // LANG alone does not establish which other literal forms occur.
                label_forms_complete = false;
                for row in rows {
                    if let Some(Some(Term::Literal(t))) = row.first() {
                        tags.insert(t.value().to_string());
                    }
                }
            }
        }
        let text = if ds.dataset.indexes().text().enabled() {
            match ds
                .dataset
                .indexes()
                .text()
                .status()
                .map(|s| s.config.predicates)
            {
                Some(sparkles::text::PredicateSet::Only(only)) => {
                    let covered: Vec<NamedNode> = labels
                        .iter()
                        .filter(|p| only.iter().any(|o| o == p.as_str()))
                        .cloned()
                        .collect();
                    (!covered.is_empty()).then_some(covered)
                }
                // every predicate is indexed: search the labels
                _ => Some(labels.clone()),
            }
        } else {
            None
        };
        let mut vector = None;
        let mut indexes = ds.dataset.indexes().vector().list();
        indexes.sort_by(|a, b| a.name.cmp(&b.name));
        for s in indexes {
            let Some(e) = &s.embedding else { continue };
            if !e.config.query_text {
                continue;
            }
            let covers = e.config.predicates.iter().any(|p| {
                labels.iter().any(|l| l.as_str() == p) || DESCRIPTIONS.contains(&p.as_str())
            });
            if covers {
                vector = Some(iri(&s.predicate));
                break;
            }
        }
        Ok(LinkSetup {
            labels,
            graphs,
            tags: tags.into_iter().collect(),
            text,
            vector,
            languages_complete,
            label_forms_complete,
        })
    }

    /// The candidates of one mention, with whether the text and vector searches took
    /// part.
    pub(crate) fn link(
        &self,
        r: &Reader,
        setup: &LinkSetup,
        m: &Mention,
        k: usize,
        ctx: &ErrorContext,
    ) -> Result<(Linked, bool, bool), ToolError> {
        self.link_with_evidence(r, setup, m, k, false, ctx)
    }

    #[allow(clippy::too_many_arguments)]
    fn link_with_evidence(
        &self,
        r: &Reader,
        setup: &LinkSetup,
        m: &Mention,
        k: usize,
        evidence: bool,
        ctx: &ErrorContext,
    ) -> Result<(Linked, bool, bool), ToolError> {
        let eng = |e| ctx.engine(e);
        let mut pool: BTreeMap<String, Candidate> = BTreeMap::new();
        // 1. exact labels, through the index: the text as a simple literal and with each
        // language tag of the label predicates
        let mut forms: Vec<String> = vec![Literal::new_simple_literal(m.text.as_str()).to_string()];
        let mut forms_complete = setup.languages_complete && setup.label_forms_complete;
        for t in &setup.tags {
            if let Ok(l) = Literal::new_language_tagged_literal(m.text.as_str(), t.as_str()) {
                forms.push(l.to_string());
            } else {
                forms_complete = false;
            }
        }
        let q = format!(
            "SELECT DISTINCT ?s WHERE {{ {} VALUES ?l {{ {} }} {} FILTER(isIRI(?s)) }} LIMIT {LIST_DEPTH}",
            values_iris("lp", &setup.labels),
            forms.join(" "),
            r.quads("?s ?lp ?l", &setup.graphs),
        );
        let exact_rows = r.rows(&q, Vec::new()).map_err(eng)?;
        let exact_labels_complete = forms_complete && exact_rows.len() < LIST_DEPTH;
        for row in exact_rows {
            if let Some(Some(Term::NamedNode(s))) = row.first() {
                pool.entry(s.as_str().to_string())
                    .or_insert_with(|| Candidate::new(s.clone()))
                    .exact = true;
            }
        }
        // 2. and 3. normalized labels and BM25, through the full-text index
        let mut text_used = false;
        if let Some(preds) = &setup.text {
            let norm = normalize(&m.text);
            let phrase = phrase_query(&m.text);
            let plain = plain_query(&m.text);
            let mut ok = true;
            if let Some(phrase) = phrase {
                match self.text_hits(r, preds, &phrase, &setup.graphs) {
                    Ok(hits) => {
                        for (s, _, lit) in hits {
                            if normalize(&lit) == norm {
                                let c = pool
                                    .entry(s.as_str().to_string())
                                    .or_insert_with(|| Candidate::new(s.clone()));
                                if !c.exact {
                                    c.normalized = true;
                                }
                            }
                        }
                    }
                    Err(e) if unavailable(&e) => ok = false,
                    Err(e) => return Err(eng(e)),
                }
            }
            if ok && let Some(plain) = plain {
                match self.text_hits(r, preds, &plain, &setup.graphs) {
                    Ok(hits) => {
                        let ranks =
                            ranked(hits.into_iter().map(|(s, sc, _)| (s, sc)).collect(), true);
                        for (s, rank) in ranks {
                            pool.entry(s.clone())
                                .or_insert_with(|| Candidate::new(iri(&s)))
                                .text_rank = Some(rank);
                        }
                    }
                    Err(e) if unavailable(&e) => ok = false,
                    Err(e) => return Err(eng(e)),
                }
            }
            text_used = ok;
        }
        // 4. vectors, of the text and its context
        let mut vector_used = false;
        if let Some(pred) = &setup.vector {
            let input = match &m.context {
                Some(c) => format!("{}\n{c}", m.text),
                None => m.text.clone(),
            };
            match crate::ask::profile::nested(
                crate::ask::profile::Stage::LinkerVectorSearch,
                r.snap.commit,
                1,
                || self.vector_hits(r, pred, &input, &setup.graphs),
            ) {
                Ok(hits) => {
                    for (s, rank) in ranked(hits, true) {
                        pool.entry(s.clone())
                            .or_insert_with(|| Candidate::new(iri(&s)))
                            .vector_rank = Some(rank);
                    }
                    vector_used = true;
                }
                Err(e) if unavailable(&e) => {}
                Err(e) => return Err(eng(e)),
            }
        }
        // the fused score, over the exact, normalized, text and vector lists
        for c in pool.values_mut() {
            c.score = [
                c.exact.then_some(1),
                c.normalized.then_some(1),
                c.text_rank,
                c.vector_rank,
            ]
            .iter()
            .flatten()
            .map(|&r| 1.0 / (RRF_K + r as f64))
            .sum();
        }
        // keep the label matches and the best of the rest
        let observed_candidates = pool.len();
        let mut all: Vec<Candidate> = pool.into_values().collect();
        all.sort_by(|a, b| {
            a.tier()
                .cmp(&b.tier())
                .then_with(|| b.score.total_cmp(&a.score))
                .then_with(|| a.iri.as_str().cmp(b.iri.as_str()))
        });
        all.truncate(LIST_DEPTH);
        // types and labels of the pool
        let iris: Vec<NamedNode> = all.iter().map(|c| c.iri.clone()).collect();
        let types = type_details(r, &iris, &setup.graphs, evidence).map_err(eng)?;
        let labels =
            label_details(r, &iris, &setup.labels, &setup.graphs, evidence).map_err(eng)?;
        let (allowed, type_hierarchy_complete) = if m.types.is_empty() {
            (None, true)
        } else {
            let (allowed, complete) = subclass_details(r, &m.types).map_err(eng)?;
            (Some(allowed), complete)
        };
        let mention_words = words(&m.text);
        for c in &mut all {
            c.types = types
                .values
                .get(c.iri.as_str())
                .cloned()
                .unwrap_or_default();
            c.labels = labels
                .values
                .get(c.iri.as_str())
                .cloned()
                .unwrap_or_default();
            if evidence {
                c.evidence = Some(DetailEvidence {
                    types_complete: types.complete.get(c.iri.as_str()).copied().unwrap_or(false),
                    labels_complete: labels
                        .complete
                        .get(c.iri.as_str())
                        .copied()
                        .unwrap_or(false),
                });
            }
            if let Some(allowed) = &allowed {
                c.type_match = c.types.iter().any(|t| allowed.contains(t.as_str()));
            }
            c.words = !mention_words.is_empty()
                && c.labels.iter().any(|(_, l)| {
                    let lw = words(l.value());
                    mention_words.iter().all(|w| lw.contains(w))
                });
        }
        all.sort_by(|a, b| {
            b.type_match
                .cmp(&a.type_match)
                .then_with(|| a.tier().cmp(&b.tier()))
                .then_with(|| b.score.total_cmp(&a.score))
                .then_with(|| a.iri.as_str().cmp(b.iri.as_str()))
        });
        let exact = all
            .iter()
            .filter(|c| c.label_match() && c.type_match)
            .count();
        let partial = all
            .iter()
            .filter(|c| c.words && c.type_match && !c.label_match())
            .count();
        let verdict = match exact {
            1 => "exact",
            n if n > 1 => "ambiguous",
            // two entities whose labels hold every word of the mention: a person must
            // choose, as between two exact labels
            _ if partial > 1 => "ambiguous",
            _ if all.is_empty() => "none",
            _ => "candidates",
        };
        all.truncate(k);
        // what tells the candidates apart: links between them and sample triples
        let top: Vec<NamedNode> = all.iter().map(|c| c.iri.clone()).collect();
        let links = same_as(r, &top, &setup.graphs).map_err(eng)?;
        for c in &mut all {
            c.same_as = links.get(c.iri.as_str()).cloned().unwrap_or_default();
            c.triples =
                sample(r, &c.iri, &setup.labels, &setup.graphs, SAMPLE_TRIPLES).map_err(eng)?;
        }
        Ok((
            Linked {
                verdict,
                candidates: all,
                evidence: evidence.then_some(LinkEvidence {
                    observed_candidates,
                    pool_truncated: observed_candidates > LIST_DEPTH,
                    display_truncated: observed_candidates > k,
                    exact_labels_complete,
                    type_hierarchy_complete,
                }),
            },
            text_used,
            vector_used,
        ))
    }

    /// Full-text hits of `query` over `preds` in every graph of the view, best first:
    /// `(subject, score, matched text)`.
    pub(crate) fn text_hits(
        &self,
        r: &Reader,
        preds: &[NamedNode],
        query: &str,
        graphs: &[NamedNode],
    ) -> Result<Vec<(NamedNode, f64, String)>, Error> {
        let mut args: Vec<String> = preds.iter().map(NamedNode::to_string).collect();
        args.push(Literal::new_simple_literal(query).to_string());
        args.push(LIST_DEPTH.to_string());
        let call = format!(
            "(?s ?score ?lit) <http://jena.apache.org/text#query> ({})",
            args.join(" ")
        );
        let q = format!(
            "SELECT ?s ?score ?lit WHERE {{ {} FILTER(isIRI(?s)) }} ORDER BY DESC(?score) LIMIT {}",
            r.quads(&call, graphs),
            2 * LIST_DEPTH
        );
        Ok(r.rows(&q, Vec::new())?
            .into_iter()
            .filter_map(|row| match row.as_slice() {
                [Some(Term::NamedNode(s)), score, lit] => Some((
                    s.clone(),
                    score_of(score.as_ref()),
                    match lit {
                        Some(Term::Literal(l)) => l.value().to_string(),
                        _ => String::new(),
                    },
                )),
                _ => None,
            })
            .collect())
    }

    /// Nearest subjects of a text's embedding under the index on `pred`, in every graph
    /// of the view: `(subject, cosine score)`.
    pub(crate) fn vector_hits(
        &self,
        r: &Reader,
        pred: &NamedNode,
        text: &str,
        graphs: &[NamedNode],
    ) -> Result<Vec<(NamedNode, f64)>, Error> {
        let call = format!(
            "(?s ?score) <{}> ({pred} {} {LIST_DEPTH} \"metric:cosine\")",
            sparkles::vector::VECTOR_SEARCH,
            Literal::new_simple_literal(text)
        );
        let q = format!(
            "SELECT ?s ?score WHERE {{ {} FILTER(isIRI(?s)) }} ORDER BY DESC(?score) LIMIT {}",
            r.quads(&call, graphs),
            2 * LIST_DEPTH
        );
        Ok(r.rows(&q, Vec::new())?
            .into_iter()
            .filter_map(|row| match row.as_slice() {
                [Some(Term::NamedNode(s)), score] => Some((s.clone(), score_of(score.as_ref()))),
                _ => None,
            })
            .collect())
    }
}

/// Bound structured evidence without allocating a second serialized response. MCP's
/// mirrored text and protocol envelope are outside this structured-content budget.
fn evidence_bytes(value: &Value, limit: usize, ctx: &ErrorContext) -> Result<(), ToolError> {
    struct Limited {
        remaining: usize,
    }
    impl std::io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.remaining {
                return Err(std::io::Error::other("evidence result byte limit"));
            }
            self.remaining -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Limited { remaining: limit }, value).map_err(|_| {
        ctx.engine(Error::BudgetExceeded(sparkles::error::Budget {
            kind: sparkles::error::BudgetKind::ResultBytes,
            limit: limit as u64,
            requested: (limit as u64).saturating_add(1),
        }))
    })
}

/// The default label predicates: those of C11 §4.4, then `skos:altLabel`.
pub(crate) fn default_labels() -> Vec<NamedNode> {
    render::LABEL_PREDICATES
        .iter()
        .chain(std::iter::once(&SKOS_ALT))
        .map(|p| iri(p))
        .collect()
}

struct Lookup<T> {
    values: HashMap<String, Vec<T>>,
    complete: HashMap<String, bool>,
}

fn completeness(
    iris: &[NamedNode],
    counts: Option<HashMap<String, usize>>,
    query_complete: bool,
) -> HashMap<String, bool> {
    counts.map_or_else(HashMap::new, |counts| {
        iris.iter()
            .map(|iri| {
                let count = counts.get(iri.as_str()).copied().unwrap_or(0);
                (iri.as_str().to_string(), query_complete && count <= 20)
            })
            .collect()
    })
}

/// At most 20 named `rdf:type`s of each IRI, in IRI order.
pub(crate) fn types_of(
    r: &Reader,
    iris: &[NamedNode],
    graphs: &[NamedNode],
) -> Result<HashMap<String, Vec<NamedNode>>, Error> {
    Ok(type_details(r, iris, graphs, false)?.values)
}

fn type_details(
    r: &Reader,
    iris: &[NamedNode],
    graphs: &[NamedNode],
    evidence: bool,
) -> Result<Lookup<NamedNode>, Error> {
    let mut out: HashMap<String, Vec<NamedNode>> = HashMap::new();
    if iris.is_empty() {
        return Ok(Lookup {
            values: out,
            complete: HashMap::new(),
        });
    }
    let limit = iris.len() * 20;
    let q = format!(
        "SELECT DISTINCT ?x ?t WHERE {{ {} {} FILTER(isIRI(?t)) }} ORDER BY ?x ?t LIMIT {limit}",
        values_iris("x", iris),
        r.quads("?x a ?t", graphs)
    );
    let rows = r.rows(&q, Vec::new())?;
    let mut query_complete = rows.len() < limit;
    let mut counts: Option<HashMap<String, usize>> = evidence.then(HashMap::new);
    for row in rows {
        if let [Some(Term::NamedNode(x)), Some(Term::NamedNode(t))] = row.as_slice() {
            if let Some(counts) = &mut counts {
                *counts.entry(x.as_str().to_string()).or_default() += 1;
            }
            let v = out.entry(x.as_str().to_string()).or_default();
            if v.len() < 20 {
                v.push(t.clone());
            }
        } else {
            query_complete = false;
        }
    }
    Ok(Lookup {
        values: out,
        complete: completeness(iris, counts, query_complete),
    })
}

/// The literals of `preds` of each IRI: `(predicate rank, literal)`, at most 20 per IRI.
pub(crate) fn literals_of(
    r: &Reader,
    iris: &[NamedNode],
    preds: &[NamedNode],
    graphs: &[NamedNode],
) -> Result<HashMap<String, Vec<(usize, Literal)>>, Error> {
    Ok(label_details(r, iris, preds, graphs, false)?.values)
}

fn label_details(
    r: &Reader,
    iris: &[NamedNode],
    preds: &[NamedNode],
    graphs: &[NamedNode],
    evidence: bool,
) -> Result<Lookup<(usize, Literal)>, Error> {
    let mut out: HashMap<String, Vec<(usize, Literal)>> = HashMap::new();
    if iris.is_empty() || preds.is_empty() {
        return Ok(Lookup {
            values: out,
            complete: HashMap::new(),
        });
    }
    let limit = iris.len() * 40;
    let q = format!(
        "SELECT DISTINCT ?x ?lp ?l WHERE {{ {} {} {} FILTER(isLITERAL(?l)) }} LIMIT {limit}",
        values_iris("x", iris),
        values_iris("lp", preds),
        r.quads("?x ?lp ?l", graphs)
    );
    let rows = r.rows(&q, Vec::new())?;
    let mut query_complete = rows.len() < limit;
    let mut counts: Option<HashMap<String, usize>> = evidence.then(HashMap::new);
    for row in rows {
        if let [
            Some(Term::NamedNode(x)),
            Some(Term::NamedNode(p)),
            Some(Term::Literal(l)),
        ] = row.as_slice()
            && let Some(rank) = preds.iter().position(|q| q == p)
        {
            if let Some(counts) = &mut counts {
                *counts.entry(x.as_str().to_string()).or_default() += 1;
            }
            let v = out.entry(x.as_str().to_string()).or_default();
            if v.len() < 20 {
                v.push((rank, l.clone()));
            }
        } else {
            query_complete = false;
        }
    }
    Ok(Lookup {
        values: out,
        complete: completeness(iris, counts, query_complete),
    })
}

/// The classes and their subclasses in the view.
fn subclass_details(r: &Reader, classes: &[NamedNode]) -> Result<(BTreeSet<String>, bool), Error> {
    let mut out: BTreeSet<String> = classes.iter().map(|c| c.as_str().to_string()).collect();
    let q = format!(
        "SELECT DISTINCT ?sub WHERE {{ {} {} }} LIMIT 10000",
        values_iris("c", classes),
        r.quads(&format!("?sub <{RDFS_SUBCLASS}>+ ?c"), &[])
    );
    let rows = r.rows(&q, Vec::new())?;
    let complete = rows.len() < 10000;
    for row in rows {
        if let Some(Some(Term::NamedNode(s))) = row.first() {
            out.insert(s.as_str().to_string());
        }
    }
    Ok((out, complete))
}

/// For each of `iris`, the others it is linked with by `owl:sameAs` or
/// `skos:exactMatch`, in either direction.
fn same_as(
    r: &Reader,
    iris: &[NamedNode],
    graphs: &[NamedNode],
) -> Result<HashMap<String, Vec<NamedNode>>, Error> {
    let mut out: HashMap<String, Vec<NamedNode>> = HashMap::new();
    if iris.len() < 2 {
        return Ok(out);
    }
    let q = format!(
        "SELECT DISTINCT ?a ?b WHERE {{ {} {} VALUES ?lp {{ <{OWL_SAME_AS}> <{SKOS_EXACT}> }} {} FILTER(?a != ?b) }} LIMIT 1000",
        values_iris("a", iris),
        values_iris("b", iris),
        r.quads("?a ?lp ?b", graphs)
    );
    for row in r.rows(&q, Vec::new())? {
        if let [Some(Term::NamedNode(a)), Some(Term::NamedNode(b))] = row.as_slice() {
            for (x, y) in [(a, b), (b, a)] {
                let v = out.entry(x.as_str().to_string()).or_default();
                if !v.contains(y) {
                    v.push(y.clone());
                }
            }
        }
    }
    Ok(out)
}

/// Up to `n` outgoing triples of `s`, sampled round-robin by predicate from its first
/// rows, without vectors. Its types and labels come last, since the candidate shows
/// them already.
fn sample(
    r: &Reader,
    s: &NamedNode,
    labels: &[NamedNode],
    graphs: &[NamedNode],
    n: usize,
) -> Result<Vec<(NamedNode, NamedNode, Term)>, Error> {
    let q = format!(
        "SELECT ?p ?o WHERE {{ {} }} LIMIT {SAMPLE_ROWS}",
        r.quads("?r ?p ?o", graphs)
    );
    // the other predicates first, then the types and labels the candidate shows anyway
    let mut by_pred: [BTreeMap<String, Vec<Term>>; 2] = Default::default();
    for row in r.rows(&q, vec![("r".into(), s.clone().into())])? {
        if let [Some(Term::NamedNode(p)), Some(o)] = row.as_slice()
            && !is_vector(o)
        {
            let known = usize::from(p.as_str() == RDF_TYPE || labels.contains(p));
            let v = by_pred[known].entry(p.as_str().to_string()).or_default();
            if !v.contains(o) {
                v.push(o.clone());
            }
        }
    }
    let mut out = Vec::new();
    for preds in &by_pred {
        let mut i = 0;
        while out.len() < n {
            let mut any = false;
            for (p, os) in preds {
                if let Some(o) = os.get(i) {
                    any = true;
                    if out.len() < n {
                        out.push((s.clone(), iri(p), o.clone()));
                    }
                }
            }
            if !any {
                break;
            }
            i += 1;
        }
    }
    Ok(out)
}

fn candidate_json(c: &Candidate, terms: &mut Terms, label_predicates: &[NamedNode]) -> Value {
    let refs: Vec<(usize, &Literal)> = c.labels.iter().map(|(r, l)| (*r, l)).collect();
    let label = render::choose_ranked(&refs, "en");
    let mut alt: Vec<String> = Vec::new();
    for (_, l) in &c.labels {
        let t = render::label_text(l.value());
        if Some(&t) != label.as_ref() && !alt.contains(&t) && alt.len() < 3 {
            alt.push(t);
        }
    }
    let mut matched = Vec::new();
    if c.exact {
        matched.push("exact");
    }
    if c.normalized {
        matched.push("normalized");
    }
    if c.text_rank.is_some() {
        matched.push("text");
    }
    if c.vector_rank.is_some() {
        matched.push("vector");
    }
    let mut j = json!({
        "iri": terms.iri(c.iri.as_str()),
        "types": c.types.iter().take(3).map(|t| terms.iri(t.as_str())).collect::<Vec<_>>(),
        "score": score_json(c.score),
        "typeMatch": c.type_match,
        "matchedBy": matched,
        "triples": c
            .triples
            .iter()
            .map(|(s, p, o)| format!("{} {} {}", terms.iri(s.as_str()), terms.iri(p.as_str()), terms.term(o)))
            .collect::<Vec<_>>(),
    });
    if let Some(l) = label {
        j["label"] = l.into();
    }
    if !alt.is_empty() {
        j["altLabels"] = alt.into();
    }
    if !c.same_as.is_empty() {
        j["sameAs"] = c
            .same_as
            .iter()
            .map(|s| terms.iri(s.as_str()))
            .collect::<Vec<_>>()
            .into();
    }
    if let Some(evidence) = &c.evidence {
        let labels: Vec<Value> = c
            .labels
            .iter()
            .filter(|(_, literal)| !sparkles::vector::is_datatype(literal.datatype().as_str()))
            .map(|(rank, literal)| {
                json!({
                    "predicate": label_predicates[*rank].as_str(),
                    "term": literal.to_string(),
                })
            })
            .collect();
        j["evidence"] = json!({
            "version": 1,
            "iri": c.iri.as_str(),
            "types": c.types.iter().map(NamedNode::as_str).collect::<Vec<_>>(),
            "typesComplete": evidence.types_complete,
            "labels": labels,
            "labelsComplete": evidence.labels_complete,
        });
    }
    j
}
