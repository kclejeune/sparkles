//! The reads of the ontology browser (item 616): an entity hierarchy one level at a time
//! ([`Ontologies::hierarchy`], item 607) and what the graphs say about one entity
//! ([`Ontologies::term`]).
//!
//! Neither reads the whole ontology. They scan the store's indexes for the entity and
//! its neighbours, through the caller's view of the graphs read, so a page of GO or
//! ChEBI costs a few hundred index lookups whatever the ontology's size. The term read
//! maps the fragment it collects through the same reverse mapping as
//! [`Ontologies::open`], so its axioms are those a full read gives for the entity.
//!
//! ```
//! use sparkles::Dataset;
//! use sparkles::handles::ontology::OntologyRequest;
//! use sparkles::handles::ontology::browse::{HierarchyRequest, TermRequest};
//!
//! let ds = Dataset::memory();
//! ds.update(
//!     "PREFIX owl: <http://www.w3.org/2002/07/owl#>
//!      PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
//!      INSERT DATA {
//!        <http://ex.org/Animal> a owl:Class ; rdfs:label \"animal\" .
//!        <http://ex.org/Dog> a owl:Class ; rdfs:label \"dog\" ;
//!          rdfs:subClassOf <http://ex.org/Animal> .
//!      }",
//! )?;
//! let req = OntologyRequest::default();
//! let h = ds.ontology().hierarchy(&req, &HierarchyRequest::default(), &Default::default())?;
//! assert_eq!(h.children[0].label.as_deref(), Some("animal"));
//! assert_eq!(h.children[0].children, 1);
//! let t = ds.ontology().term(&req, &TermRequest::new("http://ex.org/Dog"), &Default::default())?;
//! assert_eq!(t.axioms.len(), 3); // the declaration, the label and the subclass axiom
//! # Ok::<_, sparkles::Error>(())
//! ```

use super::{Ontologies, OntologyRequest, check};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::Perm;
use crate::sparql::QueryOptions;
use crate::store::{Chunk, Snapshot, TermKind};
use oxrdf::{NamedOrBlankNode, Quad, Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_owl::Axiom;
use sparkles_owl::read::{self, Mode, ReadOptions};
use sparkles_reasoner::inputs::{GraphRef, Import, Inputs};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const OBO_ID: &str = "http://www.geneontology.org/formats/oboInOwl#id";
/// `IAO_0000700`, "has ontology root term", which OBO ontologies put in their header.
pub const ROOT_TERM: &str = "http://purl.obolibrary.org/obo/IAO_0000700";

/// The children a page holds by default.
pub const DEFAULT_LIMIT: usize = 100;
/// The most children a page holds.
pub const MAX_LIMIT: usize = 1000;
/// The most ancestors an answer lists.
pub const MAX_ANCESTORS: usize = 2000;
/// A node with more children than this pages them in IRI order instead of by label.
pub const MAX_SORTED: usize = 20_000;
/// The most quads the term read collects about the entity itself.
pub const MAX_ABOUT_QUADS: usize = 20_000;
/// The axioms of other entities that mention the entity, by default and at most.
pub const DEFAULT_MENTIONS: usize = 200;
pub const MAX_MENTIONS: usize = 2000;
/// The incoming statements counted per predicate; more are reported as a lower bound.
pub const MAX_INCOMING_COUNT: usize = 10_000;
/// The subjects listed per incoming predicate by default and at most.
pub const DEFAULT_INCOMING: usize = 20;
pub const MAX_INCOMING: usize = 200;
/// The values listed per property of the entity.
pub const MAX_VALUES: usize = 500;

/// The most reifications of one axiom target that the term route reads to find the
/// annotations of the axioms that mention an entity.
const MAX_REIFIERS: usize = 20 * MAX_VALUES;

/// The annotations of reified triples, as (property, value) pairs by the triple's
/// property and object.
type Reifications = FxHashMap<(Id, Id), Vec<(Id, Id)>>;
/// The most IRIs an answer names with their labels.
pub const MAX_NAMES: usize = 4000;

/// The hierarchy a [`HierarchyRequest`] walks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TreeKind {
    /// `rdfs:subClassOf` between named classes
    Class,
    /// `rdfs:subPropertyOf` between object properties
    ObjectProperty,
    DataProperty,
    AnnotationProperty,
    /// `skos:broader` and `skos:narrower`, either way round
    Concept,
}

impl TreeKind {
    pub const ALL: [TreeKind; 5] = [
        TreeKind::Class,
        TreeKind::ObjectProperty,
        TreeKind::DataProperty,
        TreeKind::AnnotationProperty,
        TreeKind::Concept,
    ];

    pub fn name(self) -> &'static str {
        match self {
            TreeKind::Class => "class",
            TreeKind::ObjectProperty => "objectProperty",
            TreeKind::DataProperty => "dataProperty",
            TreeKind::AnnotationProperty => "annotationProperty",
            TreeKind::Concept => "concept",
        }
    }

    pub fn parse(s: &str) -> Option<TreeKind> {
        TreeKind::ALL.into_iter().find(|k| k.name() == s)
    }

    /// The types whose instances are roots of this hierarchy.
    fn types(self) -> &'static [&'static str] {
        match self {
            TreeKind::Class => &[
                "http://www.w3.org/2002/07/owl#Class",
                "http://www.w3.org/2000/01/rdf-schema#Class",
            ],
            TreeKind::ObjectProperty => &["http://www.w3.org/2002/07/owl#ObjectProperty"],
            TreeKind::DataProperty => &["http://www.w3.org/2002/07/owl#DatatypeProperty"],
            TreeKind::AnnotationProperty => &["http://www.w3.org/2002/07/owl#AnnotationProperty"],
            TreeKind::Concept => &["http://www.w3.org/2004/02/skos/core#Concept"],
        }
    }

    /// The predicate from a child up to its parent.
    fn up(self) -> &'static str {
        match self {
            TreeKind::Class => "http://www.w3.org/2000/01/rdf-schema#subClassOf",
            TreeKind::Concept => "http://www.w3.org/2004/02/skos/core#broader",
            _ => "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
        }
    }

    /// The predicate from a parent down to its child, when the vocabulary has one.
    fn down(self) -> Option<&'static str> {
        match self {
            TreeKind::Concept => Some("http://www.w3.org/2004/02/skos/core#narrower"),
            _ => None,
        }
    }

    /// Parents that do not count, such as `owl:Thing` above a class.
    fn ignored(self) -> &'static [&'static str] {
        match self {
            TreeKind::Class => &[
                "http://www.w3.org/2002/07/owl#Thing",
                "http://www.w3.org/2000/01/rdf-schema#Resource",
            ],
            TreeKind::ObjectProperty => &["http://www.w3.org/2002/07/owl#topObjectProperty"],
            TreeKind::DataProperty => &["http://www.w3.org/2002/07/owl#topDataProperty"],
            _ => &[],
        }
    }
}

/// What [`Ontologies::hierarchy`] answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HierarchyRequest {
    pub kind: TreeKind,
    /// the node whose children to list; `None` lists the roots
    pub iri: Option<String>,
    /// also list the node's ancestors up to the roots
    pub ancestors: bool,
    /// at most [`MAX_LIMIT`]
    pub limit: usize,
    pub offset: usize,
    /// keep concepts of this scheme only, and start the roots at its top concepts
    pub scheme: Option<String>,
    /// list deprecated children (true by default; they come after the others)
    pub deprecated: bool,
    /// the caller's languages for labels, most preferred first
    pub languages: Vec<String>,
}

