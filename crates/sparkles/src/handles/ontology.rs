//! The OWL 2 ontology of a dataset's graphs (spec F21): its axioms, read by the
//! reverse mapping of the OWL 2 Mapping to RDF Graphs, its profile checks, and edits
//! that add and remove axioms through the forward mapping.
//!
//! An open ontology reads the terminology of its graphs eagerly: the triples of the
//! structural vocabulary, the declarations and the annotations, which
//! [`is_terminological`](sparkles_owl::is_terminological) selects, and the class
//! assertions whose class is a class expression. The other assertions stay in the store
//! and are mapped one triple at a time when [`Opened::assertions`] streams them, so a
//! large data graph costs no memory to open. The dataset keeps the last few open
//! ontologies of callers who read every graph, keyed by commit; any commit makes them
//! stale.
//!
//! ```
//! use sparkles::Dataset;
//! use sparkles::handles::ontology::{EditRequest, OntologyRequest};
//! use sparkles::owl::{Axiom, Class, ClassExpression, ObjectProperty};
//!
//! let ds = Dataset::memory();
//! ds.update(
//!     "PREFIX owl: <http://www.w3.org/2002/07/owl#>
//!      PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
//!      INSERT DATA {
//!        <http://ex.org/o> a owl:Ontology .
//!        <http://ex.org/Professor> a owl:Class . <http://ex.org/Course> a owl:Class .
//!        <http://ex.org/teaches> a owl:ObjectProperty .
//!        <http://ex.org/Professor> rdfs:subClassOf
//!          [ a owl:Restriction ; owl:onProperty <http://ex.org/teaches> ;
//!            owl:someValuesFrom <http://ex.org/Course> ] .
//!      }",
//! )?;
//! let req = OntologyRequest::default();
//! let axiom = Axiom::sub_class_of(
//!     Class::new("http://ex.org/Professor"),
//!     ClassExpression::some(
//!         ObjectProperty::new("http://ex.org/teaches"),
//!         Class::new("http://ex.org/Course"),
//!     ),
//! );
//! let o = ds.ontology().open(&req, &Default::default())?;
//! assert!(o.ontology.contains(&axiom));
//!
//! // remove the axiom: its restriction node goes with it
//! let edit = EditRequest { remove: vec![axiom.clone()], ..Default::default() };
//! let done = ds.ontology().edit(&req, &edit, &Default::default())?;
//! assert_eq!(done.removed, 1);
//! assert_eq!(done.deleted, 4);
//! let o = ds.ontology().open(&req, &Default::default())?;
//! assert!(!o.ontology.contains(&axiom));
//! # Ok::<_, sparkles::Error>(())
//! ```

use crate::Dataset;
use crate::commit::{CommitKind, Receipt};
use crate::error::{Error, Result};
use crate::history::{At, HistoryOptions};
use crate::id::Id;
use crate::index::Perm;
use crate::sparql::QueryOptions;
use crate::store::{Chunk, Snapshot};
use oxrdf::{NamedOrBlankNode, Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_owl::profiles::{CheckOptions, Checker, Profile, Report};
use sparkles_owl::read::{self, Mode, ReadOptions};
use sparkles_owl::{Axiom, AxiomKind, AxiomType, Entity, EntityKind, Individual, vocab, write};
use sparkles_reasoner::inputs::{GraphRef, Import, ImportMode, Inputs};
pub use sparkles_reasoner::inputs::{ImportEdge, ImportTargets, Via};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

/// The most open ontologies a dataset keeps.
pub const CACHE_ENTRIES: usize = 4;

/// The most triples of an open ontology the dataset keeps; a larger terminology is read
/// again by each request.
pub const CACHE_MAX_TRIPLES: usize = 2_000_000;

/// The label prefix of the blank nodes an edit writes, which no stored node has.
const FRESH: &str = "owl-new-";

/// What ontology to open: its graphs, their imports, the mode of the read and the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OntologyRequest {
    /// the graphs read (the default graph when empty); the first one takes edits
    pub graphs: Vec<GraphRef>,
    /// follow `owl:imports` to graphs of the dataset ([`ImportMode::Dataset`], the
    /// default) or not ([`ImportMode::None`]); fetching is refused
    pub imports: ImportMode,
    pub mode: Mode,
    /// the state read (`None`: the head); edits always start from the head
    pub at: Option<At>,
    /// also read the materialized inferences ([`INFERRED_GRAPH`]) when the dataset has
    /// them, so that the axioms include the materialized closure (spec F21 §7). The
    /// answer says whether they were read and whether they are fresh
    /// ([`Opened::inferences`]). Edits and diffs refuse it.
    pub inferences: bool,
    /// where imports resolve besides the graphs their IRIs name: the dataset's import
    /// map and the vocabulary resources (spec U12, import resolution)
    pub import_targets: ImportTargets,
}

impl Default for OntologyRequest {
    fn default() -> OntologyRequest {
        OntologyRequest {
            graphs: vec![GraphRef::Default],
            imports: ImportMode::Dataset,
            mode: Mode::Lenient,
            at: None,
            inferences: false,
            import_targets: ImportTargets::default(),
        }
    }
}

impl OntologyRequest {
    fn validate(&self) -> Result<()> {
        if self.imports == ImportMode::Fetch {
            return Err(Error::invalid(
                "an ontology reads the imports that resolve to graphs of the dataset; imports=fetch is not supported",
            ));
        }
        Ok(())
    }

    /// The graphs named, each once, in the order first named.
    fn graphs(&self) -> Vec<GraphRef> {
        if self.graphs.is_empty() {
            return vec![GraphRef::Default];
        }
        let mut out: Vec<GraphRef> = Vec::new();
        for g in &self.graphs {
            if !out.contains(g) {
                out.push(g.clone());
            }
        }
        out
    }

    fn key(&self, commit: u64, inferred: bool) -> String {
        format!(
            "{commit}|{:?}|{:?}|{}|{inferred}|{:016x}",
            self.graphs(),
            self.imports,
            self.mode.as_str(),
            self.import_targets.digest()
        )
    }

    /// The graphs this request reads in `view`, with its imports resolved through the
    /// graphs `readable` admits, and the edges of the imports closure.
    pub(crate) fn resolve(
        &self,
        view: &Snapshot,
        readable: &dyn Fn(Id) -> bool,
    ) -> (sparkles_reasoner::inputs::Resolved, Vec<ImportEdge>) {
        let inputs = Inputs {
            data_graphs: self.graphs(),
            ontology_graphs: Vec::new(),
            imports: self.imports,
            location_mapping: Default::default(),
        };
        sparkles_reasoner::inputs::resolve_closure(view, &inputs, readable, &self.import_targets)
    }
}

/// An open ontology: its terminology read from one state, through the caller's view.
#[derive(Clone)]
pub struct Opened {
    pub ontology: Arc<read::Ontology>,
    /// the state read, as the caller's view sees it
    pub snapshot: Arc<Snapshot>,
    /// the graphs read: index `n` is graph `n` of [`read::Ontology::graph_of`]. The
    /// requested graphs come first, then those imports resolved to.
    pub graphs: Vec<GraphRef>,
    /// how many of [`graphs`](Self::graphs) the request named
    pub requested: usize,
    pub imports: Vec<Import>,
    /// one edge for each `owl:imports` triple of a graph read
    pub closure: Vec<ImportEdge>,
    pub warnings: Vec<String>,
    /// whether the dataset's kept copy answered
    pub cached: bool,
    /// the materialized inferences read with the ontology, when the request asked for
    /// them and the dataset has them; `None` reads the asserted axioms only
    pub inferences: Option<Inferences>,
    inner: Arc<Inner>,
}

/// The materialized inferences an open ontology read (spec F21 §7): the profile of the
/// run and their freshness at the state read, as a query that reads them reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inferences {
    /// the profile of the materialization, such as `owl-rl`
    pub profile: String,
    /// the index of [`INFERRED_GRAPH`] in [`Opened::graphs`]
    pub graph: u32,
    /// `Some(true)` when a commit since the materialization changed a graph it read,
    /// `None` when that is unknown
    pub stale: Option<bool>,
    pub commits_since: Option<u64>,
    /// why the inferences are stale, or why their freshness is unknown
    pub reason: Option<String>,
}

/// The named graph of the materialized inferences, which an inferred view reads.
pub use crate::reasoning::INFERRED_GRAPH;

/// What the dataset keeps of an open ontology besides the axioms.
struct Inner {
    /// the stored quad of each triple of the ontology
    quads: Vec<[Id; 4]>,
    /// the id of each graph read that has quads
    graph_ids: Vec<Option<Id>>,
    /// the predicates whose triples were read eagerly
    terminological: FxHashSet<Id>,
    /// the main triples of reified axioms whose predicate is not terminological, read
    /// eagerly with their `owl:Axiom` node and left out of the assertion stream
    mains: FxHashSet<[Id; 4]>,
}

/// One kept ontology.
pub struct CacheEntry {
    key: String,
    ontology: Arc<read::Ontology>,
    graphs: Vec<GraphRef>,
    requested: usize,
    imports: Vec<Import>,
    closure: Vec<ImportEdge>,
    warnings: Vec<String>,
    inner: Arc<Inner>,
}

/// The counts that describe an open ontology.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub header: sparkles_owl::Header,
    /// the axioms of the terminology by type (assertions are counted by
    /// [`Opened::assertions`])
    pub axioms: BTreeMap<AxiomType, usize>,
    /// the declared entities by kind
    pub entities: BTreeMap<EntityKind, usize>,
    pub triples: usize,
    pub unmapped: usize,
    pub warnings: usize,
    pub errors: usize,
}

/// What an answer about a read of an ontology carries besides its axioms: the commit
/// read, the graphs, the imports, the warnings and the inferences, for an [`Opened`]
/// ontology or a [`file::OntologyFile`].
#[derive(Clone, Copy, Debug)]
pub struct ReadView<'a> {
    pub commit: u64,
    /// the graphs read: index `n` is graph `n` of [`read::Ontology::graph_of`]
    pub graphs: &'a [GraphRef],
    pub imports: &'a [Import],
    pub closure: &'a [ImportEdge],
    pub warnings: &'a [String],
    pub inferences: Option<&'a Inferences>,
}

impl ReadView<'_> {
    /// The name of graph `n` of the read.
    pub fn graph_name(&self, n: u32) -> &str {
        self.graphs.get(n as usize).map_or("", |g| g.as_str())
    }
}

impl Opened {
    /// The name of graph `n` of the ontology.
    pub fn graph_name(&self, n: u32) -> &str {
        self.graphs.get(n as usize).map_or("", |g| g.as_str())
    }

