//! The OLS4 legacy REST wire format over the same authorized catalog term index.
//! See https://www.ebi.ac.uk/ols4/ols3help.

use super::http::json_response;
use super::publication::{Field, Read, Term, Terms};
use super::search::{self, Entry, Mode, Request, Window, number, selectors};
use super::{bad, not_found};
use crate::http::{ApiResult, Params};
use crate::resources::{CatalogFile, Resource};
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};

const API: &str = "/ols4/api";
const MAX_HIERARCHY_TERMS: usize = 10_000;
const MAX_HIERARCHY_EDGES: usize = 100_000;

pub(super) fn search_response(
    read: &mut Read<'_>,
    catalog: &CatalogFile,
    params: &Params,
    suggest: bool,
) -> ApiResult {
    let query = params.get("q").ok_or_else(|| bad("missing q"))?;
    search::validate_query(query)?;
    let mode = if suggest {
        Mode::Prefix
    } else if params.get("exact") == Some("true") {
        Mode::Exact
    } else {
        Mode::Lexical
    };
    let mut fields = Vec::new();
    for field in selectors(params, "queryFields") {
        let field = match field {
            "label" => Field::Label,
            "synonym" | "synonyms" => Field::Synonym,
            "description" | "definition" => Field::Definition,
            "iri" | "short_form" | "obo_id" => Field::Identifier,
            _ => return Err(bad("unsupported queryFields value")),
        };
        fields.push(field);
    }
    if fields.is_empty() {
        fields = if suggest {
            vec![Field::Label, Field::Synonym]
        } else {
            search::FIELDS.to_vec()
        };
    }
    let request = Request {
        query,
        window: Window::new(number(params, "start", 0)?, number(params, "rows", 10)?)?,
        ontologies: selectors(params, "ontology"),
        lang: params.get("lang"),
        mode,
        fields,
        types: selectors(params, "type"),
        obsolete: params.get("obsoletes") == Some("true"),
    };
    let scopes = search::corpus(read, catalog, |r| {
        request.ontologies.is_empty() || request.ontologies.contains(&r.id.as_str())
    })?;
    let results = search::match_terms(read, &scopes, &request)?;
    let mut docs = Vec::with_capacity(results.hits.len());
    for (entry, matched) in results.hits {
        read.check()?;
        if suggest {
            read.charge.charge(256 + matched.len() as u64)?;
            docs.push(json!({"autosuggest": matched}));
        } else {
            admit_entry(read, entry)?;
            docs.push(json!({
                "iri": entry.iri,
                "label": entry.label(request.lang),
                "ontology_name": entry.resource.id,
                "ontology_prefix": prefix(entry.resource),
                "short_form": entry.short_form(),
                "obo_id": obo_id(entry),
                "type": entry.kind(),
                "description": entry.literals(Field::Definition).collect::<Vec<_>>(),
                "related_synonyms": entry.literals(Field::Synonym).collect::<Vec<_>>(),
            }));
        }
    }
    let mut body = json!({
        "responseHeader": {"status": 0, "QTime": 0},
        "response": {"numFound": results.total, "start": request.window.start, "docs": docs},
    });
    if !suggest {
        let facets: Vec<_> = results
            .facets
            .iter()
            .flat_map(|(id, count)| [id.to_string(), count.to_string()])
            .collect();
        body["facet_counts"] = json!({"facet_fields": {"ontologyId": facets}});
    }
    json_response(read, &body)
}

fn prefix(resource: &Resource) -> &str {
    resource
        .input
        .namespaces
        .first()
        .map_or(&resource.id, |ns| &ns.prefix)
}

fn obo_id(entry: Entry<'_>) -> String {
    let short = entry.short_form();
    match short.split_once('_') {
        Some((prefix, local)) if local.bytes().all(|byte| byte.is_ascii_digit()) => {
            format!("{prefix}:{local}")
        }
        _ => short.to_owned(),
    }
}

fn admit_entry(read: &mut Read<'_>, entry: Entry<'_>) -> ApiResult<()> {
    read.charge.charge(2048 + (entry.iri.len() as u64) * 32)?;
    for fact in &entry.term.facts {
        read.check()?;
        read.charge.charge(192 + (fact.value.len() as u64) * 4)?;
    }
    Ok(())
}

fn link(read: &Read<'_>, path: &str) -> String {
    crate::auth::public_url(read.st).map_or_else(
        || path.to_owned(),
        |base| format!("{}{path}", base.trim_end_matches('/')),
    )
}

fn ontology_path(resource: &Resource) -> String {
    format!("{API}/ontologies/{}", resource.id)
}