impl Default for HierarchyRequest {
    fn default() -> HierarchyRequest {
        HierarchyRequest {
            kind: TreeKind::Class,
            iri: None,
            ancestors: false,
            limit: DEFAULT_LIMIT,
            offset: 0,
            scheme: None,
            deprecated: true,
            languages: Vec::new(),
        }
    }
}

/// One entity of a hierarchy or of a term answer, with what a link to it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub iri: String,
    /// its `rdfs:label`, `skos:prefLabel` or SKOS-XL preferred label in the best language
    pub label: Option<String>,
    pub label_lang: Option<String>,
    /// its `oboInOwl:id`, `skos:notation`, prefixed name or local name
    pub short_form: Option<String>,
    /// its kinds, as the entity search names them (`class`, `concept`, …)
    pub kinds: Vec<&'static str>,
    pub deprecated: bool,
}

/// A node of a hierarchy: its name and the numbers of its direct children and parents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub name: Name,
    pub children: usize,
    pub parents: usize,
}

impl std::ops::Deref for Node {
    type Target = Name;
    fn deref(&self) -> &Name {
        &self.name
    }
}

/// Where the roots of a hierarchy came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootsFrom {
    /// the ontology header's root terms (`IAO_0000700`)
    Ontology,
    /// the top concepts of the scheme, with its concepts that have no broader one
    Scheme,
    /// the entities of the kind without a parent in the graphs read
    Computed,
}

impl RootsFrom {
    pub fn name(self) -> &'static str {
        match self {
            RootsFrom::Ontology => "ontology",
            RootsFrom::Scheme => "scheme",
            RootsFrom::Computed => "computed",
        }
    }
}

/// The answer of [`Ontologies::hierarchy`].
#[derive(Clone, Debug)]
pub struct Hierarchy {
    pub commit: u64,
    pub kind: TreeKind,
    /// the node asked for; `None` for the roots
    pub node: Option<Node>,
    /// a page of the node's children, or of the roots
    pub children: Vec<Node>,
    /// the number of children or roots
    pub total: usize,
    pub offset: usize,
    /// whether the children are in label order (false: IRI order, for a very wide node)
    pub sorted: bool,
    pub roots_from: Option<RootsFrom>,
    /// the deprecated entities without a parent that the computed roots leave out when
    /// `deprecated` is false
    pub deprecated_roots: usize,
    /// the ancestors and the edges between them and the node, child first
    pub ancestors: Option<Ancestors>,
    pub graphs: Vec<GraphRef>,
    pub imports: Vec<Import>,
    pub warnings: Vec<String>,
}

/// The ancestors of a node, as a graph that may have several paths to the roots.
#[derive(Clone, Debug, Default)]
pub struct Ancestors {
    pub nodes: Vec<Node>,
    /// `(child, parent)` IRIs
    pub edges: Vec<(String, String)>,
    pub truncated: bool,
}

/// What [`Ontologies::term`] reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermRequest {
    pub iri: String,
    /// the axioms of other entities that mention it, at most [`MAX_MENTIONS`]
    pub mentions: usize,
    /// the subjects listed per incoming predicate, at most [`MAX_INCOMING`]
    pub incoming: usize,
    pub languages: Vec<String>,
}

impl TermRequest {
    pub fn new(iri: impl Into<String>) -> TermRequest {
        TermRequest {
            iri: iri.into(),
            mentions: DEFAULT_MENTIONS,
            incoming: DEFAULT_INCOMING,
            languages: Vec::new(),
        }
    }
}

/// A value of a statement or an annotation: an IRI or a literal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Iri(String),
    Literal(oxrdf::Literal),
}

/// One triple with the entity as subject, with the annotations its reification gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    pub property: String,
    pub value: Value,
    pub graph: u32,
    pub annotations: Vec<(String, Value)>,
}

/// The statements of one predicate whose object is the entity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    pub property: String,
    /// the number of statements, at most [`MAX_INCOMING_COUNT`]
    pub count: usize,
    /// whether more statements exist than `count`
    pub more: bool,
    /// the first subjects, in index order
    pub subjects: Vec<String>,
}

/// The answer of [`Ontologies::term`].
#[derive(Clone, Debug)]
pub struct TermPage {
    pub commit: u64,
    pub name: Name,
    /// the IRI's types in the graphs read
    pub types: Vec<String>,
    pub statements: Vec<Statement>,
    /// whether a property had more values than [`MAX_VALUES`], or the triples about the
    /// entity or the blank nodes they reach passed [`MAX_ABOUT_QUADS`]
    pub statements_truncated: bool,
    /// the literal forms of the entity's SKOS-XL labels: `property` is
    /// `skosxl:prefLabel`, `altLabel` or `hiddenLabel` and `value` the label's
    /// `skosxl:literalForm`
    pub xl_labels: Vec<Statement>,
    /// the axioms the triples about the entity map to, with their graphs
    pub axioms: Vec<(Axiom, u32)>,
    /// the axioms of other entities that mention it, with their graphs
    pub mentions: Vec<(Axiom, u32)>,
    /// the number of other entities' axiom roots that mention it, at most 5,000
    pub mentions_total: usize,
    pub mentions_truncated: bool,
    pub incoming: Vec<Incoming>,
    /// the statements that use the entity as their predicate, with `property` set to
    /// the entity, when there are any
    pub usage: Option<Incoming>,
    /// the ontology IRI of each graph read, when the graph declares one
    pub ontologies: Vec<Option<String>>,
    /// the IRIs the answer uses, with their labels and kinds
    pub names: BTreeMap<String, Name>,
    pub graphs: Vec<GraphRef>,
    pub imports: Vec<Import>,
    pub warnings: Vec<String>,
    /// the quads about the entity, for downloads: its triples, the blank nodes they
    /// reach and the reifications of its annotated triples
    pub quads: Vec<Quad>,
}

impl TermPage {
    /// The name of graph `n` of the read.
    pub fn graph_name(&self, n: u32) -> &str {
        self.graphs.get(n as usize).map_or("", |g| g.as_str())
    }
}

/// The view of the graphs a request reads: the snapshot through the caller's grants and
/// the ids of the readable graphs, numbered in the order of the request.
pub(super) struct Scope {
    snap: Arc<Snapshot>,
    graphs: FxHashMap<Id, u32>,
    refs: Vec<GraphRef>,
    imports: Vec<Import>,
    warnings: Vec<String>,
    languages: Vec<String>,
    prefixes: BTreeMap<String, String>,
    ids: FxHashMap<&'static str, Option<Id>>,
    opts: QueryOptions,
    /// how the reverse mapping reads the fragments
    mode: Mode,
}

impl Ontologies {
    fn browse_scope(
        &self,
        req: &OntologyRequest,
        languages: &[String],
        opts: &QueryOptions,
    ) -> Result<Scope> {
        req.validate()?;
        if req.inferences {
            return Err(Error::invalid(
                "the browser reads the asserted triples and does not take inferences",
            ));
        }
        let (snap, opts) = self.state(req, opts)?;
        let access = opts.graphs.as_ref().map(|a| a.bound(&snap));
        let view = match &access {
            Some(a) => a.masked(&snap)?,
            None => snap.clone(),
        };
        let readable = |g: Id| access.as_ref().is_none_or(|a| a.readable_id(&snap, g));
        let inputs = Inputs {
            data_graphs: req.graphs(),
            ontology_graphs: Vec::new(),
            imports: req.imports,
            location_mapping: Default::default(),
        };
        let resolved = sparkles_reasoner::inputs::resolve_readable(&view, &inputs, &readable);
        let mut graphs = FxHashMap::default();
        for (n, g) in resolved.graphs.iter().enumerate() {
            if let Some(id) = g.id(&view).filter(|id| readable(*id)) {
                graphs.entry(id).or_insert(n as u32);
            }
        }
        Ok(Scope {
            snap: view,
            graphs,
            refs: resolved.graphs,
            imports: resolved.imports,
            warnings: resolved.warnings,
            languages: languages.iter().map(|l| l.to_ascii_lowercase()).collect(),
            prefixes: self.ds.prefixes(),
            ids: FxHashMap::default(),
            opts,
            mode: req.mode,
        })
    }