    /// The members of the read that answers carry.
    pub fn view(&self) -> ReadView<'_> {
        ReadView {
            commit: self.snapshot.commit,
            graphs: &self.graphs,
            imports: &self.imports,
            closure: &self.closure,
            warnings: &self.warnings,
            inferences: self.inferences.as_ref(),
        }
    }

    /// The counts of the ontology's terminology.
    pub fn summary(&self) -> Summary {
        let o = &self.ontology;
        let mut axioms = BTreeMap::new();
        let mut entities = BTreeMap::new();
        for ax in o.axiom_set() {
            *axioms.entry(ax.axiom_type()).or_insert(0) += 1;
            if let sparkles_owl::AxiomKind::Declaration(e) = &ax.kind {
                *entities.entry(e.kind()).or_insert(0) += 1;
            }
        }
        let errors = o
            .diagnostics
            .iter()
            .filter(|d| d.severity == read::Severity::Error)
            .count();
        Summary {
            header: o.header.clone(),
            axioms,
            entities,
            triples: o.triples.len(),
            unmapped: o.unmapped.len(),
            warnings: o.diagnostics.len() - errors + o.diagnostics_dropped,
            errors,
        }
    }

    /// Stream the assertions that the open read left in the store: each triple of the
    /// graphs read whose predicate is not terminological, mapped by
    /// [`read::Ontology::map_assertion`]. `f` gets the axiom, or `None` for a triple that
    /// maps to no axiom, with the triple and its graph, and returns `false` to stop.
    pub fn assertions(
        &self,
        opts: &QueryOptions,
        mut f: impl FnMut(Option<Axiom>, &Triple, u32) -> Result<bool>,
    ) -> Result<()> {
        let opts = &*crate::overview::one_deadline(opts)?;
        let mut stream = self.stream(opts);
        for (g, id) in self.inner.graph_ids.iter().enumerate() {
            let Some(id) = id else { continue };
            let more = stream.scan(Perm::Gspo, &[id.0], |t, _| {
                let ax = self.ontology.map_assertion(&t);
                f(ax, &t, g as u32)
            })?;
            if !more {
                break;
            }
        }
        Ok(())
    }

    fn stream<'a>(&'a self, opts: &'a QueryOptions) -> Stream<'a> {
        Stream {
            o: self,
            opts,
            rdf_type: self.snapshot.lookup_iri(vocab::RDF_TYPE),
            n: 0,
        }
    }

    /// The graph index of the stored graph `id`, when the ontology read it.
    fn graph_index(&self, id: Id) -> Option<u32> {
        self.inner
            .graph_ids
            .iter()
            .position(|g| *g == Some(id))
            .map(|n| n as u32)
    }

    /// The store id of an individual, when the store holds it.
    fn individual_id(&self, i: &Individual) -> Option<Id> {
        match i {
            Individual::Named(n) => self.snapshot.lookup_iri(n.as_str()),
            Individual::Anonymous(a) => self
                .snapshot
                .lookup_term(&Term::BlankNode(oxrdf::BlankNode::new(a.label()).ok()?)),
        }
    }

    // ------------------------------------------------------ entity accessors ------

    /// The entity of kind `kind` that `iri` names (spec F21 §2.5): `NotFound` when the
    /// ontology neither declares it nor, in a lenient read, uses it as that kind. The
    /// accessors of [`read::Ontology`], such as
    /// [`super_classes`](read::Ontology::super_classes), answer from the terminology.
    ///
    /// An entity that only the assertions left in the store use is found too, by the
    /// probes of [`kinds`](Self::kinds) under the deadline of `opts`.
    pub fn entity(&self, kind: EntityKind, iri: &str, opts: &QueryOptions) -> Result<Entity> {
        if let Some(e) = self.ontology.entity(kind, iri) {
            return Ok(e);
        }
        if self.stored_kinds(iri, &[kind], opts)?.contains(&kind) {
            return Ok(entity_of(kind, iri));
        }
        Err(Error::NotFound(format!(
            "the ontology neither declares nor uses <{iri}> as {}",
            kind.name()
        )))
    }

    /// The kinds of [`read::Ontology::kinds`], with the kinds that only the assertions
    /// left in the store give `iri`. A class is found as the class of a stored class
    /// assertion, a property as the property of a stored property or annotation
    /// assertion, and a named individual as the subject of a stored assertion or the
    /// object of a stored equality or inequality. Each probe stops at its first match,
    /// runs only for a kind the read does not already give, and keeps the deadline and
    /// cancellation of `opts`.
    pub fn kinds(&self, iri: &str, opts: &QueryOptions) -> Result<Vec<EntityKind>> {
        use EntityKind as K;
        let mut kinds = self.ontology.kinds(iri);
        let missing: Vec<EntityKind> = [
            K::Class,
            K::ObjectProperty,
            K::DataProperty,
            K::AnnotationProperty,
            K::NamedIndividual,
        ]
        .into_iter()
        .filter(|k| !kinds.contains(k))
        .collect();
        kinds.extend(self.stored_kinds(iri, &missing, opts)?);
        kinds.sort();
        kinds.dedup();
        Ok(kinds)
    }

    /// The kinds among `wanted` that the assertions the store holds in the graphs read
    /// give `iri` (see [`kinds`](Self::kinds)).
    fn stored_kinds(
        &self,
        iri: &str,
        wanted: &[EntityKind],
        opts: &QueryOptions,
    ) -> Result<Vec<EntityKind>> {
        use AxiomKind as A;
        use EntityKind as K;
        let mut found = Vec::new();
        let Some(id) = self.snapshot.lookup_iri(iri) else {
            return Ok(found);
        };
        if vocab::is_reserved(iri) || wanted.is_empty() {
            return Ok(found);
        }
        let opts = &*crate::overview::one_deadline(opts)?;
        let mut stream = self.stream(opts);
        let want = |k: &EntityKind| wanted.contains(k);
        let read = |q: &[Id; 4]| self.graph_index(q[3]).is_some();
        if want(&K::Class)
            && let Some(t) = stream.rdf_type
        {
            stream.scan(Perm::Pos, &[t.0, id.0], |t, q| {
                if read(q)
                    && let Some(Axiom {
                        kind: A::ClassAssertion(..),
                        ..
                    }) = self.ontology.map_assertion(&t)
                {
                    found.push(K::Class);
                    return Ok(false);
                }
                Ok(true)
            })?;
        }
        if [K::ObjectProperty, K::DataProperty, K::AnnotationProperty]
            .iter()
            .any(want)
        {
            // the first assertion that maps decides the property's kind
            stream.scan(Perm::Pso, &[id.0], |t, q| {
                if !read(q) {
                    return Ok(true);
                }
                let Some(ax) = self.ontology.map_assertion(&t) else {
                    return Ok(true);
                };
                let kind = match ax.kind {
                    A::ObjectPropertyAssertion(..) => K::ObjectProperty,
                    A::DataPropertyAssertion(..) => K::DataProperty,
                    A::AnnotationAssertion(..) => K::AnnotationProperty,
                    _ => return Ok(false),
                };
                if want(&kind) {
                    found.push(kind);
                }
                Ok(false)
            })?;
        }
        if want(&K::NamedIndividual) {
            let individual = |ax: &Axiom| !matches!(ax.kind, A::AnnotationAssertion(..));
            let mut hit = false;
            for g in self.inner.graph_ids.iter().flatten() {
                stream.scan(Perm::Gspo, &[g.0, id.0], |t, _| {
                    hit = self
                        .ontology
                        .map_assertion(&t)
                        .is_some_and(|ax| individual(&ax));
                    Ok(!hit)
                })?;
                if hit {
                    break;
                }
            }
            for p in [vocab::OWL_SAME_AS, vocab::OWL_DIFFERENT_FROM] {
                let Some(pid) = self.snapshot.lookup_iri(p).filter(|_| !hit) else {
                    continue;
                };
                stream.scan(Perm::Ops, &[id.0, pid.0], |t, q| {
                    hit = read(q) && self.ontology.map_assertion(&t).is_some();
                    Ok(!hit)
                })?;
            }
            if !hit {
                // A streamed object assertion can name an individual only as its
                // object. Probe that incoming range under the same view, and require
                // its mapped individual position rather than any RDF object IRI.
                let rdf_type = stream.rdf_type;
                stream.scan(Perm::Osp, &[id.0], |t, q| {
                    if !read(q) || Some(q[1]) == rdf_type {
                        return Ok(true);
                    }
                    hit = self.ontology.map_assertion(&t).is_some_and(|ax| {
                        ax.signature()
                            .iter()
                            .any(|e| e.kind() == K::NamedIndividual && e.iri().as_str() == iri)
                    });
                    Ok(!hit)
                })?;
            }
            if hit {
                found.push(K::NamedIndividual);
            }
        }
        Ok(found)
    }

    /// The class `iri` names, or `NotFound`. This and the other typed lookups probe the
    /// store without a deadline; [`entity`](Self::entity) takes one.
    pub fn class(&self, iri: &str) -> Result<sparkles_owl::Class> {
        self.entity(EntityKind::Class, iri, &QueryOptions::default())
            .map(|_| sparkles_owl::Class::new(iri))
    }

    /// The object property `iri` names, or `NotFound`.
    pub fn object_property(&self, iri: &str) -> Result<sparkles_owl::ObjectProperty> {
        self.entity(EntityKind::ObjectProperty, iri, &QueryOptions::default())
            .map(|_| sparkles_owl::ObjectProperty::new(iri))
    }

    /// The data property `iri` names, or `NotFound`.
    pub fn data_property(&self, iri: &str) -> Result<sparkles_owl::DataProperty> {
        self.entity(EntityKind::DataProperty, iri, &QueryOptions::default())
            .map(|_| sparkles_owl::DataProperty::new(iri))
    }

    /// The annotation property `iri` names, or `NotFound`.
    pub fn annotation_property(&self, iri: &str) -> Result<sparkles_owl::AnnotationProperty> {
        self.entity(
            EntityKind::AnnotationProperty,
            iri,
            &QueryOptions::default(),
        )
        .map(|_| sparkles_owl::AnnotationProperty::new(iri))
    }

    /// The datatype `iri` names, or `NotFound`.
    pub fn datatype(&self, iri: &str) -> Result<sparkles_owl::Datatype> {
        self.entity(EntityKind::Datatype, iri, &QueryOptions::default())
            .map(|_| sparkles_owl::Datatype::new(iri))
    }

    /// The named individual `iri` names, or `NotFound`.
    pub fn named_individual(&self, iri: &str) -> Result<sparkles_owl::NamedIndividual> {
        self.entity(EntityKind::NamedIndividual, iri, &QueryOptions::default())
            .map(|_| sparkles_owl::NamedIndividual::new(iri))
    }

    /// Stream the instances of the class `c` (spec F21 §4.5): the class assertions the
    /// read holds, then those the store holds in the graphs read, which the store's
    /// predicate-object index finds without reading the other assertions. `f` gets each
    /// individual with its graph, once per graph that asserts it, and returns `false` to
    /// stop.
    pub fn instances(
        &self,
        c: &sparkles_owl::Class,
        opts: &QueryOptions,
        mut f: impl FnMut(Individual, u32) -> Result<bool>,
    ) -> Result<()> {
        let opts = &*crate::overview::one_deadline(opts)?;
        let ce = sparkles_owl::ClassExpression::from(c);
        for (ax, origins) in &self.ontology.axioms {
            if let AxiomKind::ClassAssertion(x, i) = &ax.kind
                && *x == ce
            {
                for g in origin_graphs(origins) {
                    if !f(i.clone(), g)? {
                        return Ok(());
                    }
                }
            }
        }
        let mut stream = self.stream(opts);
        let (Some(t), Some(cid)) = (stream.rdf_type, self.snapshot.lookup_iri(c.as_str())) else {
            return Ok(());
        };
        stream.scan(Perm::Pos, &[t.0, cid.0], |t, q| {
            let Some(g) = self.graph_index(q[3]) else {
                return Ok(true);
            };
            match self.ontology.map_assertion(&t) {
                Some(Axiom {
                    kind: AxiomKind::ClassAssertion(_, i),
                    ..
                }) => f(i, g),
                _ => Ok(true),
            }
        })?;
        Ok(())
    }

    /// Stream the classes of the class assertions about `i`: those the read holds, then
    /// those the store holds in the graphs read. `f` returns `false` to stop.
    pub fn types_of(
        &self,
        i: &Individual,
        opts: &QueryOptions,
        mut f: impl FnMut(sparkles_owl::ClassExpression, u32) -> Result<bool>,
    ) -> Result<()> {
        let opts = &*crate::overview::one_deadline(opts)?;
        for (ax, origins) in &self.ontology.axioms {
            if let AxiomKind::ClassAssertion(c, x) = &ax.kind
                && x == i
            {
                for g in origin_graphs(origins) {
                    if !f(c.clone(), g)? {
                        return Ok(());
                    }
                }
            }
        }
        let mut stream = self.stream(opts);
        let (Some(t), Some(iid)) = (stream.rdf_type, self.individual_id(i)) else {
            return Ok(());
        };
        for (g, id) in self.inner.graph_ids.iter().enumerate() {
            let Some(id) = id else { continue };
            let more = stream.scan(Perm::Gspo, &[id.0, iid.0, t.0], |t, _| {
                match self.ontology.map_assertion(&t) {
                    Some(Axiom {
                        kind: AxiomKind::ClassAssertion(c, _),
                        ..
                    }) => f(c, g as u32),
                    _ => Ok(true),
                }
            })?;
            if !more {
                break;
            }
        }
        Ok(())
    }

    /// Stream the object and data property assertions, positive and negative, whose
    /// subject is `i`: those the read holds, then those the store holds in the graphs
    /// read, which the store finds by subject. `f` returns `false` to stop.
    pub fn property_assertions(
        &self,
        i: &Individual,
        opts: &QueryOptions,
        mut f: impl FnMut(Axiom, u32) -> Result<bool>,
    ) -> Result<()> {
        use AxiomKind as K;
        let opts = &*crate::overview::one_deadline(opts)?;
        let about = |ax: &Axiom| match &ax.kind {
            K::ObjectPropertyAssertion(_, s, _)
            | K::NegativeObjectPropertyAssertion(_, s, _)
            | K::DataPropertyAssertion(_, s, _)
            | K::NegativeDataPropertyAssertion(_, s, _) => s == i,
            _ => false,
        };
        for (ax, origins) in &self.ontology.axioms {
            if about(ax) {
                for g in origin_graphs(origins) {
                    if !f(ax.clone(), g)? {
                        return Ok(());
                    }
                }
            }
        }
        let mut stream = self.stream(opts);
        let Some(iid) = self.individual_id(i) else {
            return Ok(());
        };
        for (g, id) in self.inner.graph_ids.iter().enumerate() {
            let Some(id) = id else { continue };
            let more = stream.scan(Perm::Gspo, &[id.0, iid.0], |t, _| {
                match self.ontology.map_assertion(&t) {
                    Some(ax) if about(&ax) => f(ax, g as u32),
                    _ => Ok(true),
                }
            })?;
            if !more {
                break;
            }
        }
        Ok(())
    }
}

