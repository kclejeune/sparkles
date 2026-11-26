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
use crate::budget::{Budget, BudgetFailure, Held, WorkingWarnings, table_bytes, table_grown};
use anyhow::Context as _;
use oxrdf::Term;
use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};
use sparkles_core::access::GraphAccess;
use sparkles_core::id::{self, Id, Tag};
use sparkles_core::incr::footprint::{FGraph, Footprint};
use sparkles_core::index::Perm;
use sparkles_core::store::{Chunk, Snapshot, Store, TermKind};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

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

    /// This graph reference as a shared incremental [`FGraph`].
    pub fn to_fgraph(&self) -> FGraph {
        match self {
            GraphRef::Default => FGraph::Default,
            GraphRef::Named(g) => {
                FGraph::Graph(Term::NamedNode(oxrdf::NamedNode::new_unchecked(g.clone())))
            }
        }
    }

    /// Recover a [`GraphRef`] from a shared incremental [`FGraph`], when it names a
    /// concrete default or named graph.
    pub fn from_fgraph(g: &FGraph) -> Option<GraphRef> {
        match g {
            FGraph::Default => Some(GraphRef::Default),
            FGraph::Graph(Term::NamedNode(n)) => Some(GraphRef::Named(n.as_str().to_string())),
            _ => None,
        }
    }

    /// The graph's id in a snapshot, if it has quads there.
    pub fn id(&self, snap: &Snapshot) -> Option<Id> {
        self.try_id(snap).unwrap_or(None)
    }

    /// [`id`](Self::id), with an error when the index cannot say whether the graph has
    /// quads, rather than counting it absent.
    pub fn try_id(&self, snap: &Snapshot) -> sparkles_core::Result<Option<Id>> {
        match self {
            GraphRef::Default => Ok(Some(Id::DEFAULT_GRAPH)),
            GraphRef::Named(g) => {
                let Some(id) = snap.lookup_iri(g) else {
                    return Ok(None);
                };
                Ok((snap.count(Perm::Gspo, &[id.0])? > 0).then_some(id))
            }
        }
    }

    /// Controlled [`id`](Self::id) that admits IRI lookup scratch against
    /// `budget` and polls `check`.
    fn try_id_controlled(&self, snap: &Snapshot, ctrl: &Control<'_>) -> anyhow::Result<Option<Id>> {
        ctrl.check()?;
        match self {
            GraphRef::Default => Ok(Some(Id::DEFAULT_GRAPH)),
            GraphRef::Named(g) => {
                let Some(id) = lookup_iri_controlled(snap, g, ctrl)? else {
                    return Ok(None);
                };
                Ok(has_quads_controlled(snap, id, ctrl)?.then_some(id))
            }
        }
    }
}

struct Control<'a> {
    budget: &'a Arc<Budget>,
    check: std::cell::RefCell<&'a mut dyn FnMut() -> anyhow::Result<()>>,
}

impl Control<'_> {
    fn check(&self) -> anyhow::Result<()> {
        (self.check.borrow_mut())()
    }
}

fn lookup_iri_controlled(
    snap: &Snapshot,
    iri: &str,
    ctrl: &Control<'_>,
) -> anyhow::Result<Option<Id>> {
    ctrl.check()?;
    let len = iri
        .len()
        .checked_add(1)
        .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
    let mut key_held = match Held::try_new_exact(ctrl.budget, len) {
        Ok(held) => held,
        Err(err) => {
            ctrl.check()?;
            return Err(err.into());
        }
    };
    let mut key = Vec::with_capacity(len);
    if key.capacity() > len
        && let Err(err) = key_held.try_grow(key.capacity() - len)
    {
        ctrl.check()?;
        return Err(err.into());
    }
    key.push(b'<');
    key.extend_from_slice(iri.as_bytes());
    let scratch = std::cell::RefCell::new(Held::new(ctrl.budget));
    snap.lookup_key_controlled(
        &key,
        |old, new| -> anyhow::Result<()> {
            if let Err(err) = scratch.borrow_mut().try_set(old.saturating_add(new)) {
                ctrl.check()?;
                return Err(err.into());
            }
            Ok(())
        },
        |capacity| -> anyhow::Result<()> {
            if let Err(err) = scratch.borrow_mut().try_set(capacity) {
                ctrl.check()?;
                return Err(err.into());
            }
            Ok(())
        },
        || ctrl.check(),
    )
}

enum ImportObject {
    Iri(String),
    NonIri(Term),
}

fn admit_string(s: &str, ctrl: &Control<'_>) -> anyhow::Result<(String, Held)> {
    ctrl.check()?;
    let len = s.len();
    let mut held = match Held::try_new_exact(ctrl.budget, len) {
        Ok(held) => held,
        Err(err) => {
            ctrl.check()?;
            return Err(err.into());
        }
    };
    let mut out = String::with_capacity(len);
    if out.capacity() > len
        && let Err(err) = held.try_grow(out.capacity() - len)
    {
        ctrl.check()?;
        return Err(err.into());
    }
    out.push_str(s);
    Ok((out, held))
}

fn push_warning(
    warnings: &mut WorkingWarnings,
    args: std::fmt::Arguments<'_>,
    ctrl: &Control<'_>,
) -> anyhow::Result<()> {
    ctrl.check()?;
    if let Err(err) = warnings.push_fmt(args) {
        ctrl.check()?;
        return Err(err.into());
    }
    Ok(())
}

