//! The graphs a materialization reads: data graphs, ontology graphs, and the graphs that
//! their `owl:imports` lead to.
//!
//! The reasoner reads the union of the triples of these graphs. A blank node that occurs
//! in two of them is one node, as in the union default graph. An import `?o owl:imports
//! <I>` in an input graph resolves, after the location mapping turns `I` into `L`, to the
//! named graph `I`, or else the named graph `L`. With [`ImportMode::Fetch`],
//! [`fetch_imports`] loads a missing one from `L` into the graph `I` beforehand, under the
//! rules of `LOAD`, so later runs find the copy in the dataset (Jena's
//! `OntDocumentManager` reads imports from the location its `LocationMapper` gives, and
//! keeps them in a model cache).

use crate::INFERRED_GRAPH;
use anyhow::Context as _;
use oxrdf::Term;
use serde::{Deserialize, Serialize};
use sparkles::id::Id;
use sparkles::index::Perm;
use sparkles::store::{Snapshot, Store};
use std::collections::{BTreeMap, BTreeSet};

/// `owl:imports`
pub const OWL_IMPORTS: &str = "http://www.w3.org/2002/07/owl#imports";

/// The most imports one run follows (and fetches).
pub const MAX_IMPORTS: usize = 100;

/// A graph of the dataset: the default graph or a named graph. Its text form is `default`
/// or the graph's IRI.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GraphRef {
    Default,
    Named(String),
}

impl GraphRef {
    /// `default`, or an absolute IRI.
    pub fn parse(s: &str) -> Result<GraphRef, String> {
        if s == "default" {
            return Ok(GraphRef::Default);
        }
        oxrdf::NamedNode::new(s).map_err(|e| format!("graph '{s}': {e}"))?;
        Ok(GraphRef::Named(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        match self {
            GraphRef::Default => "default",
            GraphRef::Named(g) => g,
        }
    }

    /// The graph's id in a snapshot, if it has quads there.
    pub fn id(&self, snap: &Snapshot) -> Option<Id> {
        match self {
            GraphRef::Default => Some(Id::DEFAULT_GRAPH),
            GraphRef::Named(g) => {
                let id = snap.lookup_iri(g)?;
                (snap.count(Perm::Gspo, &[id.0]).unwrap_or(0) > 0).then_some(id)
            }
        }
    }
}

impl Serialize for GraphRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for GraphRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<GraphRef, D::Error> {
        let s = String::deserialize(d)?;
        GraphRef::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// What a run does with `owl:imports`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportMode {
    /// ignore them
    None,
    /// follow them to graphs of the dataset
    #[default]
    Dataset,
    /// also fetch the missing ones ([`fetch_imports`])
    Fetch,
}

impl std::str::FromStr for ImportMode {
    type Err = String;
    fn from_str(s: &str) -> Result<ImportMode, String> {
        match s {
            "none" => Ok(ImportMode::None),
            "dataset" => Ok(ImportMode::Dataset),
            "fetch" => Ok(ImportMode::Fetch),
            _ => Err(format!(
                "unknown imports mode '{s}' (expected none, dataset or fetch)"
            )),
        }
    }
}

/// One mapping of a location mapping, as Jena's `lm:mapping` entries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MappingEntry {
    #[serde(rename_all = "camelCase")]
    Name { name: String, alt_name: String },
    #[serde(rename_all = "camelCase")]
    Prefix { prefix: String, alt_prefix: String },
}

/// Where an import is read from: an exact entry (`name` → `altName`) wins over a prefix
/// rewrite (`prefix` → `altPrefix`), and the longest matching prefix wins (Jena's
/// `LocationMapper`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Vec<MappingEntry>", into = "Vec<MappingEntry>")]
pub struct LocationMapping {
    pub names: BTreeMap<String, String>,
    pub prefixes: BTreeMap<String, String>,
}

impl From<Vec<MappingEntry>> for LocationMapping {
    fn from(v: Vec<MappingEntry>) -> LocationMapping {
        let mut m = LocationMapping::default();
        for e in v {
            match e {
                MappingEntry::Name { name, alt_name } => {
                    m.names.insert(name, alt_name);
                }
                MappingEntry::Prefix { prefix, alt_prefix } => {
                    m.prefixes.insert(prefix, alt_prefix);
                }
            }
        }
        m
    }
}

impl From<LocationMapping> for Vec<MappingEntry> {
    fn from(m: LocationMapping) -> Vec<MappingEntry> {
        let names = m
            .names
            .into_iter()
            .map(|(name, alt_name)| MappingEntry::Name { name, alt_name });
        let prefixes = m
            .prefixes
            .into_iter()
            .map(|(prefix, alt_prefix)| MappingEntry::Prefix { prefix, alt_prefix });
        names.chain(prefixes).collect()
    }
}

impl LocationMapping {
    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.prefixes.is_empty()
    }