impl Opened {
    /// Stream the individual equalities and inequalities that name `i`: the
    /// `SameIndividual` and `DifferentIndividuals` axioms the read holds, then the
    /// `owl:sameAs` and `owl:differentFrom` assertions the store holds in the graphs read
    /// with `i` as subject or object. `f` gets each axiom once per graph that holds it
    /// and returns `false` to stop.
    pub fn equalities(
        &self,
        i: &Individual,
        opts: &QueryOptions,
        mut f: impl FnMut(Axiom, u32) -> Result<bool>,
    ) -> Result<()> {
        use AxiomKind as K;
        let opts = &*crate::overview::one_deadline(opts)?;
        for (ax, origins) in &self.ontology.axioms {
            if let K::SameIndividual(set) | K::DifferentIndividuals(set) = &ax.kind
                && set.contains(i)
            {
                for g in origin_graphs(origins) {
                    if !f(ax.clone(), g)? {
                        return Ok(());
                    }
                }
            }
        }
        let Some(iid) = self.individual_id(i) else {
            return Ok(());
        };
        let mut stream = self.stream(opts);
        let preds: Vec<Id> = [vocab::OWL_SAME_AS, vocab::OWL_DIFFERENT_FROM]
            .iter()
            .filter_map(|p| self.snapshot.lookup_iri(p))
            .collect();
        for &p in &preds {
            // the other individual and graph of each assertion streamed by subject, so
            // that its reverse in the same graph, the same axiom, is not streamed again
            let mut streamed: FxHashSet<(Id, Id)> = FxHashSet::default();
            for (g, id) in self.inner.graph_ids.iter().enumerate() {
                let Some(id) = id else { continue };
                let more = stream.scan(Perm::Gspo, &[id.0, iid.0, p.0], |t, q| {
                    match self.ontology.map_assertion(&t) {
                        Some(ax) => {
                            streamed.insert((q[2], q[3]));
                            f(ax, g as u32)
                        }
                        None => Ok(true),
                    }
                })?;
                if !more {
                    return Ok(());
                }
            }
            let more = stream.scan(Perm::Ops, &[iid.0, p.0], |t, q| {
                // a reflexive assertion, or the reverse of one streamed by subject in
                // the same graph, was streamed already
                if q[0] == iid || streamed.contains(&(q[0], q[3])) {
                    return Ok(true);
                }
                match (self.graph_index(q[3]), self.ontology.map_assertion(&t)) {
                    (Some(g), Some(ax)) => f(ax, g),
                    _ => Ok(true),
                }
            })?;
            if !more {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// The distinct graphs of an axiom's origins, or graph 0 for an axiom without one.
fn origin_graphs(origins: &[read::Origin]) -> BTreeSet<u32> {
    let mut graphs: BTreeSet<u32> = origins.iter().map(|o| o.graph).collect();
    if graphs.is_empty() {
        graphs.insert(0);
    }
    graphs
}

/// The entity of kind `kind` that `iri` names.
fn entity_of(kind: EntityKind, iri: &str) -> Entity {
    use sparkles_owl as o;
    match kind {
        EntityKind::Class => Entity::Class(o::Class::new(iri)),
        EntityKind::Datatype => Entity::Datatype(o::Datatype::new(iri)),
        EntityKind::ObjectProperty => Entity::ObjectProperty(o::ObjectProperty::new(iri)),
        EntityKind::DataProperty => Entity::DataProperty(o::DataProperty::new(iri)),
        EntityKind::AnnotationProperty => {
            Entity::AnnotationProperty(o::AnnotationProperty::new(iri))
        }
        EntityKind::NamedIndividual => Entity::NamedIndividual(o::NamedIndividual::new(iri)),
    }
}

/// A scan of the assertions an open read left in the store, with the filter that
/// [`Opened::assertions`], [`Opened::instances`], [`Opened::types_of`] and
/// [`Opened::property_assertions`] share, under one deadline.
struct Stream<'a> {
    o: &'a Opened,
    opts: &'a QueryOptions,
    rdf_type: Option<Id>,
    n: usize,
}

impl Stream<'_> {
    /// The triple of a stored quad when it is an assertion the read left in the store:
    /// not terminological, not the main triple of an annotated axiom the read holds, and
    /// not a class assertion of a structural type or a class expression.
    fn assertion(o: &Opened, rdf_type: Option<Id>, q: &[Id; 4]) -> Option<Triple> {
        let inner = &o.inner;
        if inner.terminological.contains(&q[1]) || inner.mains.contains(q) {
            return None;
        }
        let t = triple_of(&o.snapshot, q)?;
        // class assertions with a structural type or a class expression were read
        if Some(q[1]) == rdf_type && read::is_terminological(&t, &|_| false) {
            return None;
        }
        Some(t)
    }

    /// Scan `perm` under `prefix`, calling `f` with each assertion and its quad until it
    /// returns `false`; the answer is whether the scan ran to its end.
    fn scan(
        &mut self,
        perm: Perm,
        prefix: &[u64],
        mut f: impl FnMut(Triple, &[Id; 4]) -> Result<bool>,
    ) -> Result<bool> {
        let stopped = std::cell::Cell::new(false);
        let (o, opts, rdf_type) = (self.o, self.opts, self.rdf_type);
        let n = &mut self.n;
        let mut each = |q: &[Id; 4]| -> Result<bool> {
            *n += 1;
            if n.is_multiple_of(4096) {
                check(opts)?;
            }
            let Some(t) = Self::assertion(o, rdf_type, q) else {
                return Ok(true);
            };
            let more = f(t, q)?;
            stopped.set(!more);
            Ok(more)
        };
        o.snapshot.scan(perm, prefix, |c| {
            match c {
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        if !each(&perm.to_quad(&b.key(i)))? {
                            return Ok(false);
                        }
                    }
                }
                Chunk::Row(k) => return each(&perm.to_quad(&k)),
            }
            Ok(true)
        })?;
        Ok(!stopped.get())
    }
}

/// What a check covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckRequest {
    pub profiles: Vec<Profile>,
    /// also check the assertions the store holds, one triple at a time
    pub assertions: bool,
    /// the most violations each report lists
    pub max_violations: usize,
}

impl Default for CheckRequest {
    fn default() -> CheckRequest {
        CheckRequest {
            profiles: vec![Profile::Dl],
            assertions: true,
            max_violations: CheckOptions::default().max_violations,
        }
    }
}

/// The axioms an edit adds and removes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditRequest {
    pub add: Vec<Axiom>,
    pub remove: Vec<Axiom>,
    /// also add the declarations of the entities an added axiom uses that the
    /// ontology does not declare (`None`: true)
    pub declare: Option<bool>,
    /// the graph that takes the added triples (`None`: the first graph of the request)
    pub graph: Option<GraphRef>,
    /// the commit message
    pub message: Option<String>,
}

/// What an edit did.
#[derive(Clone, Debug)]
pub struct EditOutcome {
    pub receipt: Receipt,
    /// the axioms added, declarations included
    pub added: usize,
    /// the axioms asked to be added that the ontology already had
    pub present: usize,
    pub removed: usize,
    /// the axioms asked to be removed that the ontology does not have
    pub absent: Vec<Axiom>,
    /// the declarations added for the entities the added axioms use
    pub declared: Vec<Entity>,
    pub inserted: usize,
    pub deleted: usize,
    /// triples of removed axioms kept because another axiom or triple uses them
    pub kept: usize,
    /// the axioms the edit wrote, declarations included, in the order written
    pub added_axioms: Vec<Axiom>,
    /// the axioms the edit removed, each once, as asked for or as an entity removal
    /// selected them
    pub removed_axioms: Vec<Axiom>,
}

/// The ontology handle of a dataset.
#[derive(Clone)]
pub struct Ontologies {
    pub(crate) ds: Dataset,
}