fn push_vec_controlled<T>(
    vec: &mut Vec<T>,
    value: T,
    held: &mut Held,
    ctrl: &Control<'_>,
) -> anyhow::Result<()> {
    let old_cap = vec.capacity();
    let elem = std::mem::size_of::<T>();
    let next_cap = sparkles_budget::sizes::vec_grown(old_cap, vec.len(), 1, elem);
    if next_cap != old_cap {
        let next_bytes = next_cap
            .checked_mul(elem)
            .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
        let mut grow = match Held::try_new_exact(ctrl.budget, next_bytes) {
            Ok(h) => h,
            Err(err) => {
                ctrl.check()?;
                return Err(err.into());
            }
        };
        vec.reserve_exact(next_cap - vec.len());
        let actual = vec
            .capacity()
            .checked_mul(elem)
            .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
        if actual > next_bytes {
            if let Err(err) = grow.try_grow(actual - next_bytes) {
                ctrl.check()?;
                return Err(err.into());
            }
        } else {
            grow.shrink(next_bytes - actual);
        }
        held.try_merge(&mut grow)?;
        held.shrink(old_cap * elem);
    }
    vec.push(value);
    Ok(())
}

fn push_deque_controlled<T>(
    deque: &mut VecDeque<T>,
    value: T,
    held: &mut Held,
    ctrl: &Control<'_>,
) -> anyhow::Result<()> {
    if deque.len() == deque.capacity() {
        let old_cap = deque.capacity();
        let next_cap = old_cap
            .checked_mul(2)
            .map(|n| n.max(4))
            .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
        let next_bytes = next_cap
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
        let mut grow = match Held::try_new_exact(ctrl.budget, next_bytes) {
            Ok(h) => h,
            Err(err) => {
                ctrl.check()?;
                return Err(err.into());
            }
        };
        deque.reserve(1);
        let actual = deque
            .capacity()
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
        if actual > next_bytes {
            if let Err(err) = grow.try_grow(actual - next_bytes) {
                ctrl.check()?;
                return Err(err.into());
            }
        } else {
            grow.shrink(next_bytes - actual);
        }
        held.try_merge(&mut grow)?;
        held.shrink(old_cap * std::mem::size_of::<T>());
    }
    deque.push_back(value);
    Ok(())
}

fn insert_id_set_controlled(
    set: &mut FxHashSet<Id>,
    id: Id,
    held: &mut Held,
    ctrl: &Control<'_>,
) -> anyhow::Result<bool> {
    if set.contains(&id) {
        return Ok(false);
    }
    let old_cap = set.capacity();
    let next_cap = table_grown(old_cap, set.len(), 1);
    if next_cap != old_cap {
        let elem = std::mem::size_of::<Id>();
        let old_bytes = table_bytes(old_cap, elem);
        let next_bytes = table_bytes(next_cap, elem);
        let mut grow = match Held::try_new_exact(ctrl.budget, next_bytes) {
            Ok(h) => h,
            Err(err) => {
                ctrl.check()?;
                return Err(err.into());
            }
        };
        set.reserve(1);
        let actual = table_bytes(set.capacity(), elem);
        if actual > next_bytes {
            if let Err(err) = grow.try_grow(actual - next_bytes) {
                ctrl.check()?;
                return Err(err.into());
            }
        } else {
            grow.shrink(next_bytes - actual);
        }
        held.try_merge(&mut grow)?;
        held.shrink(old_bytes);
    }
    Ok(set.insert(id))
}

fn has_quads_controlled(snap: &Snapshot, id: Id, ctrl: &Control<'_>) -> anyhow::Result<bool> {
    ctrl.check()?;
    match snap.count(Perm::Gspo, &[id.0]) {
        Ok(n) => Ok(n > 0),
        Err(
            e @ (sparkles_core::Error::Cancelled
            | sparkles_core::Error::Timeout
            | sparkles_core::Error::BudgetExceeded(_)),
        ) => Err(e.into()),
        Err(_) => Ok(false),
    }
}

fn decode_iri_controlled(
    snap: &Snapshot,
    id: Id,
    ctrl: &Control<'_>,
) -> anyhow::Result<Option<(String, Held)>> {
    ctrl.check()?;
    let decode_key = |key: &[u8]| -> anyhow::Result<Option<(String, Held)>> {
        if let Some(iri_bytes) = key.strip_prefix(b"<")
            && let Ok(iri) = std::str::from_utf8(iri_bytes)
        {
            return admit_string(iri, ctrl).map(Some);
        }
        Ok(None)
    };
    match id.tag() {
        Tag::Vocab => {
            if id.payload() >= snap.generation.vocab.len()
                || !snap.generation.vocab.is_iri(id.payload())
            {
                return Ok(None);
            }
            let scratch = std::cell::RefCell::new(Held::new(ctrl.budget));
            Ok(snap
                .generation
                .vocab
                .get_with_controlled_capacity_checked(
                    id.payload(),
                    |old, new| -> anyhow::Result<()> {
                        if let Err(err) = scratch.borrow_mut().try_set(old.saturating_add(new)) {
                            ctrl.check()?;
                            return Err(err.into());
                        }
                        Ok(())
                    },
                    |capacity| -> anyhow::Result<()> {
                        if let Err(err) = scratch.borrow_mut().try_set(capacity) {
                            ctrl.check()?;
                            return Err(err.into());
                        }
                        Ok(())
                    },
                    |_| ctrl.check(),
                    decode_key,
                )?
                .flatten())
        }
        Tag::Delta => snap.generation.dvocab.with(|vocab| {
            let Some(key) = vocab.get(id.payload()) else {
                return Ok(None);
            };
            decode_key(key)
        }),
        _ => Ok(None),
    }
}

