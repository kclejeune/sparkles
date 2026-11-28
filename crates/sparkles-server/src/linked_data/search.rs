//! Catalog-wide ranking over caller-visible terms. Lexical and sparse trigram vectors
//! share the resource/term key and are projected and scored one field value at a time.

use super::bad;
use super::publication::{Field, Read, Term, Terms, local_name};
use crate::http::{ApiResult, Params};
use crate::resources::{CatalogFile, Resource};
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};

const MAX_QUERY_BYTES: usize = 1000;

#[derive(Clone, Copy)]
pub(super) struct Window {
    pub(super) start: usize,
    pub(super) size: usize,
}

impl Window {
    pub(super) fn new(start: usize, size: usize) -> ApiResult<Self> {
        if size == 0 || size > 100 || start.checked_add(size).is_none_or(|end| end > 10_000) {
            return Err(bad(
                "page size must be 1 to 100 and the window must end by 10000",
            ));
        }
        Ok(Self { start, size })
    }

    pub(super) fn end(self) -> usize {
        self.start + self.size
    }
}

pub(super) fn number(params: &Params, key: &str, default: usize) -> ApiResult<usize> {
    params.get(key).map_or(Ok(default), |value| {
        value
            .parse()
            .map_err(|_| bad(format!("{key} must be a non-negative integer")))
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Hybrid,
    Lexical,
    Vector,
    Prefix,
    Exact,
}

pub(super) const FIELDS: &[Field] = &[
    Field::Label,
    Field::Synonym,
    Field::Definition,
    Field::Identifier,
];

pub(super) struct Request<'a> {
    pub(super) query: &'a str,
    pub(super) window: Window,
    pub(super) ontologies: Vec<&'a str>,
    pub(super) lang: Option<&'a str>,
    pub(super) mode: Mode,
    pub(super) fields: Vec<Field>,
    pub(super) types: Vec<&'a str>,
    pub(super) obsolete: bool,
}

impl<'a> Request<'a> {
    pub(super) fn native(params: &'a Params) -> ApiResult<Self> {
        let mode = match params.get("mode").unwrap_or("hybrid") {
            "hybrid" => Mode::Hybrid,
            "lexical" => Mode::Lexical,
            "vector" => Mode::Vector,
            "prefix" => Mode::Prefix,
            _ => return Err(bad("mode must be hybrid, lexical, vector or prefix")),
        };
        Ok(Self {
            query: params.get("q").ok_or_else(|| bad("missing q"))?,
            window: Window::new(number(params, "offset", 0)?, number(params, "limit", 20)?)?,
            ontologies: selectors(params, "ontology"),
            lang: params.get("lang"),
            mode,
            fields: FIELDS.to_vec(),
            types: Vec::new(),
            obsolete: true,
        })
    }

    fn allows(&self, resource: &Resource) -> bool {
        self.ontologies.is_empty() || self.ontologies.contains(&resource.id.as_str())
    }
}

pub(super) fn selectors<'a>(params: &'a Params, name: &str) -> Vec<&'a str> {
    params
        .0
        .iter()
        .filter(|(key, _)| key == name)
        .flat_map(|(_, value)| value.split(','))
        .filter(|value| !value.is_empty())
        .collect()
}

pub(super) fn corpus<'a>(
    read: &mut Read<'_>,
    catalog: &'a CatalogFile,
    include: impl Fn(&Resource) -> bool,
) -> ApiResult<BTreeMap<&'a str, (&'a Resource, Terms)>> {
    let mut scopes = BTreeMap::new();
    for resource in &catalog.resources {
        read.check()?;
        if !include(resource) {
            continue;
        }
        let terms = read.resource_terms(resource)?;
        if terms.visible {
            read.charge.charge(256)?;
            scopes.insert(resource.id.as_str(), (resource, terms));
        }
    }
    Ok(scopes)
}

#[derive(Clone, Copy)]
pub(super) struct Entry<'a> {
    pub(super) resource: &'a Resource,
    pub(super) iri: &'a str,
    pub(super) term: &'a Term,
}

impl<'a> Entry<'a> {
    pub(super) fn label(&self, lang: Option<&str>) -> &'a str {
        self.term.label(self.iri, lang)
    }

    pub(super) fn literals(&self, field: Field) -> impl Iterator<Item = &'a str> {
        self.term
            .values(field)
            .filter(|fact| !fact.iri)
            .map(|fact| fact.value.as_str())
    }

    pub(super) fn short_form(&self) -> &'a str {
        self.literals(Field::Identifier)
            .next()
            .unwrap_or_else(|| local_name(self.iri))
    }

    pub(super) fn kind(&self) -> &'static str {
        kind(self.term)
    }
}