impl Dataset {
    /// The OWL 2 ontology of this dataset's graphs (spec F21).
    pub fn ontology(&self) -> Ontologies {
        Ontologies { ds: self.clone() }
    }
}

impl Ontologies {
    /// A scope over one graph, `"default"` or an IRI, as spec F21 §2.1 writes it:
    /// `ds.ontology().graph(iri).imports(true).open()`. An invalid IRI is reported by
    /// the call that ends the scope.
    pub fn graph(&self, graph: &str) -> OntologyScope {
        self.graphs([graph])
    }

    /// A scope over several graphs; the first takes the axioms an edit adds.
    pub fn graphs<S: AsRef<str>>(&self, graphs: impl IntoIterator<Item = S>) -> OntologyScope {
        let mut error = None;
        let mut list = Vec::new();
        for g in graphs {
            match GraphRef::parse(g.as_ref()) {
                Ok(g) => list.push(g),
                Err(e) => {
                    error.get_or_insert(e);
                }
            }
        }
        let mut scope = self.scope(OntologyRequest::default());
        if !list.is_empty() {
            scope.req.graphs = list;
        }
        scope.error = error;
        scope
    }

    /// A scope over the ontology a request names.
    pub fn scope(&self, req: OntologyRequest) -> OntologyScope {
        OntologyScope {
            handle: self.clone(),
            req,
            opts: QueryOptions::default(),
            error: None,
        }
    }
}

/// An ontology selected by [`Ontologies::graph`]: a builder over [`OntologyRequest`] and
/// the [`QueryOptions`] of the calls that end it, which are the handle's calls with these
/// arguments.
///
/// ```
/// use sparkles::Dataset;
/// use sparkles::owl::{Axiom, Class, ClassExpression, ObjectProperty};
///
/// let ds = Dataset::memory();
/// let onto = ds.ontology().graph("http://ex.org/onto");
/// let teaches = ClassExpression::some(
///     ObjectProperty::new("http://ex.org/teaches"),
///     Class::new("http://ex.org/Course"),
/// );
/// onto.edit(|o| {
///     o.add(Axiom::sub_class_of(Class::new("http://ex.org/Professor"), teaches.clone()))?;
///     Ok(())
/// })?;
/// let o = onto.open()?;
/// let prof = o.class("http://ex.org/Professor")?;
/// assert_eq!(o.ontology.super_classes(&prof), vec![teaches]);
/// # Ok::<_, sparkles::Error>(())
/// ```
#[derive(Clone)]
pub struct OntologyScope {
    handle: Ontologies,
    req: OntologyRequest,
    opts: QueryOptions,
    error: Option<String>,
}

impl OntologyScope {
    /// Follow `owl:imports` to graphs of the dataset (the default) or not.
    pub fn imports(mut self, follow: bool) -> Self {
        self.req.imports = if follow {
            ImportMode::Dataset
        } else {
            ImportMode::None
        };
        self
    }

    /// Read leniently (the default) or strictly.
    pub fn mode(mut self, mode: Mode) -> Self {
        self.req.mode = mode;
        self
    }

    /// Read the state `at` names instead of the head.
    pub fn at(mut self, at: At) -> Self {
        self.req.at = Some(at);
        self
    }

    /// Also read the materialized inferences ([`OntologyRequest::inferences`]).
    pub fn inferences(mut self, read: bool) -> Self {
        self.req.inferences = read;
        self
    }

    /// The options of the calls: the caller's grants, the deadline and, for edits, the
    /// commit's options in `opts.write`.
    pub fn options(mut self, opts: QueryOptions) -> Self {
        self.opts = opts;
        self
    }

    /// The request the scope stands for.
    pub fn request(&self) -> &OntologyRequest {
        &self.req
    }

    fn ready(&self) -> Result<()> {
        match &self.error {
            Some(e) => Err(Error::invalid(e.clone())),
            None => Ok(()),
        }
    }

    /// [`Ontologies::open`].
    pub fn open(&self) -> Result<Opened> {
        self.ready()?;
        self.handle.open(&self.req, &self.opts)
    }

    /// [`Ontologies::check`] against `profiles`, with the other defaults of
    /// [`CheckRequest`].
    pub fn check(&self, profiles: &[Profile]) -> Result<Vec<Report>> {
        self.check_with(&CheckRequest {
            profiles: profiles.to_vec(),
            ..Default::default()
        })
    }

    /// [`Ontologies::check`].
    pub fn check_with(&self, check: &CheckRequest) -> Result<Vec<Report>> {
        self.ready()?;
        self.handle.check(&self.req, check, &self.opts)
    }

    /// [`Ontologies::diff`].
    pub fn diff(&self, from: &At, to: &At, assertions: bool) -> Result<OntologyDiff> {
        self.ready()?;
        self.handle
            .diff(&self.req, from, to, assertions, &self.opts)
    }

    /// [`Ontologies::remove_entity`].
    pub fn remove_entity(&self, iri: &str) -> Result<EditOutcome> {
        self.ready()?;
        self.handle.remove_entity(&self.req, iri, &self.opts)
    }

    /// [`Ontologies::classify`].
    pub fn classify(&self, c: &classify::ClassifyRequest) -> Result<classify::Classified> {
        self.ready()?;
        self.handle.classify(&self.req, c, &self.opts)
    }

    /// [`Ontologies::kept_classification`].
    pub fn kept_classification(&self, query: &kept::KeptQuery) -> Result<kept::KeptRead> {
        self.ready()?;
        self.handle
            .kept_classification(&self.req, query, &self.opts)
    }

    /// [`Ontologies::dl_query`].
    pub fn dl_query(&self, q: &dl_query::DlQueryRequest) -> Result<dl_query::DlQueryAnswer> {
        self.ready()?;
        self.handle.dl_query(&self.req, q, &self.opts)
    }

    /// Edit the ontology in one commit (spec F21 §2.1): `f` names the axioms to add and
    /// remove on an [`Editor`], and [`Ontologies::edit`] then applies them under the
    /// writer lock. `f` runs before the lock is taken, so it cannot see the head the
    /// edit writes on; an error from it ends the edit before anything is written.
    pub fn edit(&self, f: impl FnOnce(&mut Editor) -> Result<()>) -> Result<EditOutcome> {
        self.ready()?;
        let mut e = Editor::default();
        f(&mut e)?;
        self.handle.edit(&self.req, &e.req, &self.opts)
    }
}

/// The additions and removals of an [`OntologyScope::edit`], collected into an
/// [`EditRequest`].
#[derive(Clone, Debug, Default)]
pub struct Editor {
    req: EditRequest,
}

impl Editor {
    /// Add an axiom; one the forward mapping cannot write is `Invalid`.
    pub fn add(&mut self, axiom: Axiom) -> Result<&mut Self> {
        sparkles_owl::validate::axiom(&axiom).map_err(|e| Error::invalid(e.0))?;
        self.req.add.push(axiom);
        Ok(self)
    }

    /// Remove an axiom. Removing one the ontology does not hold is not an error; the
    /// outcome lists it among the absent axioms.
    pub fn remove(&mut self, axiom: &Axiom) -> Result<&mut Self> {
        sparkles_owl::validate::axiom(axiom).map_err(|e| Error::invalid(e.0))?;
        self.req.remove.push(axiom.clone());
        Ok(self)
    }

    /// Also declare the entities that added axioms use and the ontology does not
    /// declare (the default), or not.
    pub fn declare(&mut self, declare: bool) -> &mut Self {
        self.req.declare = Some(declare);
        self
    }

    /// The graph that takes the added axioms, `"default"` or an IRI, instead of the
    /// scope's first graph.
    pub fn target(&mut self, graph: &str) -> Result<&mut Self> {
        self.req.graph = Some(GraphRef::parse(graph).map_err(Error::invalid)?);
        Ok(self)
    }

    /// The commit message.
    pub fn message(&mut self, message: impl Into<String>) -> &mut Self {
        self.req.message = Some(message.into());
        self
    }

    /// The request collected so far.
    pub fn request(&self) -> &EditRequest {
        &self.req
    }
}