    /// Where `iri` is read from.
    pub fn map(&self, iri: &str) -> String {
        if let Some(alt) = self.names.get(iri) {
            return alt.clone();
        }
        self.prefixes
            .iter()
            .filter(|(p, _)| iri.starts_with(p.as_str()))
            .max_by_key(|(p, _)| p.len())
            .map_or_else(
                || iri.to_string(),
                |(p, alt)| format!("{alt}{}", &iri[p.len()..]),
            )
    }

    /// A Jena location-mapping document (Turtle or another RDF syntax that `format`
    /// names): `[] lm:mapping [ lm:name "…" ; lm:altName "…" ] , [ lm:prefix "…" ;
    /// lm:altPrefix "…" ]`. Literals and IRIs are accepted alike.
    pub fn from_jena(
        bytes: &[u8],
        format: sparkles::io::RdfFormat,
    ) -> anyhow::Result<LocationMapping> {
        const LM: &str = "http://jena.hpl.hp.com/2004/08/location-mapping#";
        let (quads, _) = sparkles::io::parse_to_vec(&sparkles::io::Source::from_bytes(
            bytes.to_vec(),
            format,
            None,
        ))
        .context("parsing the location mapping")?;
        let text = |t: &Term| match t {
            Term::Literal(l) => Some(l.value().to_string()),
            Term::NamedNode(n) => Some(n.as_str().to_string()),
            _ => None,
        };
        let mut by_node: BTreeMap<String, BTreeMap<&str, String>> = BTreeMap::new();
        for q in &quads {
            let Some(local) = q.predicate.as_str().strip_prefix(LM) else {
                continue;
            };
            if let ("name" | "altName" | "prefix" | "altPrefix", Some(v)) = (local, text(&q.object))
            {
                by_node
                    .entry(q.subject.to_string())
                    .or_default()
                    .insert(local, v);
            }
        }
        let mut m = LocationMapping::default();
        for (node, e) in by_node {
            match (
                e.get("name"),
                e.get("altName"),
                e.get("prefix"),
                e.get("altPrefix"),
            ) {
                (Some(n), Some(a), None, None) => {
                    m.names.insert(n.clone(), a.clone());
                }
                (None, None, Some(p), Some(a)) => {
                    m.prefixes.insert(p.clone(), a.clone());
                }
                _ => anyhow::bail!(
                    "location mapping {node}: expected lm:name with lm:altName, or lm:prefix with lm:altPrefix"
                ),
            }
        }
        Ok(m)
    }
}

/// The graphs a run reads, as configured.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inputs {
    /// graphs whose triples the rules read
    #[serde(default = "default_data")]
    pub data_graphs: Vec<GraphRef>,
    /// graphs that hold the ontology, read the same way
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ontology_graphs: Vec<GraphRef>,
    #[serde(default)]
    pub imports: ImportMode,
    #[serde(default, skip_serializing_if = "LocationMapping::is_empty")]
    pub location_mapping: LocationMapping,
}

fn default_data() -> Vec<GraphRef> {
    vec![GraphRef::Default]
}

impl Default for Inputs {
    fn default() -> Inputs {
        Inputs {
            data_graphs: default_data(),
            ontology_graphs: Vec::new(),
            imports: ImportMode::Dataset,
            location_mapping: LocationMapping::default(),
        }
    }
}

impl Inputs {
    /// The configured graphs, data graphs first, each once.
    pub fn configured(&self) -> Vec<GraphRef> {
        let mut out: Vec<GraphRef> = Vec::new();
        for g in self.data_graphs.iter().chain(&self.ontology_graphs) {
            if !out.contains(g) {
                out.push(g.clone());
            }
        }
        out
    }