    /// One level of the hierarchy of `h.kind` in the ontology `req` names: the children
    /// of `h.iri`, or the roots, a page at a time, with the numbers of their own children
    /// and parents, and the node's ancestors when `h.ancestors` is set.
    pub fn hierarchy(
        &self,
        req: &OntologyRequest,
        h: &HierarchyRequest,
        opts: &QueryOptions,
    ) -> Result<Hierarchy> {
        if h.limit == 0 || h.limit > MAX_LIMIT {
            return Err(Error::invalid(format!(
                "limit must be an integer from 1 to {MAX_LIMIT}"
            )));
        }
        let mut s = self.browse_scope(req, &h.languages, opts)?;
        let scheme = match &h.scheme {
            None => None,
            Some(iri) => Some(
                s.lookup(iri)
                    .filter(|id| s.mentioned(*id).unwrap_or(false))
                    .ok_or_else(|| {
                        Error::NotFound(format!(
                            "the graphs read do not mention the scheme <{iri}>"
                        ))
                    })?,
            ),
        };
        let tree = Tree {
            kind: h.kind,
            scheme,
        };
        let mut roots_from = None;
        let mut deprecated_roots = 0;
        let (node, mut members) = match &h.iri {
            Some(iri) => {
                let id = s.lookup(iri).filter(|id| s.mentioned(*id).unwrap_or(false));
                let Some(id) = id else {
                    return Err(Error::NotFound(format!(
                        "the graphs read do not mention <{iri}>"
                    )));
                };
                let node = s.node(&tree, id)?;
                (Some(node), tree.children(&mut s, id)?)
            }
            None => {
                let (from, roots) = tree.roots(&mut s)?;
                roots_from = Some(from);
                (None, roots)
            }
        };
        if !h.deprecated || roots_from == Some(RootsFrom::Computed) {
            let before = members.len();
            if !h.deprecated {
                members.retain(|id| !s.deprecated(*id).unwrap_or(false));
            }
            if roots_from == Some(RootsFrom::Computed) && !h.deprecated {
                deprecated_roots = before - members.len();
            }
        }
        let total = members.len();
        let sorted = total <= MAX_SORTED;
        if sorted {
            let mut keyed = Vec::with_capacity(members.len());
            for id in members {
                check(&s.opts)?;
                let dep = s.deprecated(id)?;
                let label = s.label(id)?.map(|(l, _)| l.to_lowercase());
                keyed.push(((dep, label.is_none(), label, s.iri(id)), id));
            }
            keyed.sort();
            members = keyed.into_iter().map(|(_, id)| id).collect();
        } else {
            // too many to label: IRI order, which does not depend on the ids a
            // compaction may renumber
            let mut keyed = Vec::with_capacity(members.len());
            for (n, id) in members.into_iter().enumerate() {
                if n.is_multiple_of(4096) {
                    check(&s.opts)?;
                }
                keyed.push((s.iri(id), id));
            }
            keyed.sort();
            members = keyed.into_iter().map(|(_, id)| id).collect();
        }
        let mut children = Vec::new();
        for id in members.iter().skip(h.offset).take(h.limit) {
            check(&s.opts)?;
            children.push(s.node(&tree, *id)?);
        }
        let ancestors = match (&h.iri, h.ancestors) {
            (Some(iri), true) => {
                let id = s.lookup(iri).expect("looked up above");
                Some(tree.ancestors(&mut s, id)?)
            }
            _ => None,
        };
        Ok(Hierarchy {
            commit: s.snap.commit,
            kind: h.kind,
            node,
            children,
            total,
            offset: h.offset,
            sorted,
            roots_from,
            deprecated_roots,
            ancestors,
            graphs: s.refs,
            imports: s.imports,
            warnings: s.warnings,
        })
    }