fn check(opts: &QueryOptions) -> Result<()> {
    if opts
        .cancel
        .as_ref()
        .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
    {
        return Err(Error::Cancelled);
    }
    if let Some(c) = &opts.cancellation {
        c.check()?;
    }
    if opts.deadline.is_some_and(|d| Instant::now() >= d) {
        return Err(Error::Timeout);
    }
    Ok(())
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

impl Ontologies {
    /// The state `req.at` names through the caller's view, with the options anchored to
    /// one deadline.
    fn state(
        &self,
        req: &OntologyRequest,
        opts: &QueryOptions,
    ) -> Result<(Arc<Snapshot>, QueryOptions)> {
        let defaults = self.ds.with_query_defaults(opts);
        let opts = crate::overview::one_deadline(&defaults)?.into_owned();
        let snap = match &req.at {
            None => self.ds.store().snapshot(),
            Some(at) => {
                let o = HistoryOptions {
                    cancel: opts.cancel.clone(),
                    deadline: opts.deadline,
                };
                self.ds.store().snapshot_at(at, &o)?.0
            }
        };
        Ok((snap, opts))
    }

    /// `opts` with the dataset's defaults and one deadline, anchored now: pass the
    /// answer to every stage of one request (open, check, listing), so that a relative
    /// timeout covers them all.
    pub fn anchored(&self, opts: &QueryOptions) -> Result<QueryOptions> {
        let defaults = self.ds.with_query_defaults(opts);
        Ok(crate::overview::one_deadline(&defaults)?.into_owned())
    }

    /// Open the ontology `req` names, as the caller of `opts` may read it.
    pub fn open(&self, req: &OntologyRequest, opts: &QueryOptions) -> Result<Opened> {
        req.validate()?;
        let (snap, opts) = self.state(req, opts)?;
        self.open_at(&snap, req, &opts)
    }

    /// [`open`](Self::open) at a state the caller has.
    pub fn open_at(
        &self,
        snap: &Arc<Snapshot>,
        req: &OntologyRequest,
        opts: &QueryOptions,
    ) -> Result<Opened> {
        req.validate()?;
        // the dataset's hidden control graphs leave every view (spec C26 §2.3); a caller
        // that reads every other graph still shares the kept ontology
        let shared = opts.graphs.is_none();
        let opts = &*self.ds.without_owner_graphs(opts);
        let access = opts.graphs.as_ref().map(|a| a.bound(snap));
        let view = match &access {
            Some(a) => a.masked(snap)?,
            None => snap.clone(),
        };
        let keep = shared && snap.cacheable;
        // an inferred view reads the inferred graph while the dataset records a
        // materialization; without one it reads the asserted axioms only (§7)
        let record = if req.inferences {
            self.ds.reasoning_record()
        } else {
            None
        };
        // the hidden graphs are part of the key: enabling an owner hides a graph without
        // a new commit of the data
        let key = format!(
            "{}|{:?}",
            req.key(snap.commit, record.is_some()),
            self.ds.store().hidden_graphs()
        );
        if keep {
            let mut cache = self.ds.state().ontology_cache.lock();
            if let Some(i) = cache.iter().position(|e| e.key == key) {
                let e = cache.remove(i);
                let mut opened = Opened {
                    ontology: e.ontology.clone(),
                    snapshot: view,
                    graphs: e.graphs.clone(),
                    requested: e.requested,
                    imports: e.imports.clone(),
                    closure: e.closure.clone(),
                    warnings: e.warnings.clone(),
                    cached: true,
                    inferences: None,
                    inner: e.inner.clone(),
                };
                cache.push(e);
                drop(cache);
                opened.inferences = self.inferences(&opened, record.as_ref());
                return Ok(opened);
            }
        }
        let readable = |g: Id| access.as_ref().is_none_or(|a| a.readable_id(snap, g));
        let (resolved, closure) = req.resolve(&view, &readable);
        let mut graphs = resolved.graphs.clone();
        let inferred = GraphRef::Named(INFERRED_GRAPH.to_string());
        if record.is_some() && !graphs.contains(&inferred) {
            graphs.push(inferred);
        }
        let requested = req.graphs().len();
        let graph_ids: Vec<Option<Id>> = graphs
            .iter()
            .map(|g| g.id(&view).filter(|id| readable(*id)))
            .collect();
        let (ontology, inner) = read_store(&view, &graph_ids, requested, req.mode, opts)?;
        check(opts)?;
        let ontology = Arc::new(ontology);
        let inner = Arc::new(inner);
        if keep && ontology.triples.len() <= CACHE_MAX_TRIPLES {
            let mut cache = self.ds.state().ontology_cache.lock();
            cache.retain(|e| e.key != key);
            if cache.len() >= CACHE_ENTRIES {
                cache.remove(0);
            }
            cache.push(CacheEntry {
                key,
                ontology: ontology.clone(),
                graphs: graphs.clone(),
                requested,
                imports: resolved.imports.clone(),
                closure: closure.clone(),
                warnings: resolved.warnings.clone(),
                inner: inner.clone(),
            });
        }
        let mut opened = Opened {
            ontology,
            snapshot: view,
            graphs,
            requested,
            imports: resolved.imports,
            closure,
            warnings: resolved.warnings,
            cached: false,
            inferences: None,
            inner,
        };
        opened.inferences = self.inferences(&opened, record.as_ref());
        Ok(opened)
    }

    /// The inferences `o` read, when `record` made it read the inferred graph and the
    /// caller may read that graph, with their freshness at the state read.
    fn inferences(
        &self,
        o: &Opened,
        record: Option<&crate::reasoning::ReasoningRecord>,
    ) -> Option<Inferences> {
        let record = record?;
        let inferred = GraphRef::Named(INFERRED_GRAPH.to_string());
        let n = o.graphs.iter().position(|g| *g == inferred)?;
        o.inner.graph_ids.get(n).copied().flatten()?;
        let f = crate::reasoning::freshness(record, self.ds.store(), o.snapshot.commit);
        Some(Inferences {
            profile: record.profile.clone(),
            graph: n as u32,
            stale: f.stale,
            commits_since: f.commits_since,
            reason: f.reason,
        })
    }

    /// Check the ontology `req` names against `check.profiles`: its terminology, and its
    /// assertions when `check.assertions` is set.
    pub fn check(
        &self,
        req: &OntologyRequest,
        check: &CheckRequest,
        opts: &QueryOptions,
    ) -> Result<Vec<Report>> {
        let opts = self.anchored(opts)?;
        let o = self.open(req, &opts)?;
        check_opened(&o, check, &opts)
    }

    /// Remove an entity (spec F21 §5.3): its declarations, the axioms of the
    /// terminology that mention it, and the assertions about it that the store holds, in
    /// one commit. The answer lists them under `removed`; it is `0` for an IRI the
    /// ontology does not mention.
    pub fn remove_entity(
        &self,
        req: &OntologyRequest,
        iri: &str,
        opts: &QueryOptions,
    ) -> Result<EditOutcome> {
        self.edit_with(
            req,
            &EditRequest::default(),
            Some(iri),
            opts,
            Extra::default(),
        )
    }

    /// Add and remove axioms in one commit. The ontology is read from the head the
    /// write starts from, under the writer lock, so no commit comes between the read
    /// and the write. An added axiom goes to `edit.graph` (the first graph of `req`),
    /// with the declarations of the entities it uses that the ontology lacks. A removed
    /// axiom takes its triples with it, except those that another axiom of the ontology
    /// or another triple of the graph still uses. `opts.write` carries the commit's
    /// options: the caller's graphs, a dry run, a precondition and the author.
    pub fn edit(
        &self,
        req: &OntologyRequest,
        edit: &EditRequest,
        opts: &QueryOptions,
    ) -> Result<EditOutcome> {
        self.edit_with(req, edit, None, opts, Extra::default())
    }

    /// [`edit`](Self::edit), also removing what mentions `entity`, selected from the
    /// head under the writer lock.
    fn edit_with(
        &self,
        req: &OntologyRequest,
        edit: &EditRequest,
        entity: Option<&str>,
        opts: &QueryOptions,
        mut extra: Extra<'_>,
    ) -> Result<EditOutcome> {
        req.validate()?;
        if req.at.is_some() {
            return Err(Error::invalid(
                "an edit starts from the head: leave out 'at'",
            ));
        }
        if req.inferences {
            return Err(Error::invalid(
                "an edit changes the asserted axioms: leave out 'inferences'",
            ));
        }
        for ax in edit.add.iter().chain(&edit.remove) {
            sparkles_owl::validate::axiom(ax).map_err(|e| Error::invalid(e.0))?;
        }
        let opts = self.anchored(opts)?;
        let store = self.ds.store();
        let mut wopts = opts.write.clone();
        if wopts.graphs.is_none() {
            wopts.graphs = opts.graphs.clone();
        }
        if let Some(m) = &edit.message {
            wopts.message = Some(Arc::from(m.as_str()));
        }
        if wopts.deadline.is_none() {
            wopts.deadline = opts.deadline;
        }
        if wopts.cancel.is_none() {
            wopts.cancel = opts.cancel.clone();
        }
        let mut txn = store.try_write_with(CommitKind::Update, wopts)?;
        let head = txn.base().clone();
        let o = self.open_at(&head, req, &opts)?;
        let target = edit
            .graph
            .clone()
            .unwrap_or_else(|| req.graphs()[0].clone());
        let target_id = match &target {
            GraphRef::Default => Id::DEFAULT_GRAPH,
            GraphRef::Named(g) => txn.intern(&Term::NamedNode(
                oxrdf::NamedNode::new(g.as_str())
                    .map_err(|e| Error::invalid(format!("graph <{g}>: {e}")))?,
            ))?,
        };
        let mut outcome = Plan::default();
        // removals edit the graphs the request names and the target it names (§5.4):
        // an axiom read from an imported graph is removed there only when the edit
        // names that graph
        let named = req.graphs();
        let scope: BTreeSet<u32> = o
            .graphs
            .iter()
            .enumerate()
            .filter(|(_, g)| named.contains(g) || edit.graph.as_ref() == Some(*g))
            .map(|(n, _)| n as u32)
            .collect();
        let scope_ids: FxHashSet<Id> = scope
            .iter()
            .filter_map(|n| o.inner.graph_ids.get(*n as usize).copied().flatten())
            .collect();
        let all_graphs: BTreeSet<u32> = (0..o.graphs.len() as u32).collect();
        let imported = |g: u32| {
            Error::invalid(format!(
                "the axiom is read from the imported graph {}: name that graph as the edit's \
                 graph to change it there",
                o.graph_name(g)
            ))
        };
        let mut removal: Vec<Axiom> = edit.remove.clone();
        if let Some(replace) = extra.replace {
            let adding: BTreeSet<&Axiom> = edit.add.iter().collect();
            removal.extend(
                o.ontology
                    .axioms
                    .iter()
                    .filter(|(a, origins)| {
                        replace(a)
                            && !adding.contains(a)
                            && origins.iter().any(|x| scope.contains(&x.graph))
                    })
                    .map(|(a, _)| a.clone()),
            );
        }
        if let Some(iri) = entity {
            removal.extend(
                o.ontology
                    .axioms
                    .iter()
                    .filter(|(a, origins)| {
                        a.mentions(iri) && origins.iter().any(|x| scope.contains(&x.graph))
                    })
                    .map(|(a, _)| a.clone()),
            );
            o.assertions(&opts, |ax, _, g| {
                if let Some(ax) = ax
                    && scope.contains(&g)
                    && ax.mentions(iri)
                {
                    removal.push(ax);
                }
                Ok(true)
            })?;
        }
        // removals first: an axiom removed and added again keeps its triples
        let mut deleted: Vec<[Id; 4]> = Vec::new();
        let mut users: FxHashMap<usize, usize> = FxHashMap::default();
        for origins in o.ontology.axioms.values() {
            for origin in origins {
                for i in &origin.triples {
                    *users.entry(*i).or_insert(0) += 1;
                }
            }
        }
        let removing: BTreeSet<&Axiom> = removal.iter().collect();
        let mut drop: BTreeSet<usize> = BTreeSet::new();
        let mut mains: FxHashSet<usize> = FxHashSet::default();
        // each normal form once, with whether it was found
        let mut done: BTreeMap<Axiom, bool> = BTreeMap::new();
        for ax in &removing {
            let mut found = false;
            for form in ax.normal_forms() {
                if let Some(hit) = done.get(&form) {
                    found |= *hit;
                    continue;
                }
                let hit = match o.ontology.axioms.get(&form) {
                    Some(origins) => {
                        let mut here = false;
                        for origin in origins.iter().filter(|x| scope.contains(&x.graph)) {
                            here = true;
                            mains.extend(origin.main);
                            for i in &origin.triples {
                                let u = users.get_mut(i).expect("counted");
                                *u -= 1;
                                if *u == 0 {
                                    drop.insert(*i);
                                }
                            }
                        }
                        if !here && entity.is_none() {
                            return Err(imported(origins.first().map_or(0, |x| x.graph)));
                        }
                        here
                    }
                    None => match assertion_quads(&o, &form, &txn, &scope)? {
                        Some(qs) => {
                            deleted.extend(qs);
                            true
                        }
                        None => {
                            if entity.is_none()
                                && let Some(qs) = assertion_quads(&o, &form, &txn, &all_graphs)?
                            {
                                let g = o
                                    .inner
                                    .graph_ids
                                    .iter()
                                    .position(|id| *id == Some(qs[0][3]))
                                    .unwrap_or(0);
                                return Err(imported(g as u32));
                            }
                            false
                        }
                    },
                };
                done.insert(form, hit);
                found |= hit;
            }
            if found {
                outcome.removed += 1;
                outcome.removed_axioms.push((*ax).clone());
            } else {
                outcome.absent.push((*ax).clone());
            }
        }
        // a dropped triple stays when a triple outside the dropped set still points at
        // its blank node subject, unless it is the main triple of a removed axiom
        let kept_nodes = referenced_elsewhere(&o, &drop)?;
        for i in drop {
            let q = o.inner.quads[i];
            if kept_nodes.contains(&q[0]) && !mains.contains(&i) {
                outcome.kept += 1;
                continue;
            }
            // an axiom encoded partly in an imported graph changes there only when the
            // edit names that graph
            if !scope_ids.contains(&q[3]) {
                let g = o
                    .inner
                    .graph_ids
                    .iter()
                    .position(|id| *id == Some(q[3]))
                    .unwrap_or(0);
                return Err(imported(g as u32));
            }
            deleted.push(q);
        }
        let mut count_deleted = 0;
        for q in deleted {
            check(&opts)?;
            if txn.delete(q)? {
                count_deleted += 1;
            }
        }
        // additions
        let declare = edit.declare.unwrap_or(true);
        let mut adding: Vec<Axiom> = Vec::new();
        let mut seen: BTreeSet<&Axiom> = BTreeSet::new();
        for ax in &edit.add {
            if !seen.insert(ax) {
                continue;
            }
            // present after the removals above: a form this edit removed is not
            let mut present = true;
            for form in ax.normal_forms() {
                let kept =
                    if done.get(&form) == Some(&true) {
                        // removed here, but it may still be read from a graph outside the
                        // scope, or as an assertion this transaction left
                        o.ontology.axioms.get(&form).is_some_and(|origins| {
                            origins.iter().any(|x| !scope.contains(&x.graph))
                        }) || assertion_quads(&o, &form, &txn, &all_graphs)?.is_some()
                    } else {
                        o.ontology.contains(&form)
                            || assertion_quads(&o, &form, &txn, &all_graphs)?.is_some()
                    };
                if !kept {
                    present = false;
                    break;
                }
            }
            if present {
                outcome.present += 1;
                continue;
            }
            adding.push(ax.clone());
        }
        if declare {
            let declared: FxHashSet<&Entity> = o
                .ontology
                .axiom_set()
                .filter_map(|a| match &a.kind {
                    sparkles_owl::AxiomKind::Declaration(e) => Some(e),
                    _ => None,
                })
                .collect();
            let mut new: BTreeSet<Entity> = BTreeSet::new();
            for ax in &adding {
                if matches!(ax.kind, sparkles_owl::AxiomKind::Declaration(_)) {
                    continue;
                }
                for e in ax.signature() {
                    if e.is_builtin()
                        || declared.contains(&e)
                        || e.kind() == EntityKind::NamedIndividual
                    {
                        continue;
                    }
                    new.insert(e);
                }
            }
            for ax in &adding {
                if let sparkles_owl::AxiomKind::Declaration(e) = &ax.kind {
                    new.remove(e);
                }
            }
            for e in new {
                outcome.declared.push(e.clone());
                adding.push(Axiom::declaration(e));
            }
        }
        let mut n = 0u64;
        let mut fresh = move || {
            n += 1;
            oxrdf::BlankNode::new_unchecked(format!("{FRESH}{n}"))
        };
        let mut labels: FxHashMap<String, Id> = FxHashMap::default();
        let mut inserted = 0;
        for ax in &adding {
            check(&opts)?;
            for t in write::axiom_triples(ax, &mut fresh) {
                let s = subject_term(&t.subject);
                let q = [
                    encode(&mut txn, &s, &mut labels)?,
                    encode(&mut txn, &Term::NamedNode(t.predicate.clone()), &mut labels)?,
                    encode(&mut txn, &t.object, &mut labels)?,
                    target_id,
                ];
                if txn.insert(q)? {
                    inserted += 1;
                }
            }
        }
        let added = adding.len();
        if let Some(hook) = extra.before_commit.as_mut() {
            hook(&mut txn, &head)?;
        }
        let receipt = txn.commit()?;
        Ok(EditOutcome {
            receipt,
            added,
            present: outcome.present,
            removed: outcome.removed,
            absent: outcome.absent,
            declared: outcome.declared,
            inserted,
            deleted: count_deleted,
            kept: outcome.kept,
            added_axioms: adding,
            removed_axioms: outcome.removed_axioms,
        })
    }
}

/// What an edit does besides its axioms, for callers inside the handle.
#[derive(Default)]
struct Extra<'a> {
    /// also remove the axioms of the edited graphs that match and that the edit does not
    /// add, selected from the head under the writer lock
    replace: Option<&'a dyn Fn(&Axiom) -> bool>,
    /// runs under the writer lock after the axioms are written, before the commit
    before_commit: Option<HookFn<'a>>,
}