    /// An error for a configuration that reads no graph or reads the inferred graph.
    pub fn validate(&self) -> anyhow::Result<()> {
        let all = self.configured();
        if all.is_empty() {
            anyhow::bail!("no input graph: name a data graph or an ontology graph");
        }
        if all.iter().any(|g| g.as_str() == INFERRED_GRAPH) {
            anyhow::bail!("the inferred graph <{INFERRED_GRAPH}> cannot be an input");
        }
        Ok(())
    }
}

/// An import a run found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Import {
    pub iri: String,
    /// where the location mapping sends it, when that differs from the IRI
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// the graph it resolved to (`None`: it did not resolve)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,
}

impl Import {
    /// Where the import is read from.
    pub fn source(&self) -> &str {
        self.location.as_deref().unwrap_or(&self.iri)
    }
}

/// The graphs a run reads, with its imports resolved against one snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolved {
    /// the graphs read: the configured ones, then those imports resolved to
    pub graphs: Vec<GraphRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<Import>,
    #[serde(skip)]
    pub warnings: Vec<String>,
}

impl Resolved {
    /// The graphs whose changes make the inferences stale: those read, and those an
    /// import that did not resolve would be read from.
    pub fn watched(&self) -> Vec<GraphRef> {
        let mut out: BTreeSet<GraphRef> = self.graphs.iter().cloned().collect();
        for i in self.imports.iter().filter(|i| i.graph.is_none()) {
            out.insert(GraphRef::Named(i.iri.clone()));
            if let Some(l) = &i.location {
                out.insert(GraphRef::Named(l.clone()));
            }
        }
        out.into_iter().collect()
    }

    /// The graphs read, sorted: two runs over the same set read the same triples.
    pub fn graph_set(&self) -> Vec<GraphRef> {
        let mut g = self.graphs.clone();
        g.sort();
        g.dedup();
        g
    }

    /// Whether the run reads the default graph alone.
    pub fn default_only(&self) -> bool {
        self.graph_set() == [GraphRef::Default]
    }

    /// The ids of the graphs read that have quads in `snap`.
    pub fn ids(&self, snap: &Snapshot) -> Vec<Id> {
        let mut ids: Vec<Id> = self.graph_set().iter().filter_map(|g| g.id(snap)).collect();
        ids.dedup();
        ids
    }
}

/// The `owl:imports` objects in graph `g` of `snap`.
fn imports_in(snap: &Snapshot, g: Id, warnings: &mut Vec<String>) -> Vec<String> {
    let Some(p) = snap.lookup_iri(OWL_IMPORTS) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for k in snap.scan_keys(Perm::Pos, &[p.0]).unwrap_or_default() {
        let q = Perm::Pos.to_quad(&k);
        if q[3] != g {
            continue;
        }
        match snap.term(q[2]) {
            Some(Term::NamedNode(n)) => out.push(n.into_string()),
            Some(t) => warnings.push(format!("owl:imports {t} is not an IRI and was ignored")),
            None => {}
        }
    }
    out
}

/// The graphs `inputs` reads in `snap`, with the imports resolved.
pub fn resolve(snap: &Snapshot, inputs: &Inputs) -> Resolved {
    let mut r = Resolved {
        graphs: inputs.configured(),
        ..Default::default()
    };
    for g in &r.graphs {
        if g.id(snap).is_none() {
            r.warnings
                .push(format!("the input graph <{}> is empty", g.as_str()));
        }
    }
    if inputs.imports == ImportMode::None {
        return r;
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut i = 0;
    while i < r.graphs.len() {
        let Some(g) = r.graphs[i].id(snap) else {
            i += 1;
            continue;
        };
        i += 1;
        let mut warnings = Vec::new();
        for iri in imports_in(snap, g, &mut warnings) {
            if !seen.insert(iri.clone()) {
                continue;
            }
            if r.imports.len() >= MAX_IMPORTS {
                warnings.push(format!(
                    "more than {MAX_IMPORTS} imports: <{iri}> and later ones were not followed"
                ));
                break;
            }
            let location = inputs.location_mapping.map(&iri);
            let exists = |name: &str| GraphRef::Named(name.to_string()).id(snap).is_some();
            let graph = if exists(&iri) {
                Some(iri.clone())
            } else if location != iri && exists(&location) {
                Some(location.clone())
            } else {
                None
            };
            match &graph {
                Some(name) => {
                    let gr = GraphRef::Named(name.clone());
                    if !r.graphs.contains(&gr) {
                        r.graphs.push(gr);
                    }
                }
                None => warnings.push(format!("owl:imports <{iri}> did not resolve to a graph")),
            }
            r.imports.push(Import {
                location: (location != iri).then_some(location),
                iri,
                graph,
            });
        }
        r.warnings.extend(warnings);
    }
    r
}

/// What [`fetch_imports`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fetched {
    /// the imports whose documents were loaded, by IRI
    pub fetched: Vec<String>,
    pub warnings: Vec<String>,
}