    /// What the graphs of `req` say about the entity `t.iri`: its statements with their
    /// annotations, the axioms they map to, the axioms of other entities that mention
    /// it, the statements that point at it, and the labels of every IRI used.
    pub fn term(
        &self,
        req: &OntologyRequest,
        t: &TermRequest,
        opts: &QueryOptions,
    ) -> Result<TermPage> {
        if t.mentions > MAX_MENTIONS {
            return Err(Error::invalid(format!(
                "mentions must be an integer from 0 to {MAX_MENTIONS}"
            )));
        }
        if t.incoming > MAX_INCOMING {
            return Err(Error::invalid(format!(
                "incoming must be an integer from 0 to {MAX_INCOMING}"
            )));
        }
        let iri = t.iri.trim_start_matches('<').trim_end_matches('>');
        let mut s = self.browse_scope(req, &t.languages, opts)?;
        let Some(x) = s.lookup(iri).filter(|id| s.mentioned(*id).unwrap_or(false)) else {
            return Err(Error::NotFound(format!(
                "the graphs read do not mention <{iri}>"
            )));
        };
        let mut names: BTreeSet<Id> = BTreeSet::new();
        names.insert(x);

        // the triples about the entity, the blank nodes they reach, and the
        // reifications of its annotated triples
        let mut about = Collector::default();
        let own = s.quads(Perm::Spo, &[x.0], MAX_ABOUT_QUADS)?;
        let mut truncated = own.len() >= MAX_ABOUT_QUADS;
        for q in &own {
            about.add(*q);
        }
        let mut complete = s.closure(&mut about, own.iter().map(|q| q[2]))?;
        let mut reifiers = Vec::new();
        if let Some(src) = s.id(OWL_ANNOTATED_SOURCE) {
            for q in s.quads(Perm::Pos, &[src.0, x.0], MAX_ABOUT_QUADS)? {
                reifiers.push(q[0]);
            }
        }
        for r in &reifiers {
            let qs = s.quads(Perm::Spo, &[r.0], MAX_ABOUT_QUADS)?;
            for q in &qs {
                about.add(*q);
            }
            complete &= s.closure(&mut about, qs.iter().map(|q| q[2]))?;
        }
        if !complete {
            truncated = true;
        }

        // statements: the entity's triples with a named or literal object, and the
        // annotations of their reifications
        let reif = s.reifications(&reifiers)?;
        let mut statements = Vec::new();
        let mut per_property: FxHashMap<Id, usize> = FxHashMap::default();
        let mut types = Vec::new();
        let rdf_type = s.id(RDF_TYPE);
        for q in &own {
            if s.snap.term_kind(q[2]) == TermKind::BNode {
                continue;
            }
            if Some(q[1]) == rdf_type {
                types.push(s.iri(q[2]));
            }
            let n = per_property.entry(q[1]).or_default();
            *n += 1;
            if *n > MAX_VALUES {
                truncated = true;
                continue;
            }
            let Some(value) = s.value(q[2]) else { continue };
            names.insert(q[1]);
            if matches!(value, Value::Iri(_)) {
                names.insert(q[2]);
            }
            let annotations = reif
                .get(&(q[1], q[2]))
                .map(|list| {
                    list.iter()
                        .filter_map(|(p, o)| {
                            names.insert(*p);
                            Some((s.iri(*p), s.value(*o)?))
                        })
                        .collect()
                })
                .unwrap_or_default();
            statements.push(Statement {
                property: s.iri(q[1]),
                value,
                graph: s.graphs[&q[3]],
                annotations,
            });
        }
        types.sort();
        types.dedup();

        // the literal forms of its SKOS-XL labels, which are resources of their own
        let mut xl_labels = Vec::new();
        if let Some(form) = s.id(SKOSXL_LITERAL_FORM) {
            let props: Vec<Id> = SKOSXL_LABELS.iter().filter_map(|p| s.id(p)).collect();
            for q in &own {
                if !props.contains(&q[1]) {
                    continue;
                }
                if xl_labels.len() >= MAX_VALUES {
                    truncated = true;
                    break;
                }
                for f in s.objects(q[2], form, 4)? {
                    if let Some(value @ Value::Literal(_)) = s.value(f) {
                        names.insert(q[1]);
                        xl_labels.push(Statement {
                            property: s.iri(q[1]),
                            value,
                            graph: s.graphs[&q[3]],
                            annotations: Vec::new(),
                        });
                    }
                }
            }
        }

        // the axioms of other entities that mention it: walk up from the blank nodes
        // that hold it to the named subject or the root of each axiom
        check(&s.opts)?;
        let (roots, mentions_total, mentions_truncated) = s.mention_roots(x, t.mentions)?;
        let mut mention = Collector::default();
        let mut mentions_cut = false;
        for root in &roots {
            match root {
                Root::Named(q) => {
                    let q: [Id; 4] = *q;
                    mention.add(q);
                    mentions_cut |= !s.closure(&mut mention, [q[2]])?;
                    // the axiom's annotations, reified with the same source, property
                    // and target
                    let (reifiers, complete) = s.reifiers_of(q[0], q[1], q[2])?;
                    mentions_cut |= !complete;
                    for r in reifiers {
                        let qs = s.quads(Perm::Spo, &[r.0], MAX_ABOUT_QUADS)?;
                        mention.add_all(&qs);
                        mentions_cut |= !s.closure(&mut mention, qs.iter().map(|q| q[2]))?;
                    }
                }
                Root::Blank(b) => {
                    let qs = s.quads(Perm::Spo, &[b.0], MAX_ABOUT_QUADS)?;
                    mention.add_all(&qs);
                    mentions_cut |= !s.closure(&mut mention, qs.iter().map(|q| q[2]))?;
                }
            }
        }
        let mentions_truncated = mentions_truncated || mentions_cut;

        // incoming statements from named subjects, by predicate
        check(&s.opts)?;
        let incoming = s.incoming(x, t.incoming)?;
        for i in &incoming {
            for subj in &i.subjects {
                if let Some(id) = s.lookup(subj) {
                    names.insert(id);
                }
            }
            if let Some(id) = s.lookup(&i.property) {
                names.insert(id);
            }
        }

        // statements that use it as their predicate
        let used = s.quads(Perm::Pso, &[x.0], MAX_INCOMING_COUNT + 1)?;
        let usage = (!used.is_empty()).then(|| {
            // the distinct named subjects, in index order, of the statements counted
            let mut distinct: FxHashSet<Id> = FxHashSet::default();
            let mut subjects: Vec<Id> = Vec::new();
            for q in used.iter().take(MAX_INCOMING_COUNT) {
                if s.snap.term_kind(q[0]) == TermKind::Iri
                    && distinct.insert(q[0])
                    && subjects.len() < t.incoming
                {
                    subjects.push(q[0]);
                }
            }
            for id in &subjects {
                names.insert(*id);
            }
            Incoming {
                property: iri.to_string(),
                count: distinct.len(),
                more: used.len() > MAX_INCOMING_COUNT,
                subjects: subjects.iter().map(|id| s.iri(*id)).collect(),
            }
        });

        // map both fragments through the reverse mapping, with the types of every IRI
        // they use
        check(&s.opts)?;
        let axioms = s.map(&about.quads)?;
        let mentions: Vec<(Axiom, u32)> = s
            .map(&mention.quads)?
            .into_iter()
            .filter(|(a, _)| a.mentions(iri))
            .collect();
        for (a, _) in axioms.iter().chain(&mentions) {
            for e in a.signature() {
                if names.len() >= MAX_NAMES {
                    break;
                }
                if let Some(id) = s.lookup(e.iri().as_str()) {
                    names.insert(id);
                }
            }
        }
        check(&s.opts)?;
        let mut name_map = BTreeMap::new();
        for id in names {
            if s.snap.term_kind(id) != TermKind::Iri {
                continue;
            }
            let n = s.name(id)?;
            name_map.insert(n.iri.clone(), n);
        }
        let name = name_map
            .get(iri)
            .cloned()
            .expect("the entity's own name is listed");
        let ontologies = s.ontologies()?;
        let quads = about
            .quads
            .iter()
            .filter_map(|q| s.snap.quad_to_terms(q))
            .collect();
        let mut warnings = s.warnings.clone();
        if truncated {
            warnings.push(format!(
                "the entity has more than {MAX_VALUES} values of a property or {MAX_ABOUT_QUADS} triples, and the first ones are read"
            ));
        }
        Ok(TermPage {
            commit: s.snap.commit,
            name,
            types,
            statements,
            statements_truncated: truncated,
            xl_labels,
            axioms,
            mentions,
            mentions_total,
            mentions_truncated,
            incoming,
            usage,
            ontologies,
            names: name_map,
            graphs: s.refs,
            imports: s.imports,
            warnings,
            quads,
        })
    }
}

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const SKOSXL_LITERAL_FORM: &str = "http://www.w3.org/2008/05/skos-xl#literalForm";
const SKOSXL_LABELS: [&str; 3] = [
    "http://www.w3.org/2008/05/skos-xl#prefLabel",
    "http://www.w3.org/2008/05/skos-xl#altLabel",
    "http://www.w3.org/2008/05/skos-xl#hiddenLabel",
];
const OWL_ANNOTATED_SOURCE: &str = "http://www.w3.org/2002/07/owl#annotatedSource";
const OWL_ANNOTATED_PROPERTY: &str = "http://www.w3.org/2002/07/owl#annotatedProperty";
const OWL_ANNOTATED_TARGET: &str = "http://www.w3.org/2002/07/owl#annotatedTarget";
const OWL_DEPRECATED: &str = "http://www.w3.org/2002/07/owl#deprecated";
const OWL_ONTOLOGY: &str = "http://www.w3.org/2002/07/owl#Ontology";

/// Where an axiom of another entity that mentions the entity starts.
enum Root {
    /// the triple of a named subject whose object is the blank node that holds it
    Named([Id; 4]),
    /// a blank node that no triple points at, such as the subclass of a general axiom
    Blank(Id),
}

/// Quads collected once each.
#[derive(Default)]
struct Collector {
    quads: Vec<[Id; 4]>,
    seen: FxHashSet<[Id; 4]>,
}

impl Collector {
    fn add(&mut self, q: [Id; 4]) {
        if self.seen.insert(q) {
            self.quads.push(q);
        }
    }

    fn add_all(&mut self, qs: &[[Id; 4]]) {
        for q in qs {
            self.add(*q);
        }
    }
}