type HookFn<'a> = &'a mut dyn FnMut(&mut crate::store::WriteTxn<'_>, &Arc<Snapshot>) -> Result<()>;

#[derive(Default)]
struct Plan {
    present: usize,
    removed: usize,
    absent: Vec<Axiom>,
    declared: Vec<Entity>,
    kept: usize,
    removed_axioms: Vec<Axiom>,
}

/// The id of a term an edit writes: a fresh blank node of the forward mapping, or a
/// stored term.
fn encode(
    txn: &mut crate::store::WriteTxn<'_>,
    t: &Term,
    labels: &mut FxHashMap<String, Id>,
) -> Result<Id> {
    if let Term::BlankNode(b) = t
        && b.as_str().starts_with(FRESH)
    {
        if let Some(id) = labels.get(b.as_str()) {
            return Ok(*id);
        }
        let id = txn.new_bnode();
        labels.insert(b.as_str().to_string(), id);
        return Ok(id);
    }
    txn.intern(t).map_err(|e| match e {
        Error::Invalid(m) if matches!(t, Term::BlankNode(_)) => Error::invalid(format!(
            "an anonymous individual of an added axiom must name a stored blank node: {m}"
        )),
        e => e,
    })
}

/// The stored quads of an assertion that the open read left in the store, in every
/// graph read; `None` when it has none. Such an assertion is one triple: the one the
/// forward mapping writes, or for an equality or inequality of two individuals, that
/// triple in either direction.
fn assertion_quads(
    o: &Opened,
    ax: &Axiom,
    txn: &crate::store::WriteTxn<'_>,
    graphs: &BTreeSet<u32>,
) -> Result<Option<Vec<[Id; 4]>>> {
    use sparkles_owl::AxiomKind as K;
    if !ax.annotations.is_empty() {
        return Ok(None);
    }
    let mut fresh = || oxrdf::BlankNode::new_unchecked(format!("{FRESH}x"));
    let mut triples = write::axiom_triples(ax, &mut fresh);
    if triples.len() != 1 {
        return Ok(None);
    }
    if let K::SameIndividual(_) | K::DifferentIndividuals(_) = &ax.kind {
        let t = triples[0].clone();
        if let Some(s) = term_subject(&t.object) {
            triples.push(Triple::new(s, t.predicate, subject_term(&t.subject)));
        }
    }
    let snap = &o.snapshot;
    let mut out = Vec::new();
    for t in &triples {
        let (Some(s), Some(p), Some(obj)) = (
            snap.lookup_term(&subject_term(&t.subject)),
            snap.lookup_iri(t.predicate.as_str()),
            snap.lookup_term(&t.object),
        ) else {
            continue;
        };
        if o.ontology.map_assertion(t).as_ref() != Some(ax) {
            continue;
        }
        for (n, g) in o.inner.graph_ids.iter().enumerate() {
            let Some(g) = g else { continue };
            if !graphs.contains(&(n as u32)) {
                continue;
            }
            let q = [s, p, obj, *g];
            // through the caller's view, so a protected triple is absent, and as this
            // transaction leaves it
            if snap.contains(&q)? && txn.contains(&q)? {
                out.push(q);
            }
        }
    }
    Ok((!out.is_empty()).then_some(out))
}

fn subject_term(s: &NamedOrBlankNode) -> Term {
    match s {
        NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n.clone()),
        NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.clone()),
    }
}

fn term_subject(t: &Term) -> Option<NamedOrBlankNode> {
    match t {
        Term::NamedNode(n) => Some(NamedOrBlankNode::NamedNode(n.clone())),
        Term::BlankNode(b) => Some(NamedOrBlankNode::BlankNode(b.clone())),
        _ => None,
    }
}

/// The blank nodes among the subjects of `drop` that a triple outside `drop` uses as
/// its object, in the graphs read: their structures stay.
fn referenced_elsewhere(o: &Opened, drop: &BTreeSet<usize>) -> Result<FxHashSet<Id>> {
    let snap = &o.snapshot;
    let mut nodes: FxHashSet<Id> = FxHashSet::default();
    for i in drop {
        let q = o.inner.quads[*i];
        if q[0].tag() == crate::id::Tag::BNode {
            nodes.insert(q[0]);
        }
    }
    let dropped: FxHashSet<[Id; 4]> = drop.iter().map(|i| o.inner.quads[*i]).collect();
    let read: FxHashSet<Id> = o.inner.graph_ids.iter().flatten().copied().collect();
    let mut kept = FxHashSet::default();
    for n in &nodes {
        for k in snap.scan_keys(Perm::Osp, &[n.0])? {
            let q = Perm::Osp.to_quad(&k);
            if read.contains(&q[3]) && !dropped.contains(&q) {
                kept.insert(*n);
            }
        }
    }
    // a kept node keeps the nodes its own triples lead to
    let mut stack: Vec<Id> = kept.iter().copied().collect();
    while let Some(n) = stack.pop() {
        for q in &dropped {
            if q[0] == n && nodes.contains(&q[2]) && kept.insert(q[2]) {
                stack.push(q[2]);
            }
        }
    }
    Ok(kept)
}

/// Check an open ontology.
pub fn check_opened(o: &Opened, req: &CheckRequest, opts: &QueryOptions) -> Result<Vec<Report>> {
    let opts = &*crate::overview::one_deadline(opts)?;
    let copts = CheckOptions {
        max_violations: req.max_violations,
    };
    let mut c = Checker::new(&o.ontology, &req.profiles, copts);
    for (ax, origins) in &o.ontology.axioms {
        c.axiom(ax, origins.first().map(|x| x.graph));
    }
    if req.assertions {
        let mut n = 0usize;
        o.assertions(opts, |ax, t, g| {
            n += 1;
            if n.is_multiple_of(4096) {
                check(opts)?;
            }
            match ax {
                Some(ax) => c.axiom(&ax, Some(g)),
                None => c.unmapped(t, Some(g)),
            }
            Ok(true)
        })?;
    }
    Ok(c.finish())
}