pub(super) fn kind(term: &Term) -> &'static str {
    if term.is_ontology() {
        return "ontology";
    }
    let mut inferred = "individual";
    for fact in term.values(Field::Type).filter(|fact| fact.iri) {
        match fact.value.as_str() {
            "http://www.w3.org/2002/07/owl#Class"
            | "http://www.w3.org/2000/01/rdf-schema#Class"
            | "http://www.w3.org/2004/02/skos/core#Concept" => return "class",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#Property"
            | "http://www.w3.org/2002/07/owl#ObjectProperty"
            | "http://www.w3.org/2002/07/owl#DatatypeProperty"
            | "http://www.w3.org/2002/07/owl#AnnotationProperty"
            | "http://www.w3.org/2002/07/owl#FunctionalProperty"
            | "http://www.w3.org/2002/07/owl#TransitiveProperty"
            | "http://www.w3.org/2002/07/owl#SymmetricProperty"
            | "http://www.w3.org/2002/07/owl#InverseFunctionalProperty"
            | "http://www.w3.org/2002/07/owl#AsymmetricProperty"
            | "http://www.w3.org/2002/07/owl#ReflexiveProperty"
            | "http://www.w3.org/2002/07/owl#IrreflexiveProperty" => inferred = "property",
            _ => {}
        }
    }
    if inferred == "individual" {
        for fact in term.values(Field::Parent).filter(|fact| fact.iri) {
            match fact.predicate {
                "http://www.w3.org/2000/01/rdf-schema#subPropertyOf" => return "property",
                _ => inferred = "class",
            }
        }
    }
    inferred
}

#[derive(Clone, Copy)]
struct Rank {
    tier: u8,
    field: u8,
    similarity: f64,
}

impl Rank {
    fn compare(self, other: Self) -> Ordering {
        self.tier
            .cmp(&other.tier)
            .then(self.field.cmp(&other.field))
            .then_with(|| other.similarity.total_cmp(&self.similarity))
    }
}

#[derive(Clone, Copy)]
struct Ranked<'a> {
    rank: Rank,
    entry: Entry<'a>,
    matched: &'a str,
}

impl Ord for Ranked<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank
            .compare(other.rank)
            .then(self.entry.resource.id.cmp(&other.entry.resource.id))
            .then(self.entry.iri.cmp(other.entry.iri))
    }
}
impl PartialOrd for Ranked<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Ranked<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for Ranked<'_> {}

pub(super) struct Results<'a> {
    pub(super) total: usize,
    pub(super) hits: Vec<(Entry<'a>, &'a str)>,
    pub(super) facets: BTreeMap<&'a str, usize>,
}

pub(super) fn validate_query(query: &str) -> ApiResult<()> {
    if query.len() > MAX_QUERY_BYTES || !query.chars().any(char::is_alphanumeric) {
        return Err(bad(format!(
            "q must contain a letter or number and be at most {MAX_QUERY_BYTES} bytes"
        )));
    }
    Ok(())
}

pub(super) fn match_terms<'a>(
    read: &mut Read<'_>,
    scopes: &'a BTreeMap<&str, (&Resource, Terms)>,
    request: &Request<'_>,
) -> ApiResult<Results<'a>> {
    let end = request.window.end();
    // A bounded heap retains only the page's predecessors and the page itself.
    read.charge.charge((end as u64) * 128)?;
    let mut heap = BinaryHeap::with_capacity(end);
    let mut facets = BTreeMap::new();
    let mut total = 0;
    let query = normalize(request.query);
    let mut words: Vec<_> = query.split_whitespace().collect();
    words.sort_unstable();
    words.dedup();
    let features = trigrams(&query);
    for &(resource, ref terms) in scopes.values() {
        if !request.allows(resource) {
            continue;
        }
        let mut count = 0;
        for (iri, term) in &terms.entries {
            read.check()?;
            let entry = Entry {
                resource,
                iri,
                term,
            };
            if (!request.obsolete && term.obsolete())
                || (!request.types.is_empty() && !request.types.contains(&entry.kind()))
            {
                continue;
            }
            let candidates = term.facts.iter().filter_map(|fact| {
                (!fact.iri
                    && request.fields.contains(&fact.field)
                    && (request.mode != Mode::Prefix || fact.field != Field::Definition))
                    .then_some((fact.value.as_str(), fact.field as u8))
            });
            let identifiers = [
                (iri.as_str(), Field::Identifier as u8),
                (local_name(iri), Field::Identifier as u8),
            ];
            let candidates = candidates.chain(
                identifiers
                    .into_iter()
                    .filter(|_| request.fields.contains(&Field::Identifier)),
            );
            let mut best: Option<(Rank, &str)> = None;
            for (value, field) in candidates {
                read.check()?;
                // This temporary working set holds normalized Unicode and at
                // most one trigram per character, then drops before the next value.
                let bytes = (value.len() as u64).saturating_mul(32).saturating_add(256);
                read.charge.charge(bytes)?;
                let rank = score(&query, &words, &features, value, field, request.mode, read);
                read.charge.release(bytes);
                if let Some(rank) = rank?
                    && best.is_none_or(|(previous, _)| rank.compare(previous).is_lt())
                {
                    best = Some((rank, value));
                }
            }
            if let Some((rank, matched)) = best {
                count += 1;
                let candidate = Ranked {
                    rank,
                    entry,
                    matched,
                };
                if heap.len() < end {
                    heap.push(candidate);
                } else if heap.peek().is_some_and(|last| candidate < *last) {
                    *heap.peek_mut().expect("nonempty heap") = candidate;
                }
            }
        }
        if count > 0 {
            total += count;
            facets.insert(resource.id.as_str(), count);
        }
    }
    let hits = heap
        .into_sorted_vec()
        .into_iter()
        .skip(request.window.start)
        .map(|row| (row.entry, row.matched))
        .collect();
    Ok(Results {
        total,
        hits,
        facets,
    })
}