fn decode_import_object_controlled(
    snap: &Snapshot,
    id: Id,
    need_non_iri: bool,
    ctrl: &Control<'_>,
) -> anyhow::Result<Option<(ImportObject, Held)>> {
    ctrl.check()?;
    if !need_non_iri {
        return Ok(decode_iri_controlled(snap, id, ctrl)?
            .map(|(iri, held)| (ImportObject::Iri(iri), held)));
    }
    let decode_key = |key: &[u8]| -> anyhow::Result<(ImportObject, Held)> {
        if let Some(iri_bytes) = key.strip_prefix(b"<")
            && let Ok(iri) = std::str::from_utf8(iri_bytes)
        {
            let (s, held) = admit_string(iri, ctrl)?;
            return Ok((ImportObject::Iri(s), held));
        }
        let bytes = usize::try_from(id::key_decode_bytes(key)?).unwrap_or(usize::MAX);
        let held = match Held::try_new_exact(ctrl.budget, bytes) {
            Ok(held) => held,
            Err(err) => {
                ctrl.check()?;
                return Err(err.into());
            }
        };
        ctrl.check()?;
        Ok((ImportObject::NonIri(id::key_to_term(key)), held))
    };
    match id.tag() {
        Tag::Vocab => {
            let scratch = std::cell::RefCell::new(Held::new(ctrl.budget));
            snap.generation.vocab.get_with_controlled_capacity_checked(
                id.payload(),
                |old, new| -> anyhow::Result<()> {
                    if let Err(err) = scratch.borrow_mut().try_set(old.saturating_add(new)) {
                        ctrl.check()?;
                        return Err(err.into());
                    }
                    Ok(())
                },
                |capacity| -> anyhow::Result<()> {
                    if let Err(err) = scratch.borrow_mut().try_set(capacity) {
                        ctrl.check()?;
                        return Err(err.into());
                    }
                    Ok(())
                },
                |_| ctrl.check(),
                decode_key,
            )
        }
        Tag::Delta => snap.generation.dvocab.with(|vocab| {
            let Some(key) = vocab.get(id.payload()) else {
                return Ok(None);
            };
            decode_key(key).map(Some)
        }),
        _ => {
            let Some(term) = snap.term(id) else {
                return Ok(None);
            };
            let bytes = match &term {
                Term::BlankNode(b) => b.as_str().len(),
                Term::Literal(l) => l.value().len(),
                Term::NamedNode(n) => n.as_str().len(),
                Term::Triple(_) => 64,
            }
            .saturating_add(std::mem::size_of::<Term>());
            let held = match Held::try_new_exact(ctrl.budget, bytes) {
                Ok(held) => held,
                Err(err) => {
                    ctrl.check()?;
                    return Err(err.into());
                }
            };
            Ok(Some((ImportObject::NonIri(term), held)))
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

    /// Mapped location for `iri` only when it differs from `iri`, admitting the
    /// allocated string against `ctrl.budget` and keeping its credit with the result.
    fn map_controlled(
        &self,
        iri: &str,
        ctrl: &Control<'_>,
    ) -> anyhow::Result<Option<(String, Held)>> {
        ctrl.check()?;
        if let Some(alt) = self.names.get(iri) {
            if alt == iri {
                return Ok(None);
            }
            return admit_string(alt, ctrl).map(Some);
        }
        if let Some((prefix, alt)) = self
            .prefixes
            .iter()
            .filter(|(p, _)| iri.starts_with(p.as_str()))
            .max_by_key(|(p, _)| p.len())
        {
            if alt == prefix {
                return Ok(None);
            }
            let suffix = &iri[prefix.len()..];
            let len = alt
                .len()
                .checked_add(suffix.len())
                .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
            let mut held = match Held::try_new_exact(ctrl.budget, len) {
                Ok(held) => held,
                Err(err) => {
                    ctrl.check()?;
                    return Err(err.into());
                }
            };
            let mut out = String::with_capacity(len);
            if out.capacity() > len
                && let Err(err) = held.try_grow(out.capacity() - len)
            {
                ctrl.check()?;
                return Err(err.into());
            }
            out.push_str(alt);
            out.push_str(suffix);
            return Ok(Some((out, held)));
        }
        Ok(None)
    }

    /// A Jena location-mapping document (Turtle or another RDF syntax that `format`
    /// names): `[] lm:mapping [ lm:name "…" ; lm:altName "…" ] , [ lm:prefix "…" ;
    /// lm:altPrefix "…" ]`. Literals and IRIs are accepted alike.
    pub fn from_jena(
        bytes: &[u8],
        format: sparkles_core::io::RdfFormat,
    ) -> anyhow::Result<LocationMapping> {
        const LM: &str = "http://jena.hpl.hp.com/2004/08/location-mapping#";
        let (quads, _) = sparkles_core::io::parse_to_vec(&sparkles_core::io::Source::from_bytes(
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
    /// graphs the run read besides its input graphs, such as the shapes graphs of the
    /// `shacl-af` profile, whose changes also make the inferences stale
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_read: Vec<GraphRef>,
    #[serde(skip)]
    pub warnings: Vec<String>,
}

impl Resolved {
    /// The footprint of the graphs whose changes make the inferences stale: those read,
    /// and those an import that did not resolve would be read from, in deterministic
    /// graph order.
    pub fn footprint(&self) -> Footprint {
        let entries = self
            .watched()
            .into_iter()
            .map(|g| sparkles_core::incr::footprint::Entry {
                g: g.to_fgraph(),
                s: None,
                p: None,
                o: None,
            })
            .collect();
        Footprint {
            entries,
            all: false,
        }
    }

    /// The graphs whose changes make the inferences stale: those read, and those an
    /// import that did not resolve would be read from.
    pub fn watched(&self) -> Vec<GraphRef> {
        let mut out: BTreeSet<GraphRef> = self.graphs.iter().cloned().collect();
        out.extend(self.also_read.iter().cloned());
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

    /// The ids of the readable graphs read that have quads in `snap`, checking
    /// readability by known [`GraphRef`] name before looking up the graph id.
    pub fn readable_ids(&self, snap: &Snapshot, readable: &dyn Fn(&GraphRef) -> bool) -> Vec<Id> {
        let mut ids: Vec<Id> = self
            .graph_set()
            .into_iter()
            .filter(|g| readable(g))
            .filter_map(|g| g.id(snap))
            .collect();
        ids.dedup();
        ids
    }
}

/// The `owl:imports` objects in graph `g` of `snap`. A strict read fails on a scan
/// error or an object it cannot decode, and a lenient one skips them.
fn imports_in(
    snap: &Snapshot,
    g: Id,
    warnings: &mut Vec<String>,
    strict: bool,
) -> anyhow::Result<Vec<String>> {
    let Some(p) = snap.lookup_iri(OWL_IMPORTS) else {
        return Ok(Vec::new());
    };
    let keys = match snap.scan_keys(Perm::Pos, &[p.0]) {
        Ok(keys) => keys,
        Err(e) if strict => {
            return Err(anyhow::Error::new(e).context("reading the owl:imports statements"));
        }
        Err(_) => Vec::new(),
    };
    let mut out = Vec::new();
    for k in keys {
        let q = Perm::Pos.to_quad(&k);
        if q[3] != g {
            continue;
        }
        match snap.term(q[2]) {
            Some(Term::NamedNode(n)) => out.push(n.into_string()),
            Some(t) => warnings.push(format!("owl:imports {t} is not an IRI and was ignored")),
            None if strict => {
                anyhow::bail!(
                    "the object of an owl:imports statement cannot be decoded: the index is damaged"
                )
            }
            None => {}
        }
    }
    Ok(out)
}

/// The graphs `inputs` reads in `snap`, with the imports resolved.
pub fn resolve(snap: &Snapshot, inputs: &Inputs) -> Resolved {
    resolve_readable(snap, inputs, &|_| true)
}

/// [`resolve`] for a caller who reads only the graphs `readable` admits. Imports are
/// followed only through readable graphs, a graph the caller cannot read counts as
/// absent, and no warning names what such a graph holds.
pub fn resolve_readable(
    snap: &Snapshot,
    inputs: &Inputs,
    readable: &dyn Fn(Id) -> bool,
) -> Resolved {
    resolve_closure(snap, inputs, readable, &ImportTargets::default()).0
}

/// [`resolve_readable`] checking readability by known [`GraphRef`] name without
/// decoding graph IDs back through `Snapshot::term`.
pub fn resolve_readable_iri(
    snap: &Snapshot,
    inputs: &Inputs,
    readable: &dyn Fn(&GraphRef) -> bool,
) -> Resolved {
    match closure(
        snap,
        inputs,
        ReadableCheck::ByRef(readable),
        &ImportTargets::default(),
        false,
        &|| Ok(()),
    ) {
        Ok((r, _)) => r,
        Err(_) => unreachable!("a lenient resolution does not fail"),
    }
}

/// Controlled graph and `owl:imports` resolution for a demand session.
///
/// Checks readability using known graph IRI names without decoding IDs via
/// `Snapshot::term`, processes `owl:imports` objects incrementally while keeping
/// budget credit alive with each decoded name, writes retained warnings directly
/// into `warnings`, and returns a dedicated [`Held`] that keeps the resolved
/// graph vector charged for the caller's lifetime.
pub(crate) fn resolve_session_inputs_controlled(
    snap: &Snapshot,
    inputs: &Inputs,
    readable: Option<&GraphAccess>,
    warnings: &mut WorkingWarnings,
    budget: &Arc<Budget>,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<(Vec<Id>, Held)> {
    resolve_inputs_controlled_inner(snap, inputs, readable, Some(warnings), budget, check)
}

/// Controlled graph and `owl:imports` resolution returning resolved graph IDs and the
/// retained heap bytes of the returned vector, bounded by `max_memory` and `check`.
pub fn resolve_ids_controlled(
    snap: &Snapshot,
    inputs: &Inputs,
    readable: Option<&GraphAccess>,
    max_memory: Option<usize>,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<(Vec<Id>, usize)> {
    resolve_ids_controlled_with_parent(snap, inputs, readable, max_memory, None, check)
}

/// [`resolve_ids_controlled`], reserving from `memory_parent` when present.
pub fn resolve_ids_controlled_with_parent(
    snap: &Snapshot,
    inputs: &Inputs,
    readable: Option<&GraphAccess>,
    max_memory: Option<usize>,
    memory_parent: Option<crate::ParentHandle>,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<(Vec<Id>, usize)> {
    let budget = Budget::with_parent(max_memory, memory_parent);
    let (graphs, held) =
        resolve_inputs_controlled_inner(snap, inputs, readable, None, &budget, check)?;
    let bytes = held.bytes();
    Ok((graphs, bytes))
}

/// Convert resolved graph IDs (plus any extra [`GraphRef`] entries) into a retained
/// `Vec<GraphRef>` under `max_memory` and `check`, decoding named graph IRIs without
/// unchecked `Snapshot::term` calls and returning the retained heap bytes.
pub fn graph_refs_controlled(
    snap: &Snapshot,
    graphs: &[Id],
    extra: &[GraphRef],
    max_memory: Option<usize>,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<(Vec<GraphRef>, usize)> {
    graph_refs_controlled_with_parent(snap, graphs, extra, max_memory, None, check)
}

/// [`graph_refs_controlled`], reserving from `memory_parent` when present.
pub fn graph_refs_controlled_with_parent(
    snap: &Snapshot,
    graphs: &[Id],
    extra: &[GraphRef],
    max_memory: Option<usize>,
    memory_parent: Option<crate::ParentHandle>,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<(Vec<GraphRef>, usize)> {
    let budget = Budget::with_parent(max_memory, memory_parent);
    let ctrl = Control {
        budget: &budget,
        check: std::cell::RefCell::new(check),
    };
    let mut refs: Vec<GraphRef> = Vec::new();
    let mut held = Held::new(&budget);
    for &id in graphs {
        ctrl.check()?;
        if id == Id::DEFAULT_GRAPH {
            push_vec_controlled(&mut refs, GraphRef::Default, &mut held, &ctrl)?;
        } else if let Some((iri, mut iri_held)) = decode_iri_controlled(snap, id, &ctrl)? {
            push_vec_controlled(&mut refs, GraphRef::Named(iri), &mut held, &ctrl)?;
            held.try_merge(&mut iri_held)?;
        }
    }
    for graph in extra {
        ctrl.check()?;
        match graph {
            GraphRef::Default => {
                push_vec_controlled(&mut refs, GraphRef::Default, &mut held, &ctrl)?;
            }
            GraphRef::Named(name) => {
                let (iri, mut iri_held) = admit_string(name, &ctrl)?;
                push_vec_controlled(&mut refs, GraphRef::Named(iri), &mut held, &ctrl)?;
                held.try_merge(&mut iri_held)?;
            }
        }
    }
    let bytes = held.bytes();
    Ok((refs, bytes))
}

fn resolve_inputs_controlled_inner(
    snap: &Snapshot,
    inputs: &Inputs,
    readable: Option<&GraphAccess>,
    mut warnings: Option<&mut WorkingWarnings>,
    budget: &Arc<Budget>,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<(Vec<Id>, Held)> {
    let ctrl = Control {
        budget,
        check: std::cell::RefCell::new(check),
    };
    let allows_ref = |g: &GraphRef| {
        readable.is_none_or(|a| match g {
            GraphRef::Default => a.read.default_graph(),
            GraphRef::Named(iri) => a.read.allows_named_iri(iri),
        })
    };
    let allows_named = |iri: &str| readable.is_none_or(|a| a.read.allows_named_iri(iri));

    ctrl.check()?;
    let mut state_held = Held::new(budget);
    let mut graphs_held = Held::new(budget);
    let mut graphs: Vec<Id> = Vec::new();
    let mut graph_set: FxHashSet<Id> = FxHashSet::default();
    let mut queue: VecDeque<Id> = VecDeque::new();

    let mut seen_configured: Vec<&GraphRef> = Vec::new();
    for g in inputs.data_graphs.iter().chain(&inputs.ontology_graphs) {
        ctrl.check()?;
        if seen_configured.contains(&g) {
            continue;
        }
        push_vec_controlled(&mut seen_configured, g, &mut state_held, &ctrl)?;
        if !allows_ref(g) {
            continue;
        }
        match g.try_id_controlled(snap, &ctrl)? {
            Some(gid) => {
                if insert_id_set_controlled(&mut graph_set, gid, &mut state_held, &ctrl)? {
                    push_vec_controlled(&mut graphs, gid, &mut graphs_held, &ctrl)?;
                    push_deque_controlled(&mut queue, gid, &mut state_held, &ctrl)?;
                }
            }
            None => {
                if let Some(warnings) = warnings.as_deref_mut() {
                    push_warning(
                        warnings,
                        format_args!("the input graph <{}> is empty", g.as_str()),
                        &ctrl,
                    )?;
                }
            }
        }
    }

    if inputs.imports != ImportMode::None
        && !queue.is_empty()
        && let Some(owl_imports) = lookup_iri_controlled(snap, OWL_IMPORTS, &ctrl)?
    {
        let mut seen_iris: FxHashSet<Id> = FxHashSet::default();
        let mut import_count = 0usize;
        let mut stopped = false;

        while let Some(g) = queue.pop_front() {
            ctrl.check()?;
            let mut objects: Vec<Id> = Vec::new();
            let mut objects_held = Held::new(budget);
            let mut scan_err: Option<anyhow::Error> = None;
            let scan_res = snap.scan(Perm::Pos, &[owl_imports.0], |chunk| {
                if let Err(err) = ctrl.check() {
                    scan_err = Some(err);
                    return Ok(false);
                }
                let mut visit = |k: [u64; 4]| -> bool {
                    let q = Perm::Pos.to_quad(&k);
                    if q[3] == g
                        && let Err(err) =
                            push_vec_controlled(&mut objects, q[2], &mut objects_held, &ctrl)
                    {
                        scan_err = Some(err);
                        return false;
                    }
                    true
                };
                Ok(match chunk {
                    Chunk::Block(b, s, e) => (s..e).all(|i| visit(b.key(i))),
                    Chunk::Row(k) => visit(k),
                })
            });
            if let Some(err) = scan_err {
                return Err(err);
            }
            match scan_res {
                Ok(()) => {}
                Err(
                    e @ (sparkles_core::Error::Cancelled
                    | sparkles_core::Error::Timeout
                    | sparkles_core::Error::BudgetExceeded(_)),
                ) => return Err(e.into()),
                Err(_) => continue,
            }

            for obj_id in objects {
                ctrl.check()?;
                if seen_iris.contains(&obj_id) {
                    continue;
                }
                if stopped && snap.term_kind(obj_id) == TermKind::Iri {
                    continue;
                }
                let Some((obj, _obj_held)) =
                    decode_import_object_controlled(snap, obj_id, warnings.is_some(), &ctrl)?
                else {
                    continue;
                };
                match obj {
                    ImportObject::NonIri(term) => {
                        if let Some(warnings) = warnings.as_deref_mut() {
                            push_warning(
                                warnings,
                                format_args!("owl:imports {term} is not an IRI and was ignored"),
                                &ctrl,
                            )?;
                        }
                    }
                    ImportObject::Iri(iri) => {
                        insert_id_set_controlled(&mut seen_iris, obj_id, &mut state_held, &ctrl)?;
                        if import_count >= MAX_IMPORTS {
                            if let Some(warnings) = warnings.as_deref_mut() {
                                push_warning(
                                    warnings,
                                    format_args!(
                                        "more than {MAX_IMPORTS} imports: <{iri}> and later ones were not followed"
                                    ),
                                    &ctrl,
                                )?;
                            }
                            stopped = true;
                            continue;
                        }
                        import_count += 1;
                        let mut resolved_id = None;
                        if allows_named(&iri) && has_quads_controlled(snap, obj_id, &ctrl)? {
                            resolved_id = Some(obj_id);
                        } else if !inputs.location_mapping.is_empty()
                            && let Some((location, _loc_held)) =
                                inputs.location_mapping.map_controlled(&iri, &ctrl)?
                            && allows_named(&location)
                            && let Some(loc_id) = lookup_iri_controlled(snap, &location, &ctrl)?
                            && has_quads_controlled(snap, loc_id, &ctrl)?
                        {
                            resolved_id = Some(loc_id);
                        }
                        if let Some(gid) = resolved_id {
                            if insert_id_set_controlled(
                                &mut graph_set,
                                gid,
                                &mut state_held,
                                &ctrl,
                            )? {
                                push_vec_controlled(&mut graphs, gid, &mut graphs_held, &ctrl)?;
                                push_deque_controlled(&mut queue, gid, &mut state_held, &ctrl)?;
                            }
                        } else if let Some(warnings) = warnings.as_deref_mut() {
                            push_warning(
                                warnings,
                                format_args!("owl:imports <{iri}> did not resolve to a graph"),
                                &ctrl,
                            )?;
                        }
                    }
                }
            }
        }
    }

    graphs.sort_unstable();
    graphs.dedup();
    let graphs_held = graphs_held.split(graphs_held.bytes());
    Ok((graphs, graphs_held))
}

/// Which source resolved an import (spec U12, import resolution).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// the dataset's import map
    Map,
    /// the graph named by the import's IRI, or by its location mapping
    Graph,
    /// a vocabulary resource of the resource catalog
    Resource,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Map => "map",
            Via::Graph => "graph",
            Via::Resource => "resource",
        }
    }
}

/// The graphs an import IRI may resolve to besides the graph it names: the dataset's
/// import map, tried first, and the vocabulary resources, tried last.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ImportTargets {
    /// import IRI → the graph the import map names
    pub map: BTreeMap<String, String>,
    /// import IRI → the graphs of the vocabulary resources it matches
    pub resources: BTreeMap<String, Vec<String>>,
}

impl ImportTargets {
    pub fn is_empty(&self) -> bool {
        self.map.is_empty() && self.resources.is_empty()
    }

    /// A digest of the targets, stable across runs: two sets of targets with the same
    /// digest resolve every import alike.
    pub fn digest(&self) -> u64 {
        // FNV-1a over a canonical text, since the std hasher is not stable across builds
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |s: &str| {
            for b in s.bytes().chain([0xff]) {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        };
        for (k, v) in &self.map {
            eat("m");
            eat(k);
            eat(v);
        }
        for (k, v) in &self.resources {
            eat("r");
            eat(k);
            for g in v {
                eat(g);
            }
        }
        h
    }
}

/// One `owl:imports` triple of a graph read, as resolved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportEdge {
    /// the importing graph
    pub from: GraphRef,
    pub iri: String,
    /// the graphs it resolved to (empty: unresolved)
    pub graphs: Vec<String>,
    /// the source that resolved it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    /// whether one of its graphs is the importing graph or imports it
    pub cycle: bool,
}

enum ReadableCheck<'a> {
    ById(&'a dyn Fn(Id) -> bool),
    ByRef(&'a dyn Fn(&GraphRef) -> bool),
}

/// [`resolve_readable`] with `targets`, and the edges of the imports closure. An import
/// resolves to the import map's graph, else to the graph its IRI (or location mapping)
/// names, else to the graphs of the vocabulary resources it matches; a candidate counts
/// only when it holds quads the caller may read. The edges list each `owl:imports`
/// triple found in a graph read, also when an earlier one named the same IRI.
pub fn resolve_closure(
    snap: &Snapshot,
    inputs: &Inputs,
    readable: &dyn Fn(Id) -> bool,
    targets: &ImportTargets,
) -> (Resolved, Vec<ImportEdge>) {
    match closure(
        snap,
        inputs,
        ReadableCheck::ById(readable),
        targets,
        false,
        &|| Ok(()),
    ) {
        Ok(r) => r,
        Err(_) => unreachable!("a lenient resolution does not fail"),
    }
}

/// [`resolve_closure`] for a caller that must not lose input: an index error while
/// testing a graph for quads or scanning its `owl:imports`, or an import object that
/// cannot be decoded, is an error rather than an absent graph or import. `check` runs
/// before each graph is scanned, for a deadline or a cancellation.
pub fn try_resolve_closure(
    snap: &Snapshot,
    inputs: &Inputs,
    readable: &dyn Fn(Id) -> bool,
    targets: &ImportTargets,
    check: &dyn Fn() -> anyhow::Result<()>,
) -> anyhow::Result<(Resolved, Vec<ImportEdge>)> {
    closure(
        snap,
        inputs,
        ReadableCheck::ById(readable),
        targets,
        true,
        check,
    )
}

fn closure(
    snap: &Snapshot,
    inputs: &Inputs,
    readable: ReadableCheck<'_>,
    targets: &ImportTargets,
    strict: bool,
    check: &dyn Fn() -> anyhow::Result<()>,
) -> anyhow::Result<(Resolved, Vec<ImportEdge>)> {
    let present = |g: &GraphRef| -> anyhow::Result<Option<Id>> {
        if strict {
            g.try_id(snap)
                .with_context(|| format!("testing the graph <{}> for quads", g.as_str()))
        } else {
            Ok(g.id(snap))
        }
    };
    let id = |g: &GraphRef| -> anyhow::Result<Option<Id>> {
        match readable {
            ReadableCheck::ByRef(allow) => {
                if !allow(g) {
                    return Ok(None);
                }
                present(g)
            }
            ReadableCheck::ById(allow) => Ok(present(g)?.filter(|i| allow(*i))),
        }
    };
    check()?;
    let mut r = Resolved {
        graphs: inputs.configured(),
        ..Default::default()
    };
    let mut edges: Vec<ImportEdge> = Vec::new();
    for g in &r.graphs {
        check()?;
        if present(g)?.is_none() {
            r.warnings
                .push(format!("the input graph <{}> is empty", g.as_str()));
        }
    }
    if inputs.imports == ImportMode::None {
        return Ok((r, edges));
    }
    let exists = |name: &str| -> anyhow::Result<bool> {
        Ok(id(&GraphRef::Named(name.to_string()))?.is_some())
    };
    // import IRI → what it resolved to, so a second importer gets the same edge
    let mut seen: BTreeMap<String, (Vec<String>, Option<Via>)> = BTreeMap::new();
    let mut stopped = false;
    let mut i = 0;
    while i < r.graphs.len() {
        check()?;
        let from = r.graphs[i].clone();
        let Some(g) = id(&from)? else {
            i += 1;
            continue;
        };
        i += 1;
        check()?;
        let mut warnings = Vec::new();
        for iri in imports_in(snap, g, &mut warnings, strict)? {
            if let Some((graphs, via)) = seen.get(&iri) {
                edges.push(ImportEdge {
                    from: from.clone(),
                    iri,
                    graphs: graphs.clone(),
                    via: *via,
                    cycle: false,
                });
                continue;
            }
            // an import past the bound keeps its edge, unresolved, and the warning says
            // why it was not followed
            if !stopped && r.imports.len() >= MAX_IMPORTS {
                warnings.push(format!(
                    "more than {MAX_IMPORTS} imports: <{iri}> and later ones were not followed"
                ));
                stopped = true;
            }
            if stopped {
                edges.push(ImportEdge {
                    from: from.clone(),
                    iri,
                    graphs: Vec::new(),
                    via: None,
                    cycle: false,
                });
                continue;
            }
            let location = inputs.location_mapping.map(&iri);
            let mapped = match targets.map.get(&iri) {
                Some(g) if exists(g)? => Some(g.clone()),
                _ => None,
            };
            let (graphs, via): (Vec<String>, Option<Via>) = if let Some(g) = mapped {
                (vec![g], Some(Via::Map))
            } else if exists(&iri)? {
                (vec![iri.clone()], Some(Via::Graph))
            } else if location != iri && exists(&location)? {
                (vec![location.clone()], Some(Via::Graph))
            } else {
                let mut found: Vec<String> = Vec::new();
                for g in targets.resources.get(&iri).into_iter().flatten() {
                    if exists(g)? {
                        found.push(g.clone());
                    }
                }
                if found.is_empty() {
                    (found, None)
                } else {
                    (found, Some(Via::Resource))
                }
            };
            if graphs.is_empty() {
                warnings.push(format!("owl:imports <{iri}> did not resolve to a graph"));
            }
            for name in &graphs {
                let gr = GraphRef::Named(name.clone());
                if !r.graphs.contains(&gr) {
                    r.graphs.push(gr);
                }
            }
            r.imports.push(Import {
                location: (location != iri).then_some(location),
                iri: iri.clone(),
                graph: graphs.first().cloned(),
            });
            seen.insert(iri.clone(), (graphs.clone(), via));
            edges.push(ImportEdge {
                from: from.clone(),
                iri,
                graphs,
                via,
                cycle: false,
            });
        }
        r.warnings.extend(warnings);
    }
    mark_cycles(&mut edges);
    Ok((r, edges))
}

/// Mark the edges whose target graphs reach their importing graph.
fn mark_cycles(edges: &mut [ImportEdge]) {
    let mut next: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for e in edges.iter() {
        for g in &e.graphs {
            next.entry(e.from.as_str()).or_default().insert(g.as_str());
        }
    }
    let reaches = |start: &str, goal: &str| {
        let mut stack = vec![start];
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        while let Some(n) = stack.pop() {
            if n == goal {
                return true;
            }
            if seen.insert(n) {
                stack.extend(next.get(n).into_iter().flatten().copied());
            }
        }
        false
    };
    let cycles: Vec<bool> = edges
        .iter()
        .map(|e| e.graphs.iter().any(|g| reaches(g, e.from.as_str())))
        .collect();
    for (e, c) in edges.iter_mut().zip(cycles) {
        e.cycle = c;
    }
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
    opts: &sparkles_core::sparql::QueryOptions,
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
    opts.outbound_budget = Some(sparkles_core::outbound::RequestBudget::new(&opts.outbound));
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
        match sparkles_core::sparql::update::update(store, &u, &opts) {
            Ok(_) => {
                tracing::info!(import = iri, location, "fetched an import");
                out.fetched.push(iri.to_string());
                Ok(true)
            }
            Err(
                e @ (sparkles_core::Error::NotPermitted(_)
                | sparkles_core::Error::BudgetExceeded(_)
                | sparkles_core::Error::Timeout
                | sparkles_core::Error::Cancelled),
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

#[cfg(test)]
mod controlled_budget_tests {
    use super::*;
    use crate::budget::{Budget, BudgetFailure, Held, WorkingWarnings, table_bytes};

    #[test]
    fn insert_id_set_controlled_uses_hash_table_allocation_sizes() {
        // First growth (0 -> 3 insertable entries, 4 buckets) requires table_bytes(3, 8) == 52 bytes.
        let mut set = FxHashSet::default();
        let tight = Budget::new(Some(51));
        let mut held = Held::new(&tight);
        let mut check = || Ok(());
        let ctrl = Control {
            budget: &tight,
            check: std::cell::RefCell::new(&mut check),
        };
        let err = insert_id_set_controlled(&mut set, Id(1), &mut held, &ctrl)
            .expect_err("51-byte budget must refuse 52-byte initial hash table");
        assert!(BudgetFailure::from_anyhow(&err).is_some());
        assert_eq!(set.capacity(), 0);

        let exact = Budget::new(Some(52));
        let mut held = Held::new(&exact);
        let mut check = || Ok(());
        let ctrl = Control {
            budget: &exact,
            check: std::cell::RefCell::new(&mut check),
        };
        assert!(insert_id_set_controlled(&mut set, Id(1), &mut held, &ctrl).unwrap());
        assert!(insert_id_set_controlled(&mut set, Id(2), &mut held, &ctrl).unwrap());
        assert!(insert_id_set_controlled(&mut set, Id(3), &mut held, &ctrl).unwrap());
        assert_eq!(
            held.bytes(),
            table_bytes(set.capacity(), std::mem::size_of::<Id>())
        );
        assert_eq!(held.bytes(), 52);

        // Second growth (cap 3 -> cap 7, 8 buckets == 88 bytes) must admit both old (52) and
        // new (88) allocations simultaneously (140 bytes total) before reserving.
        let mut set_tight2 = FxHashSet::default();
        let tight2 = Budget::new(Some(139));
        let mut held2 = Held::new(&tight2);
        let mut check = || Ok(());
        let ctrl = Control {
            budget: &tight2,
            check: std::cell::RefCell::new(&mut check),
        };
        for id in [Id(1), Id(2), Id(3)] {
            assert!(insert_id_set_controlled(&mut set_tight2, id, &mut held2, &ctrl).unwrap());
        }
        let err = insert_id_set_controlled(&mut set_tight2, Id(4), &mut held2, &ctrl)
            .expect_err("139-byte budget must refuse simultaneous 52 + 88 byte rehash");
        assert!(BudgetFailure::from_anyhow(&err).is_some());
        assert_eq!(set_tight2.capacity(), 3);

        let mut set_exact2 = FxHashSet::default();
        let exact2 = Budget::new(Some(140));
        let mut held2 = Held::new(&exact2);
        let mut check = || Ok(());
        let ctrl = Control {
            budget: &exact2,
            check: std::cell::RefCell::new(&mut check),
        };
        for id in [Id(1), Id(2), Id(3), Id(4)] {
            assert!(insert_id_set_controlled(&mut set_exact2, id, &mut held2, &ctrl).unwrap());
        }
        assert_eq!(
            held2.bytes(),
            table_bytes(set_exact2.capacity(), std::mem::size_of::<Id>())
        );
        assert_eq!(held2.bytes(), 88);
    }

    #[test]
    fn resolve_session_inputs_retains_graph_vector_credit() {
        let store = Store::in_memory(Default::default());
        sparkles_core::sparql::update::update(
            &store,
            "INSERT DATA { \
                <http://ex.org/s> <http://ex.org/p> <http://ex.org/o> . \
                GRAPH <http://ex.org/g1> { <http://ex.org/s> <http://ex.org/p> <http://ex.org/o> } \
                GRAPH <http://ex.org/g2> { <http://ex.org/s> <http://ex.org/p> <http://ex.org/o> } \
                GRAPH <http://ex.org/g3> { <http://ex.org/s> <http://ex.org/p> <http://ex.org/o> } \
                GRAPH <http://ex.org/g4> { <http://ex.org/s> <http://ex.org/p> <http://ex.org/o> } \
            }",
            &Default::default(),
        )
        .unwrap();
        let snap = store.snapshot();
        let inputs = Inputs {
            data_graphs: vec![
                GraphRef::Default,
                GraphRef::Named("http://ex.org/g1".into()),
                GraphRef::Named("http://ex.org/g2".into()),
                GraphRef::Named("http://ex.org/g3".into()),
                GraphRef::Named("http://ex.org/g4".into()),
            ],
            imports: ImportMode::None,
            ..Default::default()
        };
        let budget = Budget::unlimited();
        let mut warnings = WorkingWarnings::new(&budget);
        let mut check = || Ok(());
        let (resolved, graphs_held) = resolve_session_inputs_controlled(
            &snap,
            &inputs,
            None,
            &mut warnings,
            &budget,
            &mut check,
        )
        .unwrap();
        let expected = resolved.capacity() * std::mem::size_of::<Id>();
        assert!(expected >= 5 * std::mem::size_of::<Id>());
        assert_eq!(graphs_held.bytes(), expected);
        assert_eq!(
            budget.used(),
            expected,
            "resolved graph vector lost its budget credit on return (used={}, expected={expected})",
            budget.used()
        );
        drop(resolved);
        drop(graphs_held);
        assert_eq!(budget.used(), 0);
    }
}