/// Read the terminology of the graphs `graph_ids` (`None`: a graph without quads or one
/// the caller cannot read) from `snap`.
fn read_store(
    snap: &Arc<Snapshot>,
    graph_ids: &[Option<Id>],
    requested: usize,
    mode: Mode,
    opts: &QueryOptions,
) -> Result<(read::Ontology, Inner)> {
    let graph_of_id: FxHashMap<Id, u32> = graph_ids
        .iter()
        .enumerate()
        .filter_map(|(n, g)| g.map(|g| (g, n as u32)))
        .collect();
    let mut c = Collect {
        snap,
        graphs: &graph_of_id,
        quads: Vec::new(),
        seen: FxHashSet::default(),
        opts,
    };
    let mut terminological: FxHashSet<Id> = FxHashSet::default();
    let predicates = vocab::STRUCTURAL_PREDICATES
        .iter()
        .chain(vocab::FACETS.iter())
        .chain(vocab::BUILTIN_ANNOTATION_PROPERTIES.iter());
    for p in predicates {
        if let Some(id) = snap.lookup_iri(p)
            && terminological.insert(id)
        {
            check(opts)?;
            c.prefix(&[id.0])?;
        }
    }
    if let Some(rdf_type) = snap.lookup_iri(vocab::RDF_TYPE) {
        let before = c.quads.len();
        for t in vocab::STRUCTURAL_TYPES {
            if let Some(o) = snap.lookup_iri(t) {
                check(opts)?;
                c.prefix(&[rdf_type.0, o.0])?;
            }
        }
        // the annotation properties declared in the graphs read
        let ap = snap.lookup_iri(vocab::OWL_ANNOTATION_PROPERTY);
        let declared: Vec<Id> = c.quads[before..]
            .iter()
            .filter(|q| Some(q[2]) == ap)
            .map(|q| q[0])
            .collect();
        for p in declared {
            if terminological.insert(p) {
                check(opts)?;
                c.prefix(&[p.0])?;
            }
        }
        // class assertions whose class is a class expression: `x rdf:type _:c` for each
        // blank node subject read so far
        let bnodes: BTreeSet<Id> = c
            .quads
            .iter()
            .filter(|q| q[0].tag() == crate::id::Tag::BNode)
            .map(|q| q[0])
            .collect();
        for b in bnodes {
            check(opts)?;
            c.prefix(&[rdf_type.0, b.0])?;
        }
    }
    // the main triples of reified axioms whose predicate was not read: an annotated
    // assertion keeps its main triple outside the terminology's predicates
    let mut mains: FxHashSet<[Id; 4]> = FxHashSet::default();
    let ids = |p: &str| snap.lookup_iri(p);
    if let (Some(src), Some(prop), Some(tgt)) = (
        ids(vocab::OWL_ANNOTATED_SOURCE),
        ids(vocab::OWL_ANNOTATED_PROPERTY),
        ids(vocab::OWL_ANNOTATED_TARGET),
    ) {
        let mut parts: FxHashMap<(Id, Id), [Option<Id>; 3]> = FxHashMap::default();
        for q in &c.quads {
            let slot = if q[1] == src {
                0
            } else if q[1] == prop {
                1
            } else if q[1] == tgt {
                2
            } else {
                continue;
            };
            parts.entry((q[0], q[3])).or_default()[slot] = Some(q[2]);
        }
        let graphs: Vec<Id> = graph_of_id.keys().copied().collect();
        for (_, part) in parts {
            let [Some(s), Some(p), Some(o)] = part else {
                continue;
            };
            check(opts)?;
            for g in &graphs {
                let q = [s, p, o, *g];
                if !terminological.contains(&p) && snap.contains(&q)? && c.seen.insert(q) {
                    c.quads.push(q);
                    mains.insert(q);
                }
            }
        }
    }
    let quads = c.quads;
    finish(
        snap,
        quads,
        &graph_of_id,
        graph_ids.len(),
        requested,
        mode,
        (terminological, mains),
    )
}

/// The quads of the graphs read that POS prefixes select, each once.
struct Collect<'a> {
    snap: &'a Snapshot,
    graphs: &'a FxHashMap<Id, u32>,
    quads: Vec<[Id; 4]>,
    seen: FxHashSet<[Id; 4]>,
    opts: &'a QueryOptions,
}

impl Collect<'_> {
    fn prefix(&mut self, prefix: &[u64]) -> Result<()> {
        let mut n = 0usize;
        let (graphs, seen, quads, opts) = (self.graphs, &mut self.seen, &mut self.quads, self.opts);
        let mut each = |q: [Id; 4]| -> Result<()> {
            n += 1;
            if n.is_multiple_of(4096) {
                check(opts)?;
            }
            if graphs.contains_key(&q[3]) && seen.insert(q) {
                quads.push(q);
            }
            Ok(())
        };
        self.snap.scan(Perm::Pos, prefix, |c| {
            match c {
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        each(Perm::Pos.to_quad(&b.key(i)))?;
                    }
                }
                Chunk::Row(k) => each(Perm::Pos.to_quad(&k))?,
            }
            Ok(true)
        })
    }
}

fn finish(
    snap: &Snapshot,
    quads: Vec<[Id; 4]>,
    graph_of_id: &FxHashMap<Id, u32>,
    graphs: usize,
    requested: usize,
    mode: Mode,
    (terminological, mains): (FxHashSet<Id>, FxHashSet<[Id; 4]>),
) -> Result<(read::Ontology, Inner)> {
    let mut triples = Vec::with_capacity(quads.len());
    let mut graph_of = Vec::with_capacity(quads.len());
    let mut kept = Vec::with_capacity(quads.len());
    for q in quads {
        if let Some(t) = triple_of(snap, &q) {
            triples.push(t);
            graph_of.push(graph_of_id[&q[3]]);
            kept.push(q);
        }
    }
    let primary: Vec<u32> = (0..requested.min(graphs) as u32).collect();
    let o = read::read(
        triples,
        graph_of,
        &primary,
        ReadOptions {
            mode,
            terminology_only: true,
            max_diagnostics: 0,
        },
    );
    let graph_ids = {
        let mut ids = vec![None; graphs];
        for (id, n) in graph_of_id {
            ids[*n as usize] = Some(*id);
        }
        ids
    };
    Ok((
        o,
        Inner {
            quads: kept,
            graph_ids,
            terminological,
            mains,
        },
    ))
}

/// The most changed quads an ontology diff reads from the store's diff.
pub const DIFF_MAX_QUADS: u64 = 10_000_000;

/// One axiom of an ontology diff, with the graph it is in.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AxiomChange {
    pub axiom: Axiom,
    pub graph: String,
}

/// The axioms that differ between two states of an ontology (spec F21 Phase 3).
#[derive(Clone, Debug)]
pub struct OntologyDiff {
    /// the commits of the two states
    pub from: u64,
    pub to: u64,
    /// the axioms of `to` that `from` lacks, in order
    pub added: Vec<AxiomChange>,
    /// the axioms of `from` that `to` lacks, in order
    pub removed: Vec<AxiomChange>,
    /// the quads of the store's diff in the graphs the two reads cover
    pub changed_quads: u64,
    /// how the store's diff was computed
    pub method: crate::store::DiffMethod,
}

impl Ontologies {
    /// The graphs a read of `req` at `snap` covers through the caller's view: the
    /// requested graphs and the imports that resolve to graphs it may read, without
    /// reading their terminology.
    fn graphs_at(
        &self,
        snap: &Arc<Snapshot>,
        req: &OntologyRequest,
        opts: &QueryOptions,
    ) -> Result<Vec<GraphRef>> {
        let access = opts.graphs.as_ref().map(|a| a.bound(snap));
        let view = match &access {
            Some(a) => a.masked(snap)?,
            None => snap.clone(),
        };
        let readable = |g: Id| access.as_ref().is_none_or(|a| a.readable_id(snap, g));
        Ok(req.resolve(&view, &readable).0.graphs)
    }

    /// The axioms that differ between the states `from` and `to` of the ontology `req`
    /// names (its `at` is ignored), as the caller of `opts` may read them. The
    /// terminology of each state is read as [`open`](Self::open) reads it and the two
    /// sets of axioms are compared. With `assertions`, the assertions that the store's
    /// diff of triples ([`crate::store::Store::diff`]) adds or removes in the graphs read
    /// are mapped one triple at a time, as [`Opened::assertions`] maps them, so an
    /// assertion that no changed triple carries is not read. When nothing changed in the
    /// graphs read, nothing is opened and the diff is empty.
    pub fn diff(
        &self,
        req: &OntologyRequest,
        from: &At,
        to: &At,
        assertions: bool,
        opts: &QueryOptions,
    ) -> Result<OntologyDiff> {
        req.validate()?;
        if req.inferences {
            return Err(Error::invalid(
                "a diff compares the asserted axioms: leave out 'inferences'",
            ));
        }
        let opts = self.anchored(opts)?;
        let store = self.ds.store();
        let hist = HistoryOptions {
            cancel: opts.cancel.clone(),
            deadline: opts.deadline,
        };
        // both selectors resolve once, and the triple diff reads those two commits, so a
        // commit that lands meanwhile does not mix into the answer
        let (a, _) = store.snapshot_at(from, &hist)?;
        let (b, _) = store.snapshot_at(to, &hist)?;
        let mut req = req.clone();
        req.at = None;
        // the graphs a read of either state covers: the requested ones and their imports
        let mut covered: Vec<GraphRef> = self.graphs_at(&a, &req, &opts)?;
        for g in self.graphs_at(&b, &req, &opts)? {
            if !covered.contains(&g) {
                covered.push(g);
            }
        }
        let graph_name = |g: &GraphRef| match g {
            GraphRef::Default => oxrdf::GraphName::DefaultGraph,
            GraphRef::Named(n) => oxrdf::GraphName::NamedNode(oxrdf::NamedNode::new_unchecked(n)),
        };
        let triples = store.diff(
            &At::Commit(a.commit),
            &At::Commit(b.commit),
            &crate::store::DiffOptions {
                max_quads: DIFF_MAX_QUADS,
                cancel: opts.cancel.clone(),
                deadline: opts.deadline,
                graphs: opts.graphs.clone(),
                // one graph is selected in the store's diff, which then holds only its
                // changes; several are filtered below
                graph: (covered.len() == 1).then(|| graph_name(&covered[0])),
                ..Default::default()
            },
        )?;
        let mut out = OntologyDiff {
            from: a.commit,
            to: b.commit,
            added: Vec::new(),
            removed: Vec::new(),
            changed_quads: 0,
            method: triples.method,
        };
        let graphs_of =
            |snap: &Arc<Snapshot>| -> Result<Opened> { self.open_at(snap, &req, &opts) };
        let changed_in = |o: &Opened, g: &oxrdf::GraphName| {
            o.graphs.iter().any(|r| match (r, g) {
                (GraphRef::Default, oxrdf::GraphName::DefaultGraph) => true,
                (GraphRef::Named(n), oxrdf::GraphName::NamedNode(m)) => n == m.as_str(),
                _ => false,
            })
        };
        let names: Vec<oxrdf::GraphName> = covered.iter().map(graph_name).collect();
        out.changed_quads = triples
            .iter()
            .filter(|(_, q)| names.contains(&q.graph_name))
            .count() as u64;
        // nothing changed in the graphs read: neither state is opened
        if out.changed_quads == 0 {
            return Ok(out);
        }
        let oa = graphs_of(&a)?;
        let ob = graphs_of(&b)?;
        check(&opts)?;
        let graph = |o: &Opened, ax: &Axiom| -> String {
            o.ontology
                .axioms
                .get(ax)
                .and_then(|v| v.first())
                .map_or_else(String::new, |x| o.graph_name(x.graph).to_string())
        };
        for ax in ob.ontology.axiom_set() {
            if !oa.ontology.contains(ax) {
                out.added.push(AxiomChange {
                    axiom: ax.clone(),
                    graph: graph(&ob, ax),
                });
            }
        }
        for ax in oa.ontology.axiom_set() {
            if !ob.ontology.contains(ax) {
                out.removed.push(AxiomChange {
                    axiom: ax.clone(),
                    graph: graph(&oa, ax),
                });
            }
        }
        if assertions {
            let (ka, kb) = (annotated_kinds(&oa), annotated_kinds(&ob));
            let mut added = BTreeSet::new();
            let mut removed = BTreeSet::new();
            for (op, q) in triples.iter() {
                let (o, kinds, set) = match op {
                    crate::store::DiffOp::Add => (&ob, &kb, &mut added),
                    crate::store::DiffOp::Remove => (&oa, &ka, &mut removed),
                };
                if !changed_in(o, &q.graph_name) {
                    continue;
                }
                let t = Triple::new(q.subject, q.predicate, q.object);
                if let Some(ax) = assertion_of(o, kinds, &t) {
                    let g = match &q.graph_name {
                        oxrdf::GraphName::NamedNode(n) => n.as_str().to_string(),
                        _ => "default".to_string(),
                    };
                    set.insert(AxiomChange {
                        axiom: ax,
                        graph: g,
                    });
                }
            }
            // an annotated assertion that lost or gained its annotations keeps its
            // triple, which the store's diff then does not name: the plain assertion
            // appears or disappears with them
            for (ours, theirs, kinds, snap, set) in [
                (&out.removed, &ob, &kb, &ob.snapshot, &mut added),
                (&out.added, &oa, &ka, &oa.snapshot, &mut removed),
            ] {
                for c in ours {
                    if c.axiom.annotations.is_empty() || !c.axiom.axiom_type().is_assertion() {
                        continue;
                    }
                    let plain = c.axiom.without_annotations();
                    if theirs.ontology.contains(&plain) || kinds.contains(&plain.kind) {
                        continue;
                    }
                    if holds_main(snap, theirs, &plain) {
                        set.insert(AxiomChange {
                            axiom: plain,
                            graph: c.graph.clone(),
                        });
                    }
                }
            }
            out.added.extend(added);
            out.removed.extend(removed);
        }
        Ok(out)
    }
}