fn encode_iri(iri: &str) -> String {
    use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
    let encoded = utf8_percent_encode(iri, NON_ALPHANUMERIC).to_string();
    utf8_percent_encode(&encoded, NON_ALPHANUMERIC).to_string()
}

pub(super) fn decode_iri(iri: &str) -> ApiResult<String> {
    let iri = percent_encoding::percent_decode_str(iri)
        .decode_utf8()
        .map_err(|_| bad("iri must be UTF-8"))?;
    if !super::valid_lookup_iri(&iri) {
        return Err(bad("iri must be a valid absolute IRI"));
    }
    Ok(iri.into_owned())
}

fn term_path(entry: Entry<'_>) -> String {
    format!(
        "{}/terms/{}",
        ontology_path(entry.resource),
        encode_iri(entry.iri)
    )
}

fn ontology_document(
    read: &mut Read<'_>,
    resource: &Resource,
    terms: &Terms,
    lang: Option<&str>,
) -> ApiResult<Value> {
    let mut counts = [0usize; 3];
    for term in terms.entries.values() {
        read.check()?;
        match search::kind(term) {
            "class" => counts[0] += 1,
            "property" => counts[1] += 1,
            "individual" => counts[2] += 1,
            _ => {}
        }
    }
    let iri = terms.ontology_iri.as_deref();
    let declaration = iri.and_then(|iri| terms.entries.get(iri));
    let definition = declaration.and_then(|term| {
        term.values(Field::Definition)
            .find(|fact| !fact.iri)
            .map(|fact| fact.value.as_str())
    });
    let version = declaration.and_then(|term| {
        term.values(Field::Metadata)
            .find(|fact| fact.predicate == "http://www.w3.org/2002/07/owl#versionInfo" && !fact.iri)
            .map(|fact| fact.value.as_str())
    });
    let base = ontology_path(resource);
    read.charge.charge(
        2048 + resource.input.title.len() as u64
            + definition.map_or(0, str::len) as u64
            + version.map_or(0, str::len) as u64,
    )?;
    Ok(json!({
        "ontologyId": resource.id,
        "lang": lang.unwrap_or("en"),
        "status": "LOADED",
        "version": version,
        "numberOfTerms": counts[0],
        "numberOfProperties": counts[1],
        "numberOfIndividuals": counts[2],
        "config": {
            "id": resource.id,
            "title": resource.input.title,
            "description": definition,
            "preferredPrefix": prefix(resource),
            "namespace": resource.id,
            "version": version,
        },
        "_links": {
            "self": {"href": link(read, &base)},
            "terms": {"href": link(read, &format!("{base}/terms"))},
        },
    }))
}

pub(super) fn ontology_response(
    read: &mut Read<'_>,
    catalog: &CatalogFile,
    id: &str,
    params: &Params,
) -> ApiResult {
    let (resource, terms) = readable_resource(read, catalog, id)?;
    let body = ontology_document(read, resource, &terms, params.get("lang"))?;
    json_response(read, &body)
}

pub(super) fn ontology_list(
    read: &mut Read<'_>,
    catalog: &CatalogFile,
    params: &Params,
) -> ApiResult {
    let (number, window) = page(params)?;
    let scopes = search::corpus(read, catalog, |_| true)?;
    let mut documents = Vec::new();
    for &(resource, ref terms) in scopes.values().skip(window.start).take(window.size) {
        documents.push(ontology_document(
            read,
            resource,
            terms,
            params.get("lang"),
        )?);
    }
    let body = hal(
        read,
        &format!("{API}/ontologies"),
        "ontologies",
        documents,
        number,
        window,
        scopes.len(),
        params,
    );
    json_response(read, &body)
}

fn readable_resource<'a>(
    read: &mut Read<'_>,
    catalog: &'a CatalogFile,
    id: &str,
) -> ApiResult<(&'a Resource, Terms)> {
    // A hidden resource may take a query to prove unreadable. The 404 body is
    // identical to that for a catalog miss without a constant-time guarantee.
    let resource = crate::resources::find(catalog, id).ok_or_else(not_found)?;
    let terms = read.resource_terms(resource)?;
    terms
        .visible
        .then_some((resource, terms))
        .ok_or_else(not_found)
}

fn page(params: &Params) -> ApiResult<(usize, Window)> {
    let page = number(params, "page", 0)?;
    let size = number(params, "size", 20)?;
    let start = page
        .checked_mul(size)
        .ok_or_else(|| bad("page window is too large"))?;
    Ok((page, Window::new(start, size)?))
}