/// Whether a location is a document `LOAD` can read.
fn fetchable(location: &str) -> bool {
    ["http://", "https://", "file:"]
        .iter()
        .any(|p| location.starts_with(p))
}

/// Load the imports of `inputs` that resolve to no graph from their locations, each into
/// the graph named by its IRI, until every import resolves or cannot be fetched. With
/// [`ImportMode::Fetch`] only. The imports in `refresh` (by IRI) are loaded again,
/// replacing their graphs.
///
/// Each document is loaded by a SPARQL `LOAD` with `opts`, in its own commit, so the
/// outbound policy, the file-load rules, the timeouts and the response ceiling of `opts`
/// apply, and all the fetches share one request budget. A refused destination, a spent
/// budget, a timeout or a cancellation fails the call; another failure is a warning.
pub fn fetch_imports(
    store: &Store,
    inputs: &Inputs,
    refresh: &[String],
    opts: &sparkles::sparql::QueryOptions,
) -> anyhow::Result<Fetched> {
    let mut out = Fetched::default();
    if inputs.imports != ImportMode::Fetch {
        if !refresh.is_empty() {
            out.warnings
                .push("imports are fetched only with imports: fetch; nothing was refreshed".into());
        }
        return Ok(out);
    }
    let mut opts = opts.clone();
    opts.outbound_budget = Some(sparkles::outbound::RequestBudget::new(&opts.outbound));
    let mut attempted: BTreeSet<String> = BTreeSet::new();
    // `Ok(true)`: a new attempt (whatever its outcome)
    let mut load = |iri: &str, location: &str, replace: bool, out: &mut Fetched| {
        if !attempted.insert(iri.to_string()) {
            return Ok(false);
        }
        if !fetchable(location) || oxrdf::NamedNode::new(location).is_err() {
            out.warnings.push(format!(
                "owl:imports <{iri}>: <{location}> is not an http, https or file URL"
            ));
            return Ok(true);
        }
        let u = if replace {
            format!("DROP SILENT GRAPH <{iri}> ; LOAD <{location}> INTO GRAPH <{iri}>")
        } else {
            format!("LOAD <{location}> INTO GRAPH <{iri}>")
        };
        match sparkles::sparql::update::update(store, &u, &opts) {
            Ok(_) => {
                tracing::info!(import = iri, location, "fetched an import");
                out.fetched.push(iri.to_string());
                Ok(true)
            }
            Err(
                e @ (sparkles::Error::NotPermitted(_)
                | sparkles::Error::BudgetExceeded(_)
                | sparkles::Error::Timeout
                | sparkles::Error::Cancelled),
            ) => Err(anyhow::Error::new(e).context(format!("fetching owl:imports <{iri}>"))),
            Err(e) => {
                out.warnings
                    .push(format!("owl:imports <{iri}> could not be fetched: {e}"));
                Ok(true)
            }
        }
    };
    for iri in refresh {
        let location = inputs.location_mapping.map(iri);
        load(iri, &location, true, &mut out)?;
    }
    loop {
        let r = resolve(&store.snapshot(), inputs);
        let mut tried = false;
        for i in r.imports.iter().filter(|i| i.graph.is_none()) {
            tried |= load(&i.iri, i.source(), false, &mut out)?;
        }
        if !tried {
            return Ok(out);
        }
    }
}