/// The hierarchy of one kind, in one scheme for concepts.
struct Tree {
    kind: TreeKind,
    scheme: Option<Id>,
}

impl Tree {
    /// Whether `id` belongs to the scheme, when the tree has one.
    fn in_scheme(&self, s: &mut Scope, id: Id) -> Result<bool> {
        let Some(scheme) = self.scheme else {
            return Ok(true);
        };
        for p in [
            "http://www.w3.org/2004/02/skos/core#inScheme",
            "http://www.w3.org/2004/02/skos/core#topConceptOf",
        ] {
            if let Some(p) = s.id(p)
                && s.exists(&[id, p, scheme])?
            {
                return Ok(true);
            }
        }
        if let Some(p) = s.id("http://www.w3.org/2004/02/skos/core#hasTopConcept") {
            return s.exists(&[scheme, p, id]);
        }
        Ok(false)
    }

    /// The direct parents of `id`: named, readable and not ignored.
    fn parents(&self, s: &mut Scope, id: Id) -> Result<Vec<Id>> {
        let mut out = BTreeSet::new();
        if let Some(up) = s.id(self.kind.up()) {
            for q in s.quads(Perm::Spo, &[id.0, up.0], usize::MAX)? {
                out.insert(q[2]);
            }
        }
        if let Some(down) = self.kind.down().and_then(|d| s.id(d)) {
            for q in s.quads(Perm::Pos, &[down.0, id.0], usize::MAX)? {
                out.insert(q[0]);
            }
        }
        self.keep(s, out, id)
    }

    /// The direct children of `id`.
    fn children(&self, s: &mut Scope, id: Id) -> Result<Vec<Id>> {
        let mut out = BTreeSet::new();
        if let Some(up) = s.id(self.kind.up()) {
            for q in s.quads(Perm::Pos, &[up.0, id.0], usize::MAX)? {
                out.insert(q[0]);
            }
        }
        if let Some(down) = self.kind.down().and_then(|d| s.id(d)) {
            for q in s.quads(Perm::Spo, &[id.0, down.0], usize::MAX)? {
                out.insert(q[2]);
            }
        }
        self.keep(s, out, id)
    }

    fn keep(&self, s: &mut Scope, ids: BTreeSet<Id>, me: Id) -> Result<Vec<Id>> {
        let ignored: Vec<Id> = self.kind.ignored().iter().filter_map(|i| s.id(i)).collect();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if id == me || ignored.contains(&id) || s.snap.term_kind(id) != TermKind::Iri {
                continue;
            }
            if self.scheme.is_some() && !self.in_scheme(s, id)? {
                continue;
            }
            out.push(id);
        }
        Ok(out)
    }

    /// The roots: the ontology header's root terms for classes when it names some, the
    /// scheme's top concepts with its concepts without a broader one, or the entities
    /// of the kind without a parent.
    fn roots(&self, s: &mut Scope) -> Result<(RootsFrom, Vec<Id>)> {
        if self.kind == TreeKind::Class
            && let (Some(root), Some(t), Some(onto)) =
                (s.id(ROOT_TERM), s.id(RDF_TYPE), s.id(OWL_ONTOLOGY))
        {
            let mut out = BTreeSet::new();
            for q in s.quads(Perm::Pos, &[root.0], usize::MAX)? {
                if s.exists(&[q[0], t, onto])? && s.snap.term_kind(q[2]) == TermKind::Iri {
                    out.insert(q[2]);
                }
            }
            if !out.is_empty() {
                return Ok((RootsFrom::Ontology, out.into_iter().collect()));
            }
        }
        let mut out = BTreeSet::new();
        let mut from = RootsFrom::Computed;
        if let Some(scheme) = self.scheme {
            from = RootsFrom::Scheme;
            if let Some(p) = s.id("http://www.w3.org/2004/02/skos/core#hasTopConcept") {
                for q in s.quads(Perm::Spo, &[scheme.0, p.0], usize::MAX)? {
                    out.insert(q[2]);
                }
            }
            if let Some(p) = s.id("http://www.w3.org/2004/02/skos/core#topConceptOf") {
                for q in s.quads(Perm::Pos, &[p.0, scheme.0], usize::MAX)? {
                    out.insert(q[0]);
                }
            }
        }
        let Some(t) = s.id(RDF_TYPE) else {
            return Ok((from, out.into_iter().collect()));
        };
        let mut candidates = BTreeSet::new();
        for ty in self.kind.types() {
            if let Some(ty) = s.id(ty) {
                for q in s.quads(Perm::Pos, &[t.0, ty.0], usize::MAX)? {
                    candidates.insert(q[0]);
                }
            }
        }
        // a computed root has no parent; scan the parent links once instead of asking
        // per candidate when that is cheaper
        // in a scheme, a parent counts only when it is in the scheme, which the bulk scan
        // cannot tell, so each candidate of the scheme is asked
        let with_parent = if self.scheme.is_some() {
            FxHashSet::default()
        } else {
            self.with_parents(s)?
        };
        let ignored: Vec<&str> = self.kind.ignored().to_vec();
        for c in candidates {
            if s.snap.term_kind(c) != TermKind::Iri || out.contains(&c) {
                continue;
            }
            if ignored.iter().any(|i| s.id(i) == Some(c)) {
                continue;
            }
            if with_parent.contains(&c) {
                continue;
            }
            if self.scheme.is_some() && (!self.in_scheme(s, c)? || !self.parents(s, c)?.is_empty())
            {
                continue;
            }
            out.insert(c);
        }
        Ok((from, out.into_iter().collect()))
    }

    /// The entities with a parent of this tree's links in the graphs read.
    fn with_parents(&self, s: &mut Scope) -> Result<FxHashSet<Id>> {
        let ignored: FxHashSet<Id> = self.kind.ignored().iter().filter_map(|i| s.id(i)).collect();
        let mut out = FxHashSet::default();
        if let Some(up) = s.id(self.kind.up()) {
            let graphs = &s.graphs;
            let snap = &s.snap;
            let opts = &s.opts;
            let mut n = 0usize;
            snap.scan(Perm::Pso, &[up.0], |c| {
                let mut each = |k: [Id; 4]| -> Result<()> {
                    n += 1;
                    if n.is_multiple_of(4096) {
                        check(opts)?;
                    }
                    if graphs.contains_key(&k[3])
                        && !ignored.contains(&k[2])
                        && snap.term_kind(k[2]) == TermKind::Iri
                        && k[0] != k[2]
                    {
                        out.insert(k[0]);
                    }
                    Ok(())
                };
                match c {
                    Chunk::Block(b, st, e) => {
                        for i in st..e {
                            each(Perm::Pso.to_quad(&b.key(i)))?;
                        }
                    }
                    Chunk::Row(k) => each(Perm::Pso.to_quad(&k))?,
                }
                Ok(true)
            })?;
        }
        if let Some(down) = self.kind.down().and_then(|d| s.id(d)) {
            for q in s.quads(Perm::Pso, &[down.0], usize::MAX)? {
                if s.snap.term_kind(q[0]) == TermKind::Iri && q[0] != q[2] {
                    out.insert(q[2]);
                }
            }
        }
        Ok(out)
    }

    /// The ancestors of `id` up to the roots, breadth first, at most [`MAX_ANCESTORS`].
    fn ancestors(&self, s: &mut Scope, id: Id) -> Result<Ancestors> {
        let mut seen: FxHashSet<Id> = FxHashSet::default();
        seen.insert(id);
        let mut queue = std::collections::VecDeque::from([id]);
        let mut order = Vec::new();
        let mut edges = Vec::new();
        let mut truncated = false;
        while let Some(c) = queue.pop_front() {
            check(&s.opts)?;
            for p in self.parents(s, c)? {
                edges.push((s.iri(c), s.iri(p)));
                if seen.insert(p) {
                    if order.len() >= MAX_ANCESTORS {
                        truncated = true;
                        continue;
                    }
                    order.push(p);
                    queue.push_back(p);
                }
            }
        }
        let mut nodes = Vec::with_capacity(order.len());
        for p in order {
            nodes.push(s.node(self, p)?);
        }
        Ok(Ancestors {
            nodes,
            edges,
            truncated,
        })
    }
}