#[allow(clippy::too_many_arguments)]
fn hal(
    read: &Read<'_>,
    base: &str,
    member: &str,
    docs: Vec<Value>,
    number: usize,
    window: Window,
    total: usize,
    params: &Params,
) -> Value {
    let total_pages = total.div_ceil(window.size);
    let page_url = |number: usize| {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("page", &number.to_string())
            .append_pair("size", &window.size.to_string());
        for key in ["iri", "lang", "obo_id", "short_form"] {
            if let Some(value) = params.get(key) {
                query.append_pair(key, value);
            }
        }
        json!({"href": link(read, &format!("{base}?{}", query.finish()))})
    };
    let mut links = json!({"self": page_url(number), "first": page_url(0)});
    if total_pages <= 10_000 / window.size {
        links["last"] = page_url(total_pages.saturating_sub(1));
    }
    if number > 0 {
        links["prev"] = page_url(number - 1);
    }
    if number + 1 < total_pages && window.end() + window.size <= 10_000 {
        links["next"] = page_url(number + 1);
    }
    json!({
        "_embedded": { (member): docs },
        "_links": links,
        "page": {"number": number, "size": window.size, "totalElements": total, "totalPages": total_pages},
    })
}

fn term_document(
    read: &mut Read<'_>,
    entry: Entry<'_>,
    terms: &Terms,
    lang: Option<&str>,
) -> ApiResult<Value> {
    admit_entry(read, entry)?;
    let base = term_path(entry);
    let children = terms.children(entry.iri).next().is_some();
    let ontology_iri = terms.ontology_iri.as_deref();
    let defining = entry
        .resource
        .input
        .namespaces
        .iter()
        .any(|ns| entry.iri.starts_with(&ns.iri));
    Ok(json!({
        "iri": entry.iri,
        "lang": lang.unwrap_or("en"),
        "label": entry.label(lang),
        "description": entry.literals(Field::Definition).collect::<Vec<_>>(),
        "synonyms": entry.literals(Field::Synonym).collect::<Vec<_>>(),
        "annotation": {},
        "ontology_name": entry.resource.id,
        "ontology_prefix": prefix(entry.resource),
        "ontology_iri": ontology_iri,
        "is_defining_ontology": defining,
        "is_obsolete": entry.term.obsolete(),
        "is_root": parents(entry.term).next().is_none(),
        "has_children": children,
        "short_form": entry.short_form(),
        "obo_id": obo_id(entry),
        "_links": {
            "self": {"href": link(read, &base)},
            "parents": {"href": link(read, &format!("{base}/parents"))},
            "children": {"href": link(read, &format!("{base}/children"))},
            "ancestors": {"href": link(read, &format!("{base}/ancestors"))},
            "descendants": {"href": link(read, &format!("{base}/descendants"))},
        },
    }))
}

pub(super) fn term_list(
    read: &mut Read<'_>,
    catalog: &CatalogFile,
    id: &str,
    params: &Params,
) -> ApiResult {
    let (number, window) = page(params)?;
    let (resource, terms) = readable_resource(read, catalog, id)?;
    let mut documents = Vec::new();
    let mut total = 0;
    for (iri, term) in &terms.entries {
        read.check()?;
        let entry = Entry {
            resource,
            iri,
            term,
        };
        if (params.get("iri").is_none() && entry.kind() != "class")
            || params.get("iri").is_some_and(|filter| filter != iri)
            || params
                .get("short_form")
                .is_some_and(|filter| filter != entry.short_form())
            || params
                .get("obo_id")
                .is_some_and(|filter| filter != obo_id(entry))
        {
            continue;
        }
        if (window.start..window.end()).contains(&total) {
            documents.push(term_document(read, entry, &terms, params.get("lang"))?);
        }
        total += 1;
    }
    let body = hal(
        read,
        &format!("{}/terms", ontology_path(resource)),
        "terms",
        documents,
        number,
        window,
        total,
        params,
    );
    json_response(read, &body)
}

pub(super) fn term_response(
    read: &mut Read<'_>,
    catalog: &CatalogFile,
    id: &str,
    iri: &str,
    params: &Params,
) -> ApiResult {
    let (resource, terms) = readable_resource(read, catalog, id)?;
    let term = terms.entries.get(iri).ok_or_else(not_found)?;
    let entry = Entry {
        resource,
        iri,
        term,
    };
    let document = term_document(read, entry, &terms, params.get("lang"))?;
    json_response(read, &document)
}

fn parents(term: &Term) -> impl Iterator<Item = &str> {
    term.values(Field::Parent)
        .filter(|fact| fact.iri)
        .map(|fact| fact.value.as_str())
}

pub(super) enum Relation {
    Parents,
    Children,
    Ancestors,
    Descendants,
}

