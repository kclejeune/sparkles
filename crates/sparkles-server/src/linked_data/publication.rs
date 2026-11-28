//! Read asserted catalog and dataset facts through the SPARQL authorization boundary.
//! Every retained record belongs to one request. In particular the index, facets and
//! document links are derived after graph selection and triple masking.

use super::{Deadline, MemoryCharge, not_found};
use crate::auth::{Endpoint, Level, Principal};
use crate::http::caller::CallerContext;
use crate::http::{ApiResult, err_code};
use crate::resources::{CatalogFile, Kind, Resource};
use crate::state::{AppState, Dataset};
use axum::http::StatusCode;
use oxrdf::TermRef;
use sparkles::store::Snapshot;
use sparkles::task::Control;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

pub(super) const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
pub(super) const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
pub(super) const PREF_LABEL: &str = "http://www.w3.org/2004/02/skos/core#prefLabel";
pub(super) const TITLE: &str = "http://purl.org/dc/terms/title";
pub(super) const ONTOLOGY: &str = "http://www.w3.org/2002/07/owl#Ontology";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Field {
    Label,
    Synonym,
    Definition,
    Example,
    Identifier,
    Parent,
    Equivalent,
    Usage,
    Type,
    Metadata,
}

const PREDICATES: &[(&str, Field)] = &[
    (RDF_TYPE, Field::Type),
    (RDFS_LABEL, Field::Label),
    (PREF_LABEL, Field::Label),
    (TITLE, Field::Label),
    (
        "http://www.w3.org/2004/02/skos/core#altLabel",
        Field::Synonym,
    ),
    (
        "http://www.w3.org/2004/02/skos/core#hiddenLabel",
        Field::Synonym,
    ),
    (
        "http://www.geneontology.org/formats/oboInOwl#hasExactSynonym",
        Field::Synonym,
    ),
    (
        "http://www.geneontology.org/formats/oboInOwl#hasBroadSynonym",
        Field::Synonym,
    ),
    (
        "http://www.geneontology.org/formats/oboInOwl#hasNarrowSynonym",
        Field::Synonym,
    ),
    (
        "http://www.geneontology.org/formats/oboInOwl#hasRelatedSynonym",
        Field::Synonym,
    ),
    (
        "http://www.w3.org/2004/02/skos/core#definition",
        Field::Definition,
    ),
    (
        "http://purl.obolibrary.org/obo/IAO_0000115",
        Field::Definition,
    ),
    (
        "http://www.w3.org/2000/01/rdf-schema#comment",
        Field::Definition,
    ),
    ("http://purl.org/dc/terms/description", Field::Definition),
    (
        "http://www.w3.org/2004/02/skos/core#example",
        Field::Example,
    ),
    ("http://purl.obolibrary.org/obo/IAO_0000112", Field::Example),
    (
        "http://www.w3.org/2004/02/skos/core#scopeNote",
        Field::Usage,
    ),
    ("http://www.w3.org/2000/01/rdf-schema#domain", Field::Usage),
    ("http://www.w3.org/2000/01/rdf-schema#range", Field::Usage),
    ("http://www.w3.org/2000/01/rdf-schema#seeAlso", Field::Usage),
    (
        "http://www.w3.org/2004/02/skos/core#notation",
        Field::Identifier,
    ),
    (
        "http://www.geneontology.org/formats/oboInOwl#id",
        Field::Identifier,
    ),
    (
        "http://www.w3.org/2000/01/rdf-schema#subClassOf",
        Field::Parent,
    ),
    (
        "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
        Field::Parent,
    ),
    ("http://www.w3.org/2004/02/skos/core#broader", Field::Parent),
    (
        "http://www.w3.org/2002/07/owl#equivalentClass",
        Field::Equivalent,
    ),
    (
        "http://www.w3.org/2002/07/owl#equivalentProperty",
        Field::Equivalent,
    ),
    ("http://www.w3.org/2002/07/owl#sameAs", Field::Equivalent),
    (
        "http://www.w3.org/2004/02/skos/core#exactMatch",
        Field::Equivalent,
    ),
    ("http://www.w3.org/2002/07/owl#imports", Field::Metadata),
    ("http://www.w3.org/2002/07/owl#versionIRI", Field::Metadata),
    ("http://www.w3.org/2002/07/owl#versionInfo", Field::Metadata),
    ("http://www.w3.org/2002/07/owl#deprecated", Field::Metadata),
    ("http://purl.org/dc/terms/creator", Field::Metadata),
    ("http://purl.org/dc/terms/license", Field::Metadata),
];