fn normalize(value: &str) -> String {
    let mut normalized = String::new();
    let mut space = false;
    for ch in value.chars() {
        if ch.is_alphanumeric() {
            if space && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.extend(ch.to_lowercase().filter(|ch| ch.is_alphanumeric()));
            space = false;
        } else {
            space = true;
        }
    }
    normalized
}

fn trigrams(text: &str) -> Vec<[char; 3]> {
    let mut result = Vec::new();
    let (mut first, mut second) = ('\0', '\0');
    for third in text.chars().chain(['\0']) {
        result.push([first, second, third]);
        (first, second) = (second, third);
    }
    result.sort_unstable();
    result.dedup();
    result
}

fn score(
    query: &str,
    words: &[&str],
    features: &[[char; 3]],
    value: &str,
    field: u8,
    mode: Mode,
    read: &Read<'_>,
) -> sparkles::Result<Option<Rank>> {
    let text = normalize(value);
    if mode != Mode::Vector {
        if text == query {
            return Ok(Some(Rank {
                tier: 0,
                field,
                similarity: 1.0,
            }));
        }
        if mode == Mode::Exact {
            return Ok(None);
        }
        if text.starts_with(query) {
            return Ok(Some(Rank {
                tier: 1,
                field,
                similarity: 1.0,
            }));
        }
        if mode == Mode::Prefix {
            return Ok(None);
        }
        let mut matched = [false; MAX_QUERY_BYTES];
        let mut remaining = words.len();
        for (index, word) in text.split_whitespace().enumerate() {
            if index.is_multiple_of(1024) {
                read.check()?;
            }
            if let Ok(pos) = words.binary_search(&word)
                && !matched[pos]
            {
                matched[pos] = true;
                remaining -= 1;
                if remaining == 0 {
                    break;
                }
            }
        }
        if remaining == 0 {
            return Ok(Some(Rank {
                tier: 2,
                field,
                similarity: 1.0,
            }));
        }
        if mode == Mode::Lexical {
            return Ok(None);
        }
    }
    // Cosine of binary character trigram vectors. The score has no corpus-wide
    // term frequencies, so inaccessible rows cannot influence it.
    if features.len() < 4 {
        return Ok(None);
    }
    read.check()?;
    let other = trigrams(&text);
    let common = features
        .iter()
        .filter(|key| other.binary_search(key).is_ok())
        .count();
    let similarity = common as f64 / ((features.len() as f64) * (other.len() as f64)).sqrt();
    Ok((similarity >= 0.38).then_some(Rank {
        tier: 3,
        field: 0,
        similarity,
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Hit<'a> {
    iri: &'a str,
    ontology: &'a str,
    label: &'a str,
    kind: &'static str,
    documentation: String,
}

pub(super) fn response(
    read: &mut Read<'_>,
    catalog: &CatalogFile,
    request: &Request<'_>,
) -> ApiResult<axum::response::Response> {
    validate_query(request.query)?;
    let scopes = corpus(read, catalog, |resource| request.allows(resource))?;
    let results = match_terms(read, &scopes, request)?;
    let mut hits = Vec::with_capacity(results.hits.len());
    for (entry, _) in results.hits {
        read.check()?;
        read.charge.charge(256 + (entry.iri.len() as u64) * 3)?;
        hits.push(Hit {
            iri: entry.iri,
            label: entry.label(request.lang),
            ontology: &entry.resource.id,
            kind: entry.kind(),
            documentation: format!(
                "/$/ld/resources/{}/doc?iri={}",
                entry.resource.id,
                super::html::percent_encode_param(entry.iri)
            ),
        });
    }
    #[derive(Serialize)]
    struct Body<'a> {
        total: usize,
        hits: Vec<Hit<'a>>,
        facets: BTreeMap<&'a str, usize>,
    }
    super::http::json_response(
        read,
        &Body {
            total: results.total,
            hits,
            facets: results.facets,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_unicode_words_and_extracts_unique_boundary_trigrams() {
        assert_eq!(normalize("  CarDİAC / Muscle! "), "cardiac muscle");
        assert_eq!(
            trigrams("aba"),
            vec![
                ['\0', '\0', 'a'],
                ['\0', 'a', 'b'],
                ['a', 'b', 'a'],
                ['b', 'a', '\0']
            ]
        );
    }
}