impl Relation {
    pub(super) fn parse(name: &str) -> ApiResult<Self> {
        match name {
            "parents" => Ok(Self::Parents),
            "children" => Ok(Self::Children),
            "ancestors" => Ok(Self::Ancestors),
            "descendants" => Ok(Self::Descendants),
            _ => Err(not_found()),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Parents => "parents",
            Self::Children => "children",
            Self::Ancestors => "ancestors",
            Self::Descendants => "descendants",
        }
    }
}

pub(super) fn related_terms(
    read: &mut Read<'_>,
    catalog: &CatalogFile,
    id: &str,
    iri: &str,
    relation: Relation,
    params: &Params,
) -> ApiResult {
    let (number, window) = page(params)?;
    let (resource, terms) = readable_resource(read, catalog, id)?;
    let term = terms.entries.get(iri).ok_or_else(not_found)?;
    let root = Entry {
        resource,
        iri,
        term,
    };
    let seen = hierarchy(read, &terms, iri, &relation, MAX_HIERARCHY_TERMS)?;
    let mut documents = Vec::new();
    for &iri in seen.iter().skip(window.start).take(window.size) {
        let entry = Entry {
            resource,
            iri,
            term: &terms.entries[iri],
        };
        documents.push(term_document(read, entry, &terms, params.get("lang"))?);
    }
    let body = hal(
        read,
        &format!("{}/{}", term_path(root), relation.name()),
        "terms",
        documents,
        number,
        window,
        seen.len(),
        params,
    );
    json_response(read, &body)
}

fn hierarchy<'a>(
    read: &mut Read<'_>,
    terms: &'a Terms,
    iri: &'a str,
    relation: &Relation,
    max_terms: usize,
) -> sparkles::Result<BTreeSet<&'a str>> {
    let up = matches!(relation, Relation::Parents | Relation::Ancestors);
    let transitive = matches!(relation, Relation::Ancestors | Relation::Descendants);
    let mut seen = BTreeSet::from([iri]);
    let mut queue = VecDeque::from([iri]);
    let mut edges_seen = 0;
    while let Some(current) = queue.pop_front() {
        read.check()?;
        let mut visit = |next: &'a str| -> sparkles::Result<()> {
            read.check()?;
            edges_seen += 1;
            if edges_seen > MAX_HIERARCHY_EDGES {
                return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                    kind: sparkles::BudgetKind::RowsProduced,
                    limit: MAX_HIERARCHY_EDGES as u64,
                    requested: edges_seen as u64,
                }));
            }
            if !terms.entries.contains_key(next) || seen.contains(next) {
                return Ok(());
            }
            // The root takes one slot in seen and is excluded from the result.
            if seen.len() > max_terms {
                return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                    kind: sparkles::BudgetKind::Rows,
                    limit: max_terms as u64,
                    requested: seen.len() as u64,
                }));
            }
            read.charge.charge(64)?;
            seen.insert(next);
            if transitive {
                queue.push_back(next);
            }
            Ok(())
        };
        if up {
            for parent in parents(&terms.entries[current]) {
                visit(parent)?;
            }
        } else {
            for child in terms.children(current) {
                visit(child)?;
            }
        }
    }
    seen.remove(iri);
    Ok(seen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Principal;
    use crate::state::{AppState, DbType};
    use sparkles::store::StoreOptions;
    use std::sync::{Arc, atomic::AtomicBool};
    use std::time::Duration;

    #[test]
    fn hierarchy_excludes_cycles_and_refuses_over_cap() {
        let dir = tempfile::tempdir().unwrap();
        let st =
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(10)).unwrap();
        let ds = st.attach("ont", DbType::Mem, None).unwrap();
        ds.dataset
            .update(
                "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {
                <urn:a> rdfs:subClassOf <urn:b> .
                <urn:b> rdfs:subClassOf <urn:c> .
                <urn:c> rdfs:subClassOf <urn:a> .
             }",
            )
            .unwrap();
        let mut read = Read::new(
            &st,
            &Principal::local(),
            None,
            Arc::new(AtomicBool::new(false)),
        );
        let terms = read.dataset_terms("ont").unwrap_or_else(|error| {
            panic!(
                "ontology read failed: {}",
                axum::response::IntoResponse::into_response(error).status()
            )
        });
        let error = hierarchy(&mut read, &terms, "urn:a", &Relation::Ancestors, 1).unwrap_err();
        assert!(matches!(
            error,
            sparkles::Error::BudgetExceeded(sparkles::Budget {
                kind: sparkles::BudgetKind::Rows,
                limit: 1,
                requested: 2,
            })
        ));
        let result = hierarchy(&mut read, &terms, "urn:a", &Relation::Ancestors, 2).unwrap();
        assert_eq!(result, BTreeSet::from(["urn:b", "urn:c"]));
    }
}