pub(super) fn field(predicate: &str) -> Option<Field> {
    PREDICATES
        .iter()
        .find(|(iri, _)| *iri == predicate)
        .map(|(_, field)| *field)
}

pub(super) fn local_name(iri: &str) -> &str {
    iri.trim_end_matches(['/', '#'])
        .rsplit(['/', '#', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or(iri)
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Fact {
    pub(super) field: Field,
    pub(super) predicate: &'static str,
    pub(super) value: String,
    pub(super) language: Option<String>,
    pub(super) iri: bool,
}

#[derive(Default)]
pub(super) struct Term {
    pub(super) facts: BTreeSet<Fact>,
}

impl Term {
    pub(super) fn values(&self, field: Field) -> impl Iterator<Item = &Fact> {
        self.facts
            .iter()
            .skip_while(move |fact| fact.field < field)
            .take_while(move |fact| fact.field == field)
    }

    pub(super) fn label<'a>(&'a self, iri: &'a str, lang: Option<&str>) -> &'a str {
        self.values(Field::Label)
            .filter(|fact| !fact.iri)
            .min_by_key(|fact| {
                let language = fact.language.as_deref();
                let lang_rank = match language {
                    Some(tag) if lang.is_some_and(|lang| lang.eq_ignore_ascii_case(tag)) => 0,
                    Some("en") | None => 1,
                    _ => 2,
                };
                let predicate_rank = match fact.predicate {
                    PREF_LABEL => 0,
                    RDFS_LABEL => 1,
                    _ => 2,
                };
                (lang_rank, predicate_rank, &fact.value)
            })
            .map_or_else(|| local_name(iri), |fact| fact.value.as_str())
    }

    pub(super) fn is_ontology(&self) -> bool {
        self.values(Field::Type)
            .any(|fact| fact.iri && fact.value == ONTOLOGY)
    }

    pub(super) fn obsolete(&self) -> bool {
        self.values(Field::Metadata).any(|fact| {
            fact.predicate == "http://www.w3.org/2002/07/owl#deprecated"
                && !fact.iri
                && matches!(fact.value.as_str(), "true" | "1")
        })
    }
}

#[derive(Default)]
pub(super) struct Terms {
    // A catalog resource is visible once at least one authorized triple is witnessed,
    // even when none of its subjects has a named annotation or declaration.
    pub(super) visible: bool,
    pub(super) entries: BTreeMap<String, Term>,
    children: BTreeMap<String, BTreeSet<String>>,
    pub(super) ontology_iri: Option<String>,
}

impl Terms {
    pub(super) fn children<'a>(&'a self, iri: &'a str) -> impl Iterator<Item = &'a str> {
        self.children
            .get(iri)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }
}

pub(super) struct Read<'a> {
    pub(super) st: &'a AppState,
    principal: Principal,
    pub(super) control: Control,
    cancel: Arc<AtomicBool>,
    snapshots: BTreeMap<String, Arc<Snapshot>>,
    work: Arc<AtomicU64>,
    rows: usize,
    pub(super) charge: MemoryCharge,
    grant: Option<Arc<sparkles::sparql::pool::PoolGrant>>,
}

impl<'a> Read<'a> {
    pub(super) fn new(
        st: &'a AppState,
        principal: &Principal,
        grant: Option<Arc<sparkles::sparql::pool::PoolGrant>>,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        let mut control = Control::with_cancel(cancel.clone());
        control.deadline = Deadline::new(st.default_timeout).at;
        Self {
            st,
            principal: principal.clone().on_branch(None),
            control,
            cancel,
            snapshots: BTreeMap::new(),
            work: Arc::new(AtomicU64::new(0)),
            rows: 0,
            charge: MemoryCharge::new(
                grant.clone(),
                Some(st.limits.query_memory_bytes.unwrap_or(128 << 20)),
            ),
            grant,
        }
    }

    pub(super) fn check(&self) -> sparkles::Result<()> {
        self.control.check()
    }

    pub(super) fn output_charge(&self) -> sparkles::Result<MemoryCharge> {
        Ok(MemoryCharge::new(
            self.grant.clone(),
            self.charge.engine_budget()?,
        ))
    }

    pub(super) fn catalog(&self) -> ApiResult<CatalogFile> {
        self.st.resources.read().map_err(|e| {
            tracing::warn!("linked-data catalog read failed: {e:?}");
            err_code(
                StatusCode::SERVICE_UNAVAILABLE,
                "catalog-unreadable",
                "resource catalog is temporarily unavailable",
            )
        })
    }

    fn dataset(&self, name: &str) -> Option<Arc<Dataset>> {
        // Catalog publication uses the named dataset's live main state. Check again
        // against the canonical name if the path used a dataset alias.
        if !self.principal.can_at(name, Endpoint::Query, Level::Read) {
            return None;
        }
        let ds = crate::http::main_dataset(self.st, name).ok()?;
        self.principal
            .can_at(&ds.name, Endpoint::Query, Level::Read)
            .then_some(ds)
    }

    pub(super) fn dataset_terms(&mut self, name: &str) -> ApiResult<Terms> {
        let ds = self.dataset(name).ok_or_else(not_found)?;
        let mut terms = Terms::default();
        self.scan(&ds, None, &mut terms)?;
        // A dataset name is also discoverable through normal dataset read grants,
        // but its documentation must not distinguish an empty or invisible graph.
        terms.visible.then_some(terms).ok_or_else(not_found)
    }

    pub(super) fn resource_terms(&mut self, resource: &Resource) -> ApiResult<Terms> {
        let mut terms = Terms::default();
        match resource.input.kind {
            Kind::Vocabulary => {
                let mut groups = BTreeMap::<&str, BTreeSet<&str>>::new();
                for loc in &resource.input.graphs {
                    groups.entry(&loc.dataset).or_default().insert(&loc.graph);
                }
                for (name, graphs) in groups {
                    self.check()?;
                    if let Some(ds) = self.dataset(name) {
                        self.scan(&ds, Some(graphs), &mut terms)?;
                    }
                }
            }
            Kind::Dataset => {
                if let Some(name) = resource.input.dataset.as_deref()
                    && let Some(ds) = self.dataset(name)
                {
                    self.scan(&ds, None, &mut terms)?;
                }
            }
            Kind::Source => {}
        }
        Ok(terms)
    }

    fn scan(
        &mut self,
        ds: &Dataset,
        graphs: Option<BTreeSet<&str>>,
        terms: &mut Terms,
    ) -> sparkles::Result<()> {
        use std::fmt::Write as _;
        let mut opts = CallerContext::for_caller(self.st, ds, &self.principal).read_options();
        opts.grant = self.grant.clone();
        opts.deadline = self.control.deadline;
        opts.cancel = Some(self.cancel.clone());
        opts.work = Some(self.work.clone());
        opts.max_memory_bytes = self.charge.engine_budget()?;
        opts.no_cache = true;
        opts.union_default_graph = Some(false);
        opts.default_graph_extra.clear();
        opts.rdfs = None;
        opts.reasoning = None;
        // An explicit GRAPH allowlist avoids union-default behavior and keeps the
        // standard authorization machinery in charge of dynamic guards.
        let named_only = graphs.is_some();
        if let Some(graphs) = graphs {
            opts.named_graph_uris = graphs.into_iter().map(str::to_owned).collect();
        }
        let pattern = if named_only {
            "GRAPH ?g { ?s ?p ?o }"
        } else {
            "{ ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } }"
        };
        let snapshot = if let Some(snapshot) = self.snapshots.get(&ds.name) {
            snapshot.clone()
        } else {
            self.charge.charge(128 + ds.name.len() as u64)?;
            let snapshot = ds.store.snapshot();
            self.snapshots.insert(ds.name.clone(), snapshot.clone());
            snapshot
        };
        let cursor_options = sparkles::sparql::CursorOptions::default();
        let mut query = "SELECT ?s ?p ?o WHERE { VALUES ?p {".to_owned();
        for (predicate, _) in PREDICATES {
            write!(query, " <{predicate}>").expect("write to String");
        }
        write!(query, " }} {{ {pattern} }} FILTER(isIRI(?s)) }}").expect("write to String");
        {
            let mut cursor =
                sparkles::sparql::select_cursor(snapshot.clone(), &query, &opts, &cursor_options)?;
            while let Some(batch) = cursor.next_batch()? {
                for row in 0..batch.len() {
                    self.check()?;
                    self.rows = self.rows.saturating_add(1);
                    if self.rows > self.st.limits.max_rows {
                        return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                            kind: sparkles::BudgetKind::Rows,
                            limit: self.st.limits.max_rows as u64,
                            requested: self.rows as u64,
                        }));
                    }
                    let row = batch.row(row)?;
                    let (
                        Some(TermRef::NamedNode(subject)),
                        Some(TermRef::NamedNode(predicate)),
                        Some(object),
                    ) = (row.get(0)?, row.get(1)?, row.get(2)?)
                    else {
                        continue;
                    };
                    terms.visible = true;
                    self.insert(terms, subject.as_str(), predicate.as_str(), object)?;
                }
            }
        }
        if !terms.visible {
            // The probe only needs a projected constant. Hidden data cannot
            // establish visibility or become an index record.
            opts.max_memory_bytes = self.charge.engine_budget()?;
            let probe = format!("SELECT (1 AS ?witness) WHERE {{ {pattern} }} LIMIT 1");
            let mut cursor =
                sparkles::sparql::select_cursor(snapshot, &probe, &opts, &cursor_options)?;
            terms.visible = cursor.next_batch()?.is_some_and(|batch| !batch.is_empty());
        }
        Ok(())
    }

    fn insert(
        &mut self,
        terms: &mut Terms,
        subject: &str,
        predicate: &str,
        object: TermRef<'_>,
    ) -> sparkles::Result<()> {
        let Some(&(predicate, field)) = PREDICATES.iter().find(|(iri, _)| *iri == predicate) else {
            return Ok(());
        };
        let (value, language, iri) = match object {
            TermRef::NamedNode(value) => (value.as_str(), None, true),
            TermRef::Literal(value) => (value.value(), value.language(), false),
            _ => return Ok(()),
        };
        if !terms.entries.contains_key(subject) {
            self.charge.charge(160 + subject.len() as u64)?;
            terms.entries.insert(subject.to_owned(), Term::default());
        }
        let term = terms.entries.get_mut(subject).expect("inserted term");
        // Each fact owns the lexical form and language. Reserve for B-tree
        // slots, including spare slots, before either text is copied.
        let bytes = 160 + value.len() as u64 + language.map_or(0, str::len) as u64;
        self.charge.charge(bytes)?;
        if !term.facts.insert(Fact {
            field,
            predicate,
            value: value.to_owned(),
            language: language.map(str::to_owned),
            iri,
        }) {
            self.charge.release(bytes);
            return Ok(());
        }
        if field == Field::Parent
            && iri
            && subject != value
            && !terms
                .children
                .get(value)
                .is_some_and(|children| children.contains(subject))
        {
            self.charge
                .charge(160 + (value.len() + subject.len()) as u64)?;
            terms
                .children
                .entry(value.to_owned())
                .or_default()
                .insert(subject.to_owned());
        }
        if field == Field::Type
            && iri
            && value == ONTOLOGY
            && terms
                .ontology_iri
                .as_deref()
                .is_none_or(|current| subject < current)
        {
            self.charge.charge(subject.len() as u64)?;
            if let Some(old) = terms.ontology_iri.replace(subject.to_owned()) {
                self.charge.release(old.len() as u64);
            }
        }
        Ok(())
    }
}