/// The kinds of the annotated assertions of the terminology, whose main triples a read
/// of the assertions skips.
fn annotated_kinds(o: &Opened) -> FxHashSet<sparkles_owl::AxiomKind> {
    o.ontology
        .axiom_set()
        .filter(|a| !a.annotations.is_empty() && a.axiom_type().is_assertion())
        .map(|a| a.kind.clone())
        .collect()
}

/// The assertion a triple outside the terminology of `o` maps to, as
/// [`Opened::assertions`] maps it.
fn assertion_of(
    o: &Opened,
    annotated: &FxHashSet<sparkles_owl::AxiomKind>,
    t: &Triple,
) -> Option<Axiom> {
    let annotation_property = |p: &str| {
        o.ontology
            .typing
            .is_declared(p, EntityKind::AnnotationProperty)
    };
    if read::is_terminological(t, &annotation_property) {
        return None;
    }
    let ax = o.ontology.map_assertion(t)?;
    if annotated.contains(&ax.kind) {
        return None;
    }
    Some(ax)
}

/// Whether the state holds the one triple of a plain assertion in a graph `o` reads.
fn holds_main(snap: &Snapshot, o: &Opened, plain: &Axiom) -> bool {
    let mut fresh = write::counter(FRESH);
    let ts = write::axiom_triples(plain, &mut fresh);
    let [t] = ts.as_slice() else { return false };
    let ids = (
        snap.lookup_term(&t.subject.clone().into()),
        snap.lookup_term(&Term::NamedNode(t.predicate.clone())),
        snap.lookup_term(&t.object),
    );
    let (Some(s), Some(p), Some(x)) = ids else {
        return false;
    };
    // an equality or inequality of two individuals is either triple's axiom
    let symmetric = matches!(
        plain.kind,
        sparkles_owl::AxiomKind::SameIndividual(_)
            | sparkles_owl::AxiomKind::DifferentIndividuals(_)
    );
    o.inner.graph_ids.iter().flatten().any(|g| {
        snap.contains(&[s, p, x, *g]).unwrap_or(false)
            || symmetric && snap.contains(&[x, p, s, *g]).unwrap_or(false)
    })
}

/// The profiles of a list such as `el,ql`.
pub fn parse_profiles(s: &str) -> Result<Vec<Profile>> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            if p.eq_ignore_ascii_case("all") {
                return Ok(Profile::ALL.to_vec());
            }
            Profile::parse(p).map(|p| vec![p]).ok_or_else(|| {
                Error::invalid(format!("unknown profile '{p}': el, ql, rl, dl or all"))
            })
        })
        .collect::<Result<Vec<Vec<Profile>>>>()
        .map(|v| v.into_iter().flatten().collect())
}

/// The profiles' reports, for the JSON forms.
pub fn report_json(
    o: &Opened,
    r: &Report,
    render: &sparkles_owl::ofn::Renderer,
) -> serde_json::Value {
    report_view_json(&o.view(), r, render)
}

/// [`report_json`] for the read `o` describes.
pub fn report_view_json(
    o: &ReadView<'_>,
    r: &Report,
    render: &sparkles_owl::ofn::Renderer,
) -> serde_json::Value {
    let violations: Vec<serde_json::Value> = r
        .violations
        .iter()
        .map(|v| {
            let mut j = serde_json::json!({ "rule": v.rule, "message": v.message });
            if let Some(ax) = &v.axiom {
                j["axiom"] = render.axiom(ax).into();
            }
            if let Some(g) = v.graph {
                j["graph"] = o.graph_name(g).into();
            }
            j
        })
        .collect();
    serde_json::json!({
        "profile": r.profile.as_str(),
        "title": r.profile.title(),
        "conforms": r.conforms,
        "checked": r.checked,
        "violations": violations,
        "total": r.total,
        "truncated": r.truncated,
    })
}

/// The kinds of axioms a filter names, such as `SubClassOf,ClassAssertion`.
pub fn parse_axiom_types(s: &str) -> Result<BTreeSet<AxiomType>> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| {
            AxiomType::parse(t).ok_or_else(|| Error::invalid(format!("unknown axiom type '{t}'")))
        })
        .collect()
}

// --------------------------------------------------- reasoning runs (§7) ------

/// The size of an axiom that a gap of [`sparkles_reasoner::OWL_RL_GAPS`] limits: the
/// properties of a chain (`prp-spo2`) or of a key (`prp-key`).
fn gap_size(rule: &str, ax: &Axiom) -> Option<usize> {
    use sparkles_owl::{AxiomKind as K, SubObjectPropertyExpression as S};
    match (rule, &ax.kind) {
        ("prp-spo2", K::SubObjectPropertyOf(S::Chain(c), _)) => Some(c.len()),
        ("prp-key", K::HasKey(_, ops, dps)) => Some(ops.len() + dps.len()),
        _ => None,
    }
}

/// The gaps of the OWL 2 RL rule subset: each rule, the largest size it covers and
/// what the size counts.
pub fn rl_gaps() -> Vec<(&'static str, usize, &'static str)> {
    sparkles_reasoner::OWL_RL_GAPS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut parts = l.splitn(3, ' ');
            let rule = parts.next()?;
            let size = parts.next()?.parse().ok()?;
            Some((rule, size, parts.next().unwrap_or("").trim()))
        })
        .collect()
}

/// The warnings of an `owl-rl` materialization over `inputs` in the state `snap`
/// (spec F21 §7):
/// how many axioms of the input graphs and the imports they resolve to lie outside
/// OWL 2 RL, for which the rules may miss entailments (OWL 2 Profiles §4.3, Theorem
/// PR1), and how many axioms in OWL 2 RL need a rule that the rule subset covers only
/// up to a size ([`sparkles_reasoner::OWL_RL_GAPS`]). The terminology is read as
/// [`Ontologies::open`] reads it, leniently, and split by the OWL 2 RL grammar alone
/// ([`sparkles_owl::profiles::partition`]). Assertions are not streamed, since a class
/// assertion with a class expression is read with the terminology and the other
/// assertions are always in OWL 2 RL. Fetching imports is the run's work, so an import
/// is followed only to a graph of the dataset. The answer is empty when everything is
/// in OWL 2 RL and covered. `prefixes` abbreviate the IRIs of the axioms the warnings
/// quote.
pub fn rl_warnings(
    snap: &Arc<Snapshot>,
    prefixes: &BTreeMap<String, String>,
    inputs: &Inputs,
    opts: &QueryOptions,
) -> Result<Vec<String>> {
    let inputs = Inputs {
        data_graphs: inputs.configured(),
        ontology_graphs: Vec::new(),
        imports: match inputs.imports {
            ImportMode::None => ImportMode::None,
            _ => ImportMode::Dataset,
        },
        location_mapping: inputs.location_mapping.clone(),
    };
    // an import is followed only to a graph the view reads
    let readable = |g: Id| {
        opts.graphs
            .as_ref()
            .is_none_or(|a| a.bound(snap).readable_id(snap, g))
    };
    let resolved = sparkles_reasoner::inputs::resolve_readable(snap, &inputs, &readable);
    let graph_ids: Vec<Option<Id>> = resolved.graphs.iter().map(|g| g.id(snap)).collect();
    let requested = inputs.data_graphs.len();
    let (ontology, _) = read_store(snap, &graph_ids, requested, Mode::Lenient, opts)?;
    let part = sparkles_owl::profiles::partition(&ontology, Profile::Rl);
    let render = sparkles_owl::ofn::Renderer::new(prefixes.clone());
    let mut out = Vec::new();
    let outside = part.outside_axioms();
    if let Some(first) = part.outside.first() {
        let n = outside.len();
        out.push(format!(
            "{n} axiom{} outside OWL 2 RL, for which the OWL 2 RL rules may not derive every \
             entailment (OWL 2 Profiles §4.3); the first is {} ({}: {})",
            if n == 1 { " is" } else { "s are" },
            render.axiom(first.axiom.as_ref().expect("a partition names its axioms")),
            first.rule,
            first.message,
        ));
    }
    for (rule, max, what) in rl_gaps() {
        let over: Vec<&Axiom> = part
            .inside
            .iter()
            .filter(|ax| gap_size(rule, ax).is_some_and(|n| n > max))
            .collect();
        if let Some(first) = over.first() {
            let n = over.len();
            out.push(format!(
                "{n} axiom{} in OWL 2 RL need{} rule {rule} for more than {max} {what}, which \
                 the rule set does not have, so its entailments are not derived; the first \
                 is {}",
                if n == 1 { "" } else { "s" },
                if n == 1 { "s" } else { "" },
                render.axiom(first),
            ));
        }
    }
    Ok(out)
}

/// The [`rl_warnings`] of an `owl-rl` run over `inputs` in `store`, as a check the run
/// makes of the state it reads under the writer lock
/// ([`sparkles_reasoner::ReasonOptions::inspect`]).
pub fn rl_inspector(
    store: &crate::store::Store,
    inputs: &Inputs,
    mut opts: QueryOptions,
) -> sparkles_reasoner::InspectFn {
    // the store's hidden control graphs (spec C26 §2.3) are never inspected
    opts.graphs = crate::access::GraphAccess::hiding_graphs(opts.graphs, &store.hidden_graphs());
    let prefixes = store.prefixes();
    let inputs = inputs.clone();
    Arc::new(move |snap| Ok(rl_warnings(snap, &prefixes, &inputs, &opts)?))
}

pub mod browse;
pub mod classify;
pub mod dl_query;
pub mod file;
pub mod kept;
pub mod metrics;
pub mod search;
pub mod transform;
pub mod wire;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod individual_lookup_tests;