impl Scope {
    /// The id of a vocabulary IRI, looked up once.
    fn id(&mut self, iri: &'static str) -> Option<Id> {
        let snap = &self.snap;
        *self.ids.entry(iri).or_insert_with(|| snap.lookup_iri(iri))
    }

    fn lookup(&self, iri: &str) -> Option<Id> {
        self.snap.lookup_iri(iri)
    }

    fn iri(&self, id: Id) -> String {
        match self.snap.term(id) {
            Some(Term::NamedNode(n)) => n.into_string(),
            Some(t) => t.to_string(),
            None => String::new(),
        }
    }

    fn value(&self, id: Id) -> Option<Value> {
        match self.snap.term(id)? {
            Term::NamedNode(n) => Some(Value::Iri(n.into_string())),
            Term::Literal(l) => Some(Value::Literal(l)),
            _ => None,
        }
    }

    /// The quads of the graphs read whose key in `perm` starts with `prefix`, at most
    /// `limit`.
    fn quads(&self, perm: Perm, prefix: &[u64], limit: usize) -> Result<Vec<[Id; 4]>> {
        let mut out = Vec::new();
        let mut n = 0usize;
        let graphs = &self.graphs;
        let opts = &self.opts;
        self.snap.scan(perm, prefix, |c| {
            let mut each = |q: [Id; 4], out: &mut Vec<[Id; 4]>| -> Result<bool> {
                n += 1;
                if n.is_multiple_of(4096) {
                    check(opts)?;
                }
                if graphs.contains_key(&q[3]) {
                    out.push(q);
                }
                Ok(out.len() < limit)
            };
            match c {
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        if !each(perm.to_quad(&b.key(i)), &mut out)? {
                            return Ok(false);
                        }
                    }
                }
                Chunk::Row(k) => {
                    if !each(perm.to_quad(&k), &mut out)? {
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        })?;
        Ok(out)
    }

    /// Whether the triple `s p o` is in a graph read.
    fn exists(&self, spo: &[Id; 3]) -> Result<bool> {
        Ok(!self
            .quads(Perm::Spo, &[spo[0].0, spo[1].0, spo[2].0], 1)?
            .is_empty())
    }

    /// Whether a graph read uses `id` as a subject, a predicate or an object.
    fn mentioned(&self, id: Id) -> Result<bool> {
        Ok(!self.quads(Perm::Spo, &[id.0], 1)?.is_empty()
            || !self.quads(Perm::Osp, &[id.0], 1)?.is_empty()
            || !self.quads(Perm::Pso, &[id.0], 1)?.is_empty())
    }

    /// The objects of `s p` in the graphs read.
    fn objects(&self, s: Id, p: Id, limit: usize) -> Result<Vec<Id>> {
        Ok(self
            .quads(Perm::Spo, &[s.0, p.0], limit)?
            .into_iter()
            .map(|q| q[2])
            .collect())
    }

    /// Add the blank nodes `from` reaches, with their triples, to `c`. Returns false
    /// when the collector reached its cap before the closure was complete.
    fn closure(&self, c: &mut Collector, from: impl IntoIterator<Item = Id>) -> Result<bool> {
        let mut stack: Vec<Id> = from
            .into_iter()
            .filter(|o| self.snap.term_kind(*o) == TermKind::BNode)
            .collect();
        let mut seen: FxHashSet<Id> = stack.iter().copied().collect();
        while let Some(b) = stack.pop() {
            if c.quads.len() >= MAX_ABOUT_QUADS * 2 {
                return Ok(false);
            }
            let qs = self.quads(Perm::Spo, &[b.0], MAX_ABOUT_QUADS)?;
            if qs.len() >= MAX_ABOUT_QUADS {
                c.add_all(&qs);
                return Ok(false);
            }
            for q in qs {
                c.add(q);
                if self.snap.term_kind(q[2]) == TermKind::BNode && seen.insert(q[2]) {
                    stack.push(q[2]);
                }
            }
        }
        Ok(true)
    }

    /// The `owl:Axiom` nodes that reify the triple `s p o`, and whether the scan saw every
    /// reification of the target `o`. A blank-node target that many axioms share has one
    /// reification for each of them, so the scan reads up to [`MAX_REIFIERS`] of them.
    fn reifiers_of(&mut self, s: Id, p: Id, o: Id) -> Result<(Vec<Id>, bool)> {
        let (Some(src), Some(prop), Some(tgt)) = (
            self.id(OWL_ANNOTATED_SOURCE),
            self.id(OWL_ANNOTATED_PROPERTY),
            self.id(OWL_ANNOTATED_TARGET),
        ) else {
            return Ok((Vec::new(), true));
        };
        let mut out = Vec::new();
        let found = self.quads(Perm::Pos, &[tgt.0, o.0], MAX_REIFIERS + 1)?;
        let complete = found.len() <= MAX_REIFIERS;
        for q in found.into_iter().take(MAX_REIFIERS) {
            let r = q[0];
            if self.snap.term_kind(r) == TermKind::Iri || out.contains(&r) {
                continue;
            }
            if self.exists(&[r, src, s])? && self.exists(&[r, prop, p])? {
                out.push(r);
            }
        }
        Ok((out, complete))
    }

    /// The annotations of the reified triples of `reifiers`, by their property and
    /// target.
    fn reifications(&mut self, reifiers: &[Id]) -> Result<Reifications> {
        let (Some(prop), Some(tgt), Some(src)) = (
            self.id(OWL_ANNOTATED_PROPERTY),
            self.id(OWL_ANNOTATED_TARGET),
            self.id(OWL_ANNOTATED_SOURCE),
        ) else {
            return Ok(FxHashMap::default());
        };
        let rdf_type = self.id(RDF_TYPE);
        let mut out: FxHashMap<(Id, Id), Vec<(Id, Id)>> = FxHashMap::default();
        for r in reifiers {
            let qs = self.quads(Perm::Spo, &[r.0], MAX_VALUES)?;
            let p = qs.iter().find(|q| q[1] == prop).map(|q| q[2]);
            let o = qs.iter().find(|q| q[1] == tgt).map(|q| q[2]);
            let (Some(p), Some(o)) = (p, o) else { continue };
            let list = out.entry((p, o)).or_default();
            for q in qs {
                if q[1] == prop || q[1] == tgt || q[1] == src || Some(q[1]) == rdf_type {
                    continue;
                }
                if self.snap.term_kind(q[2]) == TermKind::BNode {
                    continue;
                }
                if !list.contains(&(q[1], q[2])) {
                    list.push((q[1], q[2]));
                }
            }
        }
        Ok(out)
    }

    /// The roots of the axioms of other entities that hold `x` inside blank nodes, at
    /// most `limit`, with their number up to 5,000 and whether more exist.
    fn mention_roots(&mut self, x: Id, limit: usize) -> Result<(Vec<Root>, usize, bool)> {
        const MAX_COUNTED: usize = 5000;
        let src = self.id(OWL_ANNOTATED_SOURCE);
        let skip: Vec<Id> = [OWL_ANNOTATED_SOURCE, OWL_ANNOTATED_TARGET]
            .iter()
            .filter_map(|p| self.id(p))
            .collect();
        let mut roots = Vec::new();
        let mut keys: FxHashSet<[Id; 4]> = FxHashSet::default();
        let mut blanks: FxHashSet<Id> = FxHashSet::default();
        let mut total = 0usize;
        let mut truncated = false;
        let mut seen: FxHashSet<Id> = FxHashSet::default();
        let mut stack: Vec<Id> = Vec::new();
        for q in self.quads(Perm::Osp, &[x.0], MAX_INCOMING_COUNT * 10)? {
            if self.snap.term_kind(q[0]) == TermKind::BNode
                && !skip.contains(&q[1])
                && seen.insert(q[0])
            {
                stack.push(q[0]);
            }
        }
        let mut walked = 0usize;
        while let Some(b) = stack.pop() {
            walked += 1;
            if walked.is_multiple_of(256) {
                check(&self.opts)?;
            }
            if total >= MAX_COUNTED || walked > MAX_COUNTED * 20 {
                truncated = true;
                break;
            }
            // a reification node annotates another axiom: not a use of the entity
            if let Some(src) = src
                && !self.quads(Perm::Spo, &[b.0, src.0], 1)?.is_empty()
            {
                continue;
            }
            let parents = self.quads(Perm::Osp, &[b.0], MAX_COUNTED * 20)?;
            if parents.len() >= MAX_COUNTED * 20 {
                truncated = true;
            }
            if parents.is_empty() {
                if blanks.insert(b) {
                    total += 1;
                    if roots.len() < limit {
                        roots.push(Root::Blank(b));
                    }
                }
                continue;
            }
            for q in parents {
                if skip.contains(&q[1]) {
                    continue;
                }
                if self.snap.term_kind(q[0]) == TermKind::BNode {
                    if seen.insert(q[0]) {
                        stack.push(q[0]);
                    }
                } else if keys.insert(q) {
                    total += 1;
                    if roots.len() < limit {
                        roots.push(Root::Named(q));
                    }
                }
            }
        }
        let listed = roots.len();
        Ok((roots, total, truncated || total > listed))
    }

    /// The statements of named subjects whose object is `x`, by predicate.
    fn incoming(&mut self, x: Id, listed: usize) -> Result<Vec<Incoming>> {
        let skip: Vec<Id> = [OWL_ANNOTATED_SOURCE, OWL_ANNOTATED_TARGET]
            .iter()
            .filter_map(|p| self.id(p))
            .collect();
        let mut out = Vec::new();
        // walk the predicates of `x` in Ops order, jumping past each predicate's range
        // once its count reaches the cap
        let mut lo = [x.0, 0, 0, 0];
        loop {
            check(&self.opts)?;
            let mut first: Option<Id> = None;
            let mut count = 0usize;
            let mut subjects: Vec<Id> = Vec::new();
            let mut more = false;
            let mut next: Option<u64> = None;
            let graphs = &self.graphs;
            let snap = &self.snap;
            snap.scan_between(Perm::Ops, lo, [x.0, u64::MAX, u64::MAX, u64::MAX], |c| {
                let mut each = |q: [Id; 4]| -> bool {
                    match first {
                        None => first = Some(q[1]),
                        Some(p) if p != q[1] => {
                            next = Some(q[1].0);
                            return false;
                        }
                        _ => {}
                    }
                    if !graphs.contains_key(&q[3]) || snap.term_kind(q[0]) == TermKind::BNode {
                        return true;
                    }
                    if count >= MAX_INCOMING_COUNT {
                        more = true;
                        next = q[1].0.checked_add(1);
                        return false;
                    }
                    count += 1;
                    if subjects.len() < listed && !subjects.contains(&q[0]) {
                        subjects.push(q[0]);
                    }
                    true
                };
                match c {
                    Chunk::Block(b, s, e) => {
                        for i in s..e {
                            if !each(Perm::Ops.to_quad(&b.key(i))) {
                                return Ok(false);
                            }
                        }
                    }
                    Chunk::Row(k) => {
                        if !each(Perm::Ops.to_quad(&k)) {
                            return Ok(false);
                        }
                    }
                }
                Ok(true)
            })?;
            if let Some(p) = first
                && count > 0
                && !skip.contains(&p)
            {
                out.push(Incoming {
                    property: self.iri(p),
                    count,
                    more,
                    subjects: subjects.iter().map(|s| self.iri(*s)).collect(),
                });
            }
            match next {
                Some(n) => lo = [x.0, n, 0, 0],
                None => break,
            }
        }
        out.sort_by(|a, b| b.count.cmp(&a.count).then(a.property.cmp(&b.property)));
        Ok(out)
    }

    /// The axioms `quads` map to, read with the types of every IRI they use, with the
    /// graph of each axiom.
    fn map(&mut self, quads: &[[Id; 4]]) -> Result<Vec<(Axiom, u32)>> {
        if quads.is_empty() {
            return Ok(Vec::new());
        }
        let mut all = Collector::default();
        for q in quads {
            all.add(*q);
        }
        // the quads after these are context: the types of the IRIs the fragment uses
        let fragment = all.quads.len();
        if let Some(t) = self.id(RDF_TYPE) {
            let used: BTreeSet<Id> = quads
                .iter()
                .flat_map(|q| [q[0], q[1], q[2]])
                .filter(|id| self.snap.term_kind(*id) == TermKind::Iri)
                .collect();
            for id in used {
                for q in self.quads(Perm::Spo, &[id.0, t.0], 64)? {
                    all.add(q);
                }
            }
        }
        let mut triples = Vec::with_capacity(all.quads.len());
        let mut graph_of = Vec::with_capacity(all.quads.len());
        let mut context = Vec::with_capacity(all.quads.len());
        for (n, q) in all.quads.iter().enumerate() {
            if let Some(t) = triple_of(&self.snap, q) {
                triples.push(t);
                graph_of.push(self.graphs[&q[3]]);
                context.push(n >= fragment);
            }
        }
        let primary: Vec<u32> = (0..self.refs.len().max(1) as u32).collect();
        let o = read::read(
            triples,
            graph_of,
            &primary,
            ReadOptions {
                mode: self.mode,
                terminology_only: false,
                max_diagnostics: 0,
            },
        );
        // An axiom belongs to the fragment when its main triple does, or one of its
        // triples when it has no main triple. The context's type triples only type the
        // fragment's IRIs, so the axioms they alone give, such as another IRI's class
        // assertion or a property characteristic, are left out. A declaration that the
        // mapping implies has no triples and is kept for the fragment's own subjects.
        let own: FxHashSet<Id> = quads.iter().map(|q| q[0]).collect();
        let own_iris: FxHashSet<String> = own
            .iter()
            .filter(|id| self.snap.term_kind(**id) == TermKind::Iri)
            .map(|id| self.iri(*id))
            .collect();
        let in_fragment = |o: &read::Origin| match o.main {
            Some(m) => !context[m],
            None => o.triples.iter().any(|t| !context[*t]),
        };
        Ok(o.axioms
            .into_iter()
            .filter(|(a, origins)| {
                if origins.iter().any(|o| !o.triples.is_empty()) {
                    return origins.iter().any(in_fragment);
                }
                match &a.kind {
                    sparkles_owl::AxiomKind::Declaration(entity) => {
                        own_iris.contains(entity.iri().as_str())
                    }
                    _ => true,
                }
            })
            .map(|(a, origins)| {
                let g = origins.first().map_or(0, |o| o.graph);
                (a, g)
            })
            .collect())
    }

    /// The ontology IRI that each graph read declares, if any.
    fn ontologies(&mut self) -> Result<Vec<Option<String>>> {
        let mut out = vec![None; self.refs.len()];
        let (Some(t), Some(onto)) = (self.id(RDF_TYPE), self.id(OWL_ONTOLOGY)) else {
            return Ok(out);
        };
        for q in self.quads(Perm::Pos, &[t.0, onto.0], 1000)? {
            let n = self.graphs[&q[3]] as usize;
            if out[n].is_none() && self.snap.term_kind(q[0]) == TermKind::Iri {
                out[n] = Some(self.iri(q[0]));
            }
        }
        Ok(out)
    }

    fn deprecated(&mut self, id: Id) -> Result<bool> {
        let Some(p) = self.id(OWL_DEPRECATED) else {
            return Ok(false);
        };
        for o in self.objects(id, p, 4)? {
            if let Some(Term::Literal(l)) = self.snap.term(o)
                && matches!(l.value(), "true" | "1")
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The rank of a language tag among the caller's: theirs first, then untagged,
    /// then English, then the others.
    fn lang_rank(&self, lang: &str) -> usize {
        let lang = lang.to_ascii_lowercase();
        if let Some(i) = self
            .languages
            .iter()
            .position(|l| *l == lang || lang.starts_with(&format!("{l}-")))
        {
            return i;
        }
        let n = self.languages.len();
        match lang.as_str() {
            "" => n,
            "en" => n + 1,
            l if l.starts_with("en-") => n + 1,
            _ => n + 2,
        }
    }

    /// The label of `id` in the best language: `rdfs:label`, `skos:prefLabel` or the
    /// literal form of a SKOS-XL preferred label.
    fn label(&mut self, id: Id) -> Result<Option<(String, Option<String>)>> {
        let mut best: Option<(usize, usize, String, String)> = None;
        let props = [
            "http://www.w3.org/2000/01/rdf-schema#label",
            "http://www.w3.org/2004/02/skos/core#prefLabel",
        ];
        let mut lits: Vec<(usize, Id)> = Vec::new();
        for (rank, p) in props.iter().enumerate() {
            if let Some(p) = self.id(p) {
                for o in self.objects(id, p, 64)? {
                    lits.push((rank, o));
                }
            }
        }
        if let (Some(xl), Some(form)) = (
            self.id("http://www.w3.org/2008/05/skos-xl#prefLabel"),
            self.id("http://www.w3.org/2008/05/skos-xl#literalForm"),
        ) {
            for l in self.objects(id, xl, 64)? {
                for o in self.objects(l, form, 4)? {
                    lits.push((2, o));
                }
            }
        }
        for (prop, o) in lits {
            if let Some(Term::Literal(l)) = self.snap.term(o) {
                let lang = l.language().unwrap_or("").to_string();
                let key = (self.lang_rank(&lang), prop, l.value().to_string(), lang);
                if best.as_ref().is_none_or(|b| key < *b) {
                    best = Some(key);
                }
            }
        }
        Ok(best.map(|(_, _, v, l)| (v, (!l.is_empty()).then_some(l))))
    }

    /// The short form of `id`: `oboInOwl:id`, `skos:notation`, a prefixed name or the
    /// local name of its IRI.
    fn short_form(&mut self, id: Id, iri: &str) -> Result<Option<String>> {
        for p in [OBO_ID, "http://www.w3.org/2004/02/skos/core#notation"] {
            if let Some(p) = self.id(p) {
                let mut vals: Vec<String> = self
                    .objects(id, p, 8)?
                    .into_iter()
                    .filter_map(|o| match self.snap.term(o) {
                        Some(Term::Literal(l)) => Some(l.value().to_string()),
                        _ => None,
                    })
                    .collect();
                vals.sort();
                if let Some(v) = vals.into_iter().next() {
                    return Ok(Some(v));
                }
            }
        }
        if let Some(p) = super::search::prefixed(iri, &self.prefixes) {
            return Ok(Some(p));
        }
        let local = iri
            .rsplit(['#', '/', ':'])
            .next()
            .filter(|l| !l.is_empty() && l.len() < iri.len());
        Ok(local.map(str::to_string))
    }

    /// The kinds of `id` from its types, named as the entity search names them.
    fn kinds(&mut self, id: Id) -> Result<Vec<&'static str>> {
        const KINDS: [(&str, &str); 14] = [
            ("http://www.w3.org/2002/07/owl#Class", "class"),
            ("http://www.w3.org/2000/01/rdf-schema#Class", "class"),
            (
                "http://www.w3.org/2002/07/owl#ObjectProperty",
                "objectProperty",
            ),
            (
                "http://www.w3.org/2002/07/owl#DatatypeProperty",
                "dataProperty",
            ),
            (
                "http://www.w3.org/2002/07/owl#AnnotationProperty",
                "annotationProperty",
            ),
            (
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#Property",
                "property",
            ),
            (
                "http://www.w3.org/2002/07/owl#NamedIndividual",
                "individual",
            ),
            ("http://www.w3.org/2000/01/rdf-schema#Datatype", "datatype"),
            ("http://www.w3.org/2004/02/skos/core#Concept", "concept"),
            (
                "http://www.w3.org/2004/02/skos/core#ConceptScheme",
                "conceptScheme",
            ),
            (
                "http://www.w3.org/2004/02/skos/core#Collection",
                "collection",
            ),
            (
                "http://www.w3.org/2004/02/skos/core#OrderedCollection",
                "collection",
            ),
            (
                "http://www.w3.org/2002/07/owl#TransitiveProperty",
                "objectProperty",
            ),
            ("http://www.w3.org/2002/07/owl#Ontology", "ontology"),
        ];
        let Some(t) = self.id(RDF_TYPE) else {
            return Ok(Vec::new());
        };
        let mut out: Vec<&'static str> = Vec::new();
        let types = self.objects(id, t, 64)?;
        for (iri, kind) in KINDS {
            if let Some(ty) = self.id(iri)
                && types.contains(&ty)
                && !out.contains(&kind)
            {
                out.push(kind);
            }
        }
        Ok(out)
    }

    fn name(&mut self, id: Id) -> Result<Name> {
        let iri = self.iri(id);
        let label = self.label(id)?;
        Ok(Name {
            short_form: self.short_form(id, &iri)?,
            kinds: self.kinds(id)?,
            deprecated: self.deprecated(id)?,
            label: label.as_ref().map(|l| l.0.clone()),
            label_lang: label.and_then(|l| l.1),
            iri,
        })
    }

    fn node(&mut self, tree: &Tree, id: Id) -> Result<Node> {
        Ok(Node {
            name: self.name(id)?,
            children: tree.children(self, id)?.len(),
            parents: tree.parents(self, id)?.len(),
        })
    }
}

fn triple_of(snap: &Snapshot, q: &[Id; 4]) -> Option<Triple> {
    let s = match snap.term(q[0])? {
        Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
        Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
        _ => return None,
    };
    let Term::NamedNode(p) = snap.term(q[1])? else {
        return None;
    };
    Some(Triple::new(s, p, snap.term(q[2])?))
}

#[cfg(test)]
mod tests;
