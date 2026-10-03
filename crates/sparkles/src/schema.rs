//! Schema discovery: one report of the classes and predicates of a dataset, computed
//! from a single [`Snapshot`].
//!
//! The report has two layers that are kept apart:
//!
//! - **observed**: exact counts over the selected graphs at the snapshot (distinct
//!   triples, subjects, objects, object kinds, literal datatypes and languages, the
//!   largest number of objects of one subject). These are measurements, not guarantees:
//!   the next write may change any of them.
//! - **declared**: what the RDFS/OWL vocabulary in the data asserts (`rdf:type`
//!   `owl:Class`, `rdfs:subClassOf`, `rdfs:domain`, labels, ...), read from the
//!   declared graphs.
//!
//! A third layer, **constraints**, lists the SHACL shapes the dataset uses per target
//! class ([`constraints`]). This crate holds its types, and `sparkles_shacl::constraints`
//! builds it, so the report never derives a constraint from the counts.
//!
//! On request, each predicate also lists the classes of its subjects
//! ([`SchemaOptions::subject_classes`]), from a merge join of the predicate's subjects
//! with the `rdf:type` triples, both in subject order.
//!
//! Counting uses the permutation indexes directly: one ordered pass over PSO and one over
//! POS per predicate, which merge the base index with the delta, so the numbers are exact
//! after updates. A triple stored in several selected graphs counts once, as in SPARQL's
//! union graph. The caller passes the name of the graph of materialized inferences, if
//! any, so it can be included in or kept out of the selection.

use crate::error::Error;
use crate::id::{Id, KEY_SEP, Tag};
use crate::index::{Key, Perm};
use crate::store::{Chunk, Snapshot};
use oxrdf::{NamedNode, Term};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Version of the JSON shape of [`SchemaReport`] and [`SchemaSummary`].
pub const SCHEMA_FORMAT: u32 = 1;

/// Default cap on the number of classes, and separately of predicates, in one report.
pub const DEFAULT_MAX_ENTRIES: usize = 1_000_000;

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const SH: &str = "http://www.w3.org/ns/shacl#";

/// Namespaces whose terms are flagged `builtin`.
const BUILTIN_NAMESPACES: [&str; 5] = [RDF, RDFS, OWL, XSD, SH];

/// `rdf:type` objects that declare a class.
const CLASS_TYPES: [&str; 3] = [
    "http://www.w3.org/2000/01/rdf-schema#Class",
    "http://www.w3.org/2002/07/owl#Class",
    "http://www.w3.org/2000/01/rdf-schema#Datatype",
];

/// `rdf:type` objects that declare a property.
const PROPERTY_TYPES: [&str; 13] = [
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#Property",
    "http://www.w3.org/2002/07/owl#ObjectProperty",
    "http://www.w3.org/2002/07/owl#DatatypeProperty",
    "http://www.w3.org/2002/07/owl#AnnotationProperty",
    "http://www.w3.org/2002/07/owl#OntologyProperty",
    "http://www.w3.org/2002/07/owl#FunctionalProperty",
    "http://www.w3.org/2002/07/owl#InverseFunctionalProperty",
    "http://www.w3.org/2002/07/owl#TransitiveProperty",
    "http://www.w3.org/2002/07/owl#SymmetricProperty",
    "http://www.w3.org/2002/07/owl#AsymmetricProperty",
    "http://www.w3.org/2002/07/owl#ReflexiveProperty",
    "http://www.w3.org/2002/07/owl#IrreflexiveProperty",
    "http://www.w3.org/2002/07/owl#DeprecatedProperty",
];

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";
const RDF_DIR_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#dirLangString";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDFS_COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
const OWL_ONTOLOGY: &str = "http://www.w3.org/2002/07/owl#Ontology";
const OWL_VERSION_INFO: &str = "http://www.w3.org/2002/07/owl#versionInfo";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// Class axioms: the subject and IRI objects are classes; blank-node objects are
/// anonymous class expressions.
#[derive(Clone, Copy)]
enum ClassRel {
    SubClassOf,
    EquivalentClass,
    DisjointWith,
}

const CLASS_RELS: [(&str, ClassRel); 3] = [
    (
        "http://www.w3.org/2000/01/rdf-schema#subClassOf",
        ClassRel::SubClassOf,
    ),
    (
        "http://www.w3.org/2002/07/owl#equivalentClass",
        ClassRel::EquivalentClass,
    ),
    (
        "http://www.w3.org/2002/07/owl#disjointWith",
        ClassRel::DisjointWith,
    ),
];

/// Property axioms: the subject is a property; `rdfs:domain`/`rdfs:range` objects are
/// classes (blank ones are anonymous class expressions), `rdfs:subPropertyOf` and
/// `owl:inverseOf` objects are properties.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PropRel {
    Domain,
    Range,
    SubPropertyOf,
    InverseOf,
}

const PROP_RELS: [(&str, PropRel); 4] = [
    (
        "http://www.w3.org/2000/01/rdf-schema#domain",
        PropRel::Domain,
    ),
    ("http://www.w3.org/2000/01/rdf-schema#range", PropRel::Range),
    (
        "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
        PropRel::SubPropertyOf,
    ),
    (
        "http://www.w3.org/2002/07/owl#inverseOf",
        PropRel::InverseOf,
    ),
];

// --------------------------------------------------------------------- options ------

/// Which graphs a report reads.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GraphSelection {
    /// The default graph (every graph when the store treats the default graph as the
    /// union of all graphs).
    Default,
    /// Every graph.
    Union,
    /// One named graph.
    Named(NamedNode),
}

impl GraphSelection {
    /// `default`, `union`, `urn:x-arq:DefaultGraph`, `urn:x-arq:UnionGraph`, or a graph
    /// IRI (optionally in angle brackets).
    pub fn parse(s: &str) -> Result<GraphSelection, String> {
        Ok(match s.trim() {
            "" | "default" | crate::sparql::ctx::DEFAULT_GRAPH_IRI => GraphSelection::Default,
            "union" | crate::sparql::ctx::UNION_GRAPH_IRI => GraphSelection::Union,
            iri => {
                let iri = iri
                    .strip_prefix('<')
                    .and_then(|i| i.strip_suffix('>'))
                    .unwrap_or(iri);
                GraphSelection::Named(
                    NamedNode::new(iri).map_err(|e| format!("invalid graph IRI '{iri}': {e}"))?,
                )
            }
        })
    }

    /// The canonical parameter value: `default`, `union` or the graph IRI.
    pub fn name(&self) -> &str {
        match self {
            GraphSelection::Default => "default",
            GraphSelection::Union => "union",
            GraphSelection::Named(n) => n.as_str(),
        }
    }
}

/// Parameters of [`discover`].
#[derive(Clone, Debug)]
pub struct SchemaOptions {
    /// Graphs whose triples are counted (the observed layer).
    pub graph: GraphSelection,
    /// Graphs read for declarations; `None` means the same as `graph`.
    pub declared_graph: Option<GraphSelection>,
    /// Name of the graph of materialized inferences, if the dataset has one.
    pub inferred_graph: Option<String>,
    /// Count the inferred graph as part of `default` / `union` (observed layer).
    pub include_inferred: bool,
    /// Read declarations from the inferred graph too (otherwise asserted ones only).
    pub declared_from_inferred: bool,
    /// Fail with [`SchemaError::Timeout`] after this instant.
    pub deadline: Option<Instant>,
    /// Fail with [`SchemaError::Cancelled`] once this flag is set.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Fail with [`SchemaError::TooManyEntries`] above this many classes or predicates.
    pub max_entries: usize,
    /// Also count the distinct subjects, objects and IRI subjects of the whole selection
    /// ([`SchemaReport::term_totals`], which the VoID export needs). This costs one more
    /// pass over the SPO and the OSP index.
    pub term_totals: bool,
    /// The graphs the caller may read (`None`: every graph). The report covers only
    /// these, and a hidden graph named by `graph` or `declared_graph` is reported as
    /// missing.
    pub graphs: Option<Arc<crate::access::GraphAccess>>,
    /// Also list, for each predicate, the classes of its subjects
    /// ([`PredicateObserved::subject_classes`]). This costs one pass over the `rdf:type`
    /// triples of the selection and memory for them.
    pub subject_classes: bool,
}

impl Default for SchemaOptions {
    fn default() -> Self {
        SchemaOptions {
            graph: GraphSelection::Default,
            declared_graph: None,
            inferred_graph: None,
            include_inferred: true,
            declared_from_inferred: false,
            deadline: None,
            cancel: None,
            max_entries: DEFAULT_MAX_ENTRIES,
            term_totals: false,
            graphs: None,
            subject_classes: false,
        }
    }
}

/// Why a report could not be computed. A report is never returned partially.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("no such graph: <{0}>")]
    NoSuchGraph(String),
    /// The deadline passed; `phase` says what was running, e.g.
    /// `scanning predicates (412/9031)`.
    #[error("schema discovery timed out while {phase}")]
    Timeout { phase: String },
    #[error("schema discovery cancelled")]
    Cancelled,
    #[error("dataset has {count} {kind} (limit {limit})")]
    TooManyEntries {
        kind: &'static str,
        count: usize,
        limit: usize,
    },
    #[error(transparent)]
    Store(#[from] Error),
}

// ---------------------------------------------------------------------- report ------

/// The identity a report and its pagination cursors are bound to.
///
/// This is the in-process snapshot version: it changes on every commit and compaction
/// and restarts at 0 when the store is reopened. Once snapshots carry a durable commit
/// sequence, this is the one place to switch to it.
pub fn snapshot_identity(snap: &Snapshot) -> u64 {
    snap.version
}

/// A language-tagged or plain literal (labels, comments, version info).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Lit {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotInfo {
    /// [`snapshot_identity`] of the snapshot the report was computed at.
    pub version: u64,
    /// The commit the report was computed at.
    pub commit: u64,
    /// Name of the base index generation.
    pub generation: String,
    /// RFC 3339 time the report was computed.
    pub computed_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub graph: String,
    pub declared_graph: String,
    pub reasoning: bool,
    /// `asserted` or `all`
    pub declared: &'static str,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    /// Distinct triples in the selection.
    pub triples: u64,
    pub classes: usize,
    pub predicates: usize,
    /// Distinct blank-node objects of `rdf:type`.
    pub anonymous_type_targets: u64,
    /// Distinct blank-node objects of class axioms and of `rdfs:domain` / `rdfs:range`.
    pub anonymous_class_expressions: u64,
}

/// Distinct terms of the whole selection ([`SchemaOptions::term_totals`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TermTotals {
    /// Distinct subjects of the selected triples.
    pub distinct_subjects: u64,
    /// Distinct objects of the selected triples (terms, not values).
    pub distinct_objects: u64,
    /// Distinct subjects that are IRIs (VoID's entities).
    pub entities: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OntologyEntry {
    pub iri: String,
    pub labels: Vec<Lit>,
    pub version_info: Vec<Lit>,
    pub comments: Vec<Lit>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Hierarchy {
    /// Classes with no declared superclass outside their own strongly connected
    /// component (one representative per cycle), most subclasses first, then by IRI.
    pub roots: Vec<String>,
    /// `rdfs:subClassOf` cycles: components with more than one member, sorted.
    pub cycles: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ClassObserved {
    /// Distinct subjects typed with the class in the selection.
    pub instances: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassDeclared {
    /// Subset of `rdfs:Class`, `owl:Class`, `rdfs:Datatype`.
    pub types: Vec<String>,
    /// Asserted IRI objects of `rdfs:subClassOf`, without the class itself.
    pub super_classes: Vec<String>,
    pub equivalent_classes: Vec<String>,
    pub disjoint_with: Vec<String>,
    /// Anonymous superclasses: class expressions in the OWL 2 Manchester Syntax, with
    /// IRIs in angle brackets.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub super_class_expressions: Vec<String>,
    /// Anonymous equivalent classes, rendered the same way.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub equivalent_class_expressions: Vec<String>,
    pub labels: Vec<Lit>,
    pub comments: Vec<Lit>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ClassEntry {
    pub iri: String,
    /// In the rdf:, rdfs:, owl:, xsd: or sh: namespace.
    pub builtin: bool,
    pub observed: ClassObserved,
    pub declared: ClassDeclared,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct KindCount {
    pub triples: u64,
    pub distinct: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct LangCount {
    pub lang: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    pub triples: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct LiteralGroup {
    /// Datatype IRI; `xsd:string` for simple literals, `rdf:langString` /
    /// `rdf:dirLangString` for language-tagged ones.
    pub datatype: String,
    pub triples: u64,
    pub distinct: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub languages: Option<Vec<LangCount>>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectKinds {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iri: Option<KindCount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blank: Option<KindCount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triple_term: Option<KindCount>,
    /// Sorted by datatype IRI.
    pub literals: Vec<LiteralGroup>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PredicateObserved {
    pub triples: u64,
    pub distinct_subjects: u64,
    pub distinct_objects: u64,
    /// The largest number of distinct objects one subject has in this snapshot: a
    /// measurement, not a constraint.
    pub max_per_subject: u64,
    /// Subjects with two or more distinct objects.
    pub subjects_with_multiple: u64,
    pub objects: ObjectKinds,
    /// The IRI classes of the predicate's subjects, sorted by class IRI. A subject with
    /// several classes counts under each of them. Only computed when
    /// [`SchemaOptions::subject_classes`] asks for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_classes: Option<Vec<SubjectClass>>,
    /// The triples and subjects whose subject has no `rdf:type` with an IRI object in the
    /// selection (with [`SchemaOptions::subject_classes`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub untyped_subjects: Option<SubjectCount>,
}

/// Triples of a predicate whose subjects have one class, and how many such subjects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SubjectClass {
    pub class: String,
    pub triples: u64,
    pub subjects: u64,
}

/// Triples and distinct subjects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SubjectCount {
    pub triples: u64,
    pub subjects: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PredicateDeclared {
    /// `rdf:Property` and the `owl:*Property` types the predicate is declared with.
    pub types: Vec<String>,
    pub domains: Vec<String>,
    pub ranges: Vec<String>,
    pub super_properties: Vec<String>,
    pub inverse_of: Vec<String>,
    /// Anonymous domains and ranges: class expressions in the OWL 2 Manchester Syntax.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub domain_expressions: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub range_expressions: Vec<String>,
    pub labels: Vec<Lit>,
    pub comments: Vec<Lit>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PredicateEntry {
    pub iri: String,
    pub builtin: bool,
    pub observed: PredicateObserved,
    pub declared: PredicateDeclared,
}

/// The complete schema report of one snapshot. Classes and predicates are sorted by IRI
/// (byte order).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaReport {
    pub schema_format: u32,
    pub snapshot: SnapshotInfo,
    pub selection: Selection,
    pub totals: Totals,
    pub ontology: Vec<OntologyEntry>,
    pub hierarchy: Hierarchy,
    pub classes: Vec<ClassEntry>,
    pub predicates: Vec<PredicateEntry>,
    /// Counted only when [`SchemaOptions::term_totals`] asks for it. It is left out of
    /// the JSON document, so that the document does not depend on which request
    /// computed a cached report.
    #[serde(skip)]
    pub term_totals: Option<TermTotals>,
    /// What [`update`] needs to bring the report up to date from later changes (not
    /// kept for reports with subject classes, which it cannot maintain).
    #[serde(skip)]
    pub maintenance: Option<Arc<Maintenance>>,
}

/// One page of a list, in IRI order.
#[derive(Clone, Debug, Serialize)]
pub struct Page<'a, T> {
    pub items: &'a [T],
    /// Size of the whole list.
    pub total: usize,
    /// Continuation token, `None` on the last page.
    pub next: Option<String>,
}

/// The summary document: the report with the first page of each list.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaSummary<'a> {
    pub schema_format: u32,
    pub dataset: &'a str,
    pub snapshot: &'a SnapshotInfo,
    pub selection: &'a Selection,
    pub totals: &'a Totals,
    pub ontology: &'a [OntologyEntry],
    pub hierarchy: &'a Hierarchy,
    pub classes: Page<'a, ClassEntry>,
    pub predicates: Page<'a, PredicateEntry>,
    /// The SHACL constraints layer, when the caller added one
    /// ([`SchemaSummary::with_constraints`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constraints: Option<&'a ConstraintsLayer>,
}

impl<'a> SchemaSummary<'a> {
    /// The summary with a constraints layer.
    pub fn with_constraints(mut self, constraints: Option<&'a ConstraintsLayer>) -> Self {
        self.constraints = constraints;
        self
    }
}

/// Entries addressable by IRI (for pagination).
pub trait HasIri {
    fn iri(&self) -> &str;
}

impl HasIri for ClassEntry {
    fn iri(&self) -> &str {
        &self.iri
    }
}

impl HasIri for PredicateEntry {
    fn iri(&self) -> &str {
        &self.iri
    }
}

/// At most `limit` items of an IRI-sorted list that come strictly after `after`, and
/// whether more follow.
pub fn page_after<'a, T: HasIri>(
    items: &'a [T],
    after: Option<&str>,
    limit: usize,
) -> (&'a [T], bool) {
    let start = after.map_or(0, |a| items.partition_point(|x| x.iri() <= a));
    let end = start.saturating_add(limit).min(items.len());
    (&items[start..end], end < items.len())
}

impl SchemaReport {
    /// The summary document with the given pages (use [`Page`] with every item and no
    /// `next` for a complete, unpaginated document).
    pub fn summary<'a>(
        &'a self,
        dataset: &'a str,
        classes: Page<'a, ClassEntry>,
        predicates: Page<'a, PredicateEntry>,
    ) -> SchemaSummary<'a> {
        SchemaSummary {
            schema_format: self.schema_format,
            dataset,
            snapshot: &self.snapshot,
            selection: &self.selection,
            totals: &self.totals,
            ontology: &self.ontology,
            hierarchy: &self.hierarchy,
            classes,
            predicates,
            constraints: None,
        }
    }
}

// ------------------------------------------------------------------ discovery ------

/// Graph filter on the graph column of PSO / POS / SPO keys.
enum GraphFilter {
    All,
    AllExcept(u64),
    Set(Vec<u64>),
}

impl GraphFilter {
    #[inline]
    fn accepts(&self, g: u64) -> bool {
        match self {
            GraphFilter::All => true,
            GraphFilter::AllExcept(x) => g != *x,
            GraphFilter::Set(s) => s.contains(&g),
        }
    }
}

fn resolve(
    snap: &Snapshot,
    sel: &GraphSelection,
    inferred: Option<u64>,
    with_inferred: bool,
) -> Result<GraphFilter, SchemaError> {
    let every = || match inferred {
        Some(i) if !with_inferred => GraphFilter::AllExcept(i),
        _ => GraphFilter::All,
    };
    Ok(match sel {
        GraphSelection::Union => every(),
        GraphSelection::Default if snap.union_default_graph => every(),
        GraphSelection::Default => {
            let mut s = vec![Id::DEFAULT_GRAPH.0];
            if let Some(i) = inferred.filter(|_| with_inferred) {
                s.push(i);
            }
            GraphFilter::Set(s)
        }
        GraphSelection::Named(n) => {
            let g = snap
                .lookup_iri(n.as_str())
                .filter(|g| snap.count(Perm::Gspo, &[g.0]).unwrap_or(0) > 0)
                .ok_or_else(|| SchemaError::NoSuchGraph(n.as_str().to_string()))?;
            GraphFilter::Set(vec![g.0])
        }
    })
}

/// Limit a resolved selection to the graphs a view reads; a hidden graph named by the
/// selection is reported as missing.
fn within_view(
    snap: &Snapshot,
    f: GraphFilter,
    sel: &GraphSelection,
    access: &crate::access::GraphAccess,
) -> Result<GraphFilter, SchemaError> {
    let mut visible: Vec<u64> = access
        .visible_named(snap)
        .map_err(SchemaError::from)?
        .iter()
        .map(|g| g.0)
        .collect();
    if access.read.default_graph() {
        visible.push(Id::DEFAULT_GRAPH.0);
    }
    if let GraphSelection::Named(n) = sel
        && !access.read.allows_iri(n.as_str())
    {
        return Err(SchemaError::NoSuchGraph(n.as_str().to_string()));
    }
    Ok(GraphFilter::Set(match f {
        GraphFilter::All => visible,
        GraphFilter::AllExcept(x) => visible.into_iter().filter(|g| *g != x).collect(),
        GraphFilter::Set(s) => s
            .into_iter()
            .filter(|g| {
                visible.contains(g) || *g == Id::DEFAULT_GRAPH.0 && access.read.default_graph()
            })
            .collect(),
    }))
}

/// Deadline and cancellation checks.
struct Budget<'a> {
    deadline: Option<Instant>,
    cancel: Option<&'a AtomicBool>,
}

impl Budget<'_> {
    fn check(&self) -> crate::Result<()> {
        if self.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Error::Cancelled);
        }
        if self.deadline.is_some_and(|d| Instant::now() > d) {
            return Err(Error::Timeout);
        }
        Ok(())
    }
}

/// Attach the running phase to a budget failure.
fn in_phase<T>(r: crate::Result<T>, phase: impl FnOnce() -> String) -> Result<T, SchemaError> {
    r.map_err(|e| match e {
        Error::Timeout => SchemaError::Timeout { phase: phase() },
        Error::Cancelled => SchemaError::Cancelled,
        e => SchemaError::Store(e),
    })
}

/// Visit every key with a prefix, checking the budget after each base block and every
/// 1024 delta rows.
fn for_each_key(
    snap: &Snapshot,
    perm: Perm,
    prefix: &[u64],
    budget: &Budget,
    mut f: impl FnMut(&Key),
) -> crate::Result<()> {
    budget.check()?;
    let mut rows = 0u32;
    snap.scan(perm, prefix, |c| {
        match c {
            Chunk::Block(b, s, e) => {
                budget.check()?;
                for i in s..e {
                    f(&b.key(i));
                }
            }
            Chunk::Row(k) => {
                rows = rows.wrapping_add(1);
                if rows.is_multiple_of(1024) {
                    budget.check()?;
                }
                f(&k);
            }
        }
        Ok(true)
    })
}

/// Kind of a stored term, from its id (and vocabulary key when needed).
enum Kind<'k> {
    Iri,
    Blank,
    Triple,
    /// A literal, with its key suffix: empty (`xsd:string`), `@lang[--dir]` or
    /// `^datatype`.
    Literal(std::borrow::Cow<'k, [u8]>),
    Other,
}

const INT_SUFFIX: &[u8] = b"^http://www.w3.org/2001/XMLSchema#integer";
const DECIMAL_SUFFIX: &[u8] = b"^http://www.w3.org/2001/XMLSchema#decimal";
const DOUBLE_SUFFIX: &[u8] = b"^http://www.w3.org/2001/XMLSchema#double";
const BOOL_SUFFIX: &[u8] = b"^http://www.w3.org/2001/XMLSchema#boolean";
const DATETIME_SUFFIX: &[u8] = b"^http://www.w3.org/2001/XMLSchema#dateTime";
const DATE_SUFFIX: &[u8] = b"^http://www.w3.org/2001/XMLSchema#date";

fn literal_suffix(key: &[u8]) -> &[u8] {
    let sep = key.iter().rposition(|&b| b == KEY_SEP).unwrap_or(key.len());
    key.get(sep + 1..).unwrap_or(&[])
}

fn kind_of_key(key: &[u8]) -> Kind<'static> {
    match key.first() {
        Some(b'<') => Kind::Iri,
        Some(b'(') => Kind::Triple,
        Some(b'_') => Kind::Blank,
        Some(b'"') => Kind::Literal(literal_suffix(key).to_vec().into()),
        _ => Kind::Other,
    }
}

/// Kind of an id without a vocabulary lookup, or `None` for a base-vocabulary literal
/// (whose datatype needs its key).
fn quick_kind(snap: &Snapshot, id: u64) -> Option<Kind<'static>> {
    let id = Id(id);
    let suffix = |s: &'static [u8]| Some(Kind::Literal(s.into()));
    match id.tag() {
        Tag::Int => suffix(INT_SUFFIX),
        Tag::Decimal => suffix(DECIMAL_SUFFIX),
        Tag::Double => suffix(DOUBLE_SUFFIX),
        Tag::Bool => suffix(BOOL_SUFFIX),
        Tag::DateTime => suffix(DATETIME_SUFFIX),
        Tag::Date => suffix(DATE_SUFFIX),
        Tag::BNode => Some(Kind::Blank),
        Tag::Vocab => {
            let v = &snap.generation.vocab;
            if v.is_iri(id.payload()) {
                Some(Kind::Iri)
            } else if v.is_triple(id.payload()) {
                Some(Kind::Triple)
            } else {
                None
            }
        }
        Tag::Delta => Some(snap.key(id).map_or(Kind::Other, |k| kind_of_key(&k))),
        _ => Some(Kind::Other),
    }
}

fn is_iri(snap: &Snapshot, id: u64) -> bool {
    matches!(quick_kind(snap, id), Some(Kind::Iri))
}

fn is_blank(snap: &Snapshot, id: u64) -> bool {
    matches!(quick_kind(snap, id), Some(Kind::Blank))
}

/// Per-predicate accumulator of the two passes. A report keeps them by predicate (see
/// [`Maintenance`]), so it can be brought up to date from later changes ([`update`]).
#[derive(Clone, Debug, Default)]
pub(crate) struct PredAcc {
    triples: u64,
    distinct_subjects: u64,
    distinct_objects: u64,
    /// distinct objects of one subject → subjects with that many
    per_subject: BTreeMap<u64, u64>,
    iri: KindCount,
    blank: KindCount,
    triple: KindCount,
    /// literal key suffix → (triples, distinct)
    lits: FxHashMap<Vec<u8>, (u64, u64)>,
}

/// `x + d`, or `None` below zero.
fn bumped(x: u64, d: i64) -> Option<u64> {
    x.checked_add_signed(d)
}

impl PredAcc {
    /// Add `triples` triples and `distinct` distinct objects of one kind (both may be
    /// negative when changes are applied); `None` when a count would fall below zero.
    fn object(&mut self, kind: Kind, triples: i64, distinct: i64) -> Option<()> {
        let add = |k: &mut KindCount| -> Option<()> {
            k.triples = bumped(k.triples, triples)?;
            k.distinct = bumped(k.distinct, distinct)?;
            Some(())
        };
        match kind {
            Kind::Iri => add(&mut self.iri),
            Kind::Blank => add(&mut self.blank),
            Kind::Triple => add(&mut self.triple),
            Kind::Literal(s) => {
                let e = self.lits.entry(s.into_owned()).or_default();
                e.0 = bumped(e.0, triples)?;
                e.1 = bumped(e.1, distinct)?;
                if *e == (0, 0) {
                    self.lits.retain(|_, v| *v != (0, 0));
                }
                Some(())
            }
            Kind::Other => Some(()),
        }
    }

    /// One subject more with `n` distinct objects (`n > 0`), or with `remove`, one less.
    fn subject_run(&mut self, n: u64, remove: bool) -> Option<()> {
        if n == 0 {
            return Some(());
        }
        if remove {
            let e = self.per_subject.get_mut(&n)?;
            *e = e.checked_sub(1)?;
            if *e == 0 {
                self.per_subject.remove(&n);
            }
        } else {
            *self.per_subject.entry(n).or_default() += 1;
        }
        Some(())
    }

    fn is_empty(&self) -> bool {
        self.triples == 0
            && self.distinct_subjects == 0
            && self.distinct_objects == 0
            && self.per_subject.is_empty()
            && self.iri == KindCount::default()
            && self.blank == KindCount::default()
            && self.triple == KindCount::default()
            && self.lits.is_empty()
    }

    fn observed(&self) -> PredicateObserved {
        let some = |k: KindCount| (k.distinct > 0).then_some(k);
        #[derive(Default)]
        struct Group {
            triples: u64,
            distinct: u64,
            langs: Option<BTreeMap<(String, Option<String>), u64>>,
        }
        let mut groups: BTreeMap<String, Group> = BTreeMap::new();
        for (suffix, &(t, d)) in &self.lits {
            let text = String::from_utf8_lossy(suffix.get(1..).unwrap_or(&[])).into_owned();
            let (datatype, lang) = match suffix.first() {
                None => (XSD_STRING.to_string(), None),
                Some(b'@') => match text.rsplit_once("--") {
                    Some((l, d @ ("ltr" | "rtl"))) => (
                        RDF_DIR_LANG_STRING.to_string(),
                        Some((l.to_string(), Some(d.to_string()))),
                    ),
                    _ => (RDF_LANG_STRING.to_string(), Some((text, None))),
                },
                Some(_) => (text, None),
            };
            let g = groups.entry(datatype).or_default();
            g.triples += t;
            g.distinct += d;
            if let Some(l) = lang {
                *g.langs
                    .get_or_insert_with(Default::default)
                    .entry(l)
                    .or_default() += t;
            }
        }
        PredicateObserved {
            triples: self.triples,
            distinct_subjects: self.distinct_subjects,
            distinct_objects: self.distinct_objects,
            max_per_subject: self.per_subject.keys().next_back().copied().unwrap_or(0),
            subjects_with_multiple: self.per_subject.range(2..).map(|(_, n)| n).sum(),
            objects: ObjectKinds {
                iri: some(self.iri),
                blank: some(self.blank),
                triple_term: some(self.triple),
                literals: groups
                    .into_iter()
                    .map(|(datatype, g)| LiteralGroup {
                        datatype,
                        triples: g.triples,
                        distinct: g.distinct,
                        languages: g.langs.map(|m| {
                            m.into_iter()
                                .map(|((lang, direction), triples)| LangCount {
                                    lang,
                                    direction,
                                    triples,
                                })
                                .collect()
                        }),
                    })
                    .collect(),
            },
            subject_classes: None,
            untyped_subjects: None,
        }
    }
}

/// What a report keeps to be brought up to date from the changes after its commit
/// ([`update`]): the accumulators of its predicates, by the predicate's vocabulary key,
/// which every generation shares.
#[derive(Clone, Debug, Default)]
pub struct Maintenance {
    /// the commit the report was computed at
    pub(crate) commit: u64,
    pub(crate) preds: FxHashMap<Vec<u8>, PredAcc>,
}

/// Pass A over `PSO[p]`: triples, distinct subjects, objects per subject, and with
/// `join`, the classes of the subjects.
fn subject_pass(
    src: &Src,
    p: u64,
    filter: &GraphFilter,
    budget: &Budget,
    acc: &mut PredAcc,
    mut join: Option<&mut ClassJoin>,
) -> crate::Result<()> {
    let mut prev: Option<(u64, u64)> = None;
    let mut run = 0u64;
    let mut end_subject = |acc: &mut PredAcc, s: Option<u64>, run: u64| {
        acc.subject_run(run, false);
        if let (Some(j), Some(s)) = (join.as_deref_mut(), s) {
            j.subject(s, run);
        }
    };
    src.for_each_key(Perm::Pso, &[p], budget, |k| {
        if !filter.accepts(k[3]) || prev == Some((k[1], k[2])) {
            return;
        }
        acc.triples += 1;
        if prev.is_none_or(|(s, _)| s != k[1]) {
            end_subject(acc, prev.map(|(s, _)| s), run);
            acc.distinct_subjects += 1;
            run = 0;
        }
        run += 1;
        prev = Some((k[1], k[2]));
    })?;
    end_subject(acc, prev.map(|(s, _)| s), run);
    Ok(())
}

/// The `(subject, class)` pairs of the selection's `rdf:type` triples whose object is
/// an IRI, sorted by subject and then by class.
fn subject_types(
    src: &Src,
    rdf_type: u64,
    filter: &GraphFilter,
    budget: &Budget,
) -> crate::Result<Vec<(u64, u64)>> {
    let mut iri: FxHashMap<u64, bool> = FxHashMap::default();
    let mut out: Vec<(u64, u64)> = Vec::new();
    let mut prev: Option<(u64, u64)> = None;
    src.for_each_key(Perm::Pso, &[rdf_type], budget, |k| {
        if !filter.accepts(k[3]) || prev == Some((k[1], k[2])) {
            return;
        }
        prev = Some((k[1], k[2]));
        if *iri.entry(k[2]).or_insert_with(|| is_iri(src, k[2])) {
            out.push((k[1], k[2]));
        }
    })?;
    Ok(out)
}

/// The merge join of one predicate's subjects, in `PSO` order, with
/// [`subject_types`]: the triples and subjects of each class.
struct ClassJoin<'a> {
    types: &'a [(u64, u64)],
    pos: usize,
    by_class: FxHashMap<u64, SubjectCount>,
    untyped: SubjectCount,
}

impl<'a> ClassJoin<'a> {
    fn new(types: &'a [(u64, u64)]) -> Self {
        ClassJoin {
            types,
            pos: 0,
            by_class: FxHashMap::default(),
            untyped: SubjectCount::default(),
        }
    }

    /// Subject `s` has `run` triples. Subjects arrive in ascending order.
    fn subject(&mut self, s: u64, run: u64) {
        self.pos += self.types[self.pos..].partition_point(|t| t.0 < s);
        let mut typed = false;
        while let Some(&(ts, c)) = self.types.get(self.pos) {
            if ts != s {
                break;
            }
            typed = true;
            let e = self.by_class.entry(c).or_default();
            e.triples += run;
            e.subjects += 1;
            self.pos += 1;
        }
        if !typed {
            self.untyped.triples += run;
            self.untyped.subjects += 1;
        }
    }
}

/// Base-vocabulary literal objects waiting for a batched key lookup.
#[derive(Default)]
struct PendingLiterals {
    ids: Vec<u64>,
    counts: Vec<u64>,
}

impl PendingLiterals {
    const BATCH: usize = 4096;

    fn flush(&mut self, snap: &Snapshot, acc: &mut PredAcc) {
        if self.ids.is_empty() {
            return;
        }
        let mut i = 0;
        let counts = &self.counts;
        let ids = &self.ids;
        snap.generation.vocab.get_sorted(ids, |id, key| {
            while i < ids.len() && ids[i] < id {
                i += 1;
            }
            if i < ids.len() && ids[i] == id {
                acc.object(
                    Kind::Literal(literal_suffix(key).into()),
                    counts[i] as i64,
                    1,
                );
                i += 1;
            }
        });
        self.ids.clear();
        self.counts.clear();
    }
}

/// Pass B over `POS[p]`: distinct objects and their kinds. For `rdf:type`, also the
/// instances of each class.
fn object_pass(
    src: &Src,
    p: u64,
    filter: &GraphFilter,
    budget: &Budget,
    acc: &mut PredAcc,
    mut class_instances: Option<&mut FxHashMap<u64, u64>>,
) -> crate::Result<()> {
    let mut pending = PendingLiterals::default();
    let mut prev: Option<(u64, u64)> = None;
    let mut run: Option<(u64, u64)> = None; // (object, distinct subjects)
    let mut end_object = |acc: &mut PredAcc, pending: &mut PendingLiterals, o: u64, n: u64| {
        acc.distinct_objects += 1;
        match quick_kind(src, o) {
            Some(kind) => {
                if let (Some(ci), Kind::Iri) = (class_instances.as_deref_mut(), &kind) {
                    ci.insert(o, n);
                }
                acc.object(kind, n as i64, 1);
            }
            None => {
                pending.ids.push(Id(o).payload());
                pending.counts.push(n);
                if pending.ids.len() >= PendingLiterals::BATCH {
                    pending.flush(src, acc);
                }
            }
        }
    };
    src.for_each_key(Perm::Pos, &[p], budget, |k| {
        if !filter.accepts(k[3]) || prev == Some((k[1], k[2])) {
            return;
        }
        prev = Some((k[1], k[2]));
        match &mut run {
            Some((o, n)) if *o == k[1] => *n += 1,
            _ => {
                if let Some((o, n)) = run {
                    end_object(acc, &mut pending, o, n);
                }
                run = Some((k[1], 1));
            }
        }
    })?;
    if let Some((o, n)) = run {
        end_object(acc, &mut pending, o, n);
    }
    pending.flush(src, acc);
    Ok(())
}

/// Declarations of one class, keyed by ids until the end.
#[derive(Default)]
struct ClassDecl {
    instances: u64,
    types: BTreeSet<&'static str>,
    supers: BTreeSet<u64>,
    equivalents: BTreeSet<u64>,
    disjoint: BTreeSet<u64>,
    super_exprs: BTreeSet<u64>,
    equivalent_exprs: BTreeSet<u64>,
}

#[derive(Default)]
struct PropDecl {
    observed: Option<PredicateObserved>,
    types: BTreeSet<&'static str>,
    domains: BTreeSet<u64>,
    ranges: BTreeSet<u64>,
    supers: BTreeSet<u64>,
    inverse: BTreeSet<u64>,
    domain_exprs: BTreeSet<u64>,
    range_exprs: BTreeSet<u64>,
}

/// Distinct literal objects of `(s, p)` in the filtered graphs, sorted.
fn literals_of(
    src: &Src,
    s: u64,
    p: Option<Id>,
    filter: &GraphFilter,
    budget: &Budget,
) -> crate::Result<Vec<Lit>> {
    let Some(p) = p else {
        return Ok(Vec::new());
    };
    let mut ids = Vec::new();
    src.for_each_key(Perm::Spo, &[s, p.0], budget, |k| {
        if filter.accepts(k[3]) {
            ids.push(k[2]);
        }
    })?;
    ids.dedup();
    let mut out: Vec<Lit> = ids
        .into_iter()
        .filter_map(|o| match src.term(Id(o))? {
            Term::Literal(l) => Some(Lit {
                value: l.value().to_string(),
                lang: l.language().map(str::to_string),
            }),
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| a.lang.cmp(&b.lang).then_with(|| a.value.cmp(&b.value)));
    Ok(out)
}

fn builtin(iri: &str) -> bool {
    BUILTIN_NAMESPACES.iter().any(|ns| iri.starts_with(ns))
}

/// IRI strings of ids, cached.
struct Iris<'s> {
    snap: &'s Snapshot,
    cache: FxHashMap<u64, Option<String>>,
}

impl Iris<'_> {
    fn get(&mut self, id: u64) -> Option<String> {
        let snap = self.snap;
        self.cache
            .entry(id)
            .or_insert_with(|| match snap.term(Id(id)) {
                Some(Term::NamedNode(n)) => Some(n.into_string()),
                _ => None,
            })
            .clone()
    }

    fn all(&mut self, ids: &BTreeSet<u64>) -> Vec<String> {
        let mut v: Vec<String> = ids.iter().filter_map(|&i| self.get(i)).collect();
        v.sort();
        v.dedup();
        v
    }
}

/// The resolved graph filters of a report.
struct Selected<'o> {
    observed: GraphFilter,
    declared: GraphFilter,
    declared_sel: &'o GraphSelection,
}

impl<'o> Selected<'o> {
    fn resolve(snap: &Snapshot, opts: &'o SchemaOptions) -> Result<Selected<'o>, SchemaError> {
        let inferred = opts
            .inferred_graph
            .as_deref()
            .and_then(|g| snap.lookup_iri(g))
            .map(|g| g.0);
        let observed = resolve(snap, &opts.graph, inferred, opts.include_inferred)?;
        let declared_sel = opts.declared_graph.as_ref().unwrap_or(&opts.graph);
        let declared = resolve(snap, declared_sel, inferred, opts.declared_from_inferred)?;
        let (observed, declared) = match opts.graphs.as_ref().filter(|a| !a.reads_all()) {
            Some(a) => (
                within_view(snap, observed, &opts.graph, a)?,
                within_view(snap, declared, declared_sel, a)?,
            ),
            None => (observed, declared),
        };
        Ok(Selected {
            observed,
            declared,
            declared_sel,
        })
    }
}

/// The subject classes of each predicate, and its untyped subjects, by id.
type Joins = FxHashMap<u64, (FxHashMap<u64, SubjectCount>, SubjectCount)>;

/// The observed layer of a report, by id: the accumulator of every used predicate and
/// the instances of every IRI class.
struct Observed {
    preds: FxHashMap<u64, PredAcc>,
    class_instances: FxHashMap<u64, u64>,
    /// the subject classes of each predicate by class id, named at the end
    joins: Option<Joins>,
    term_totals: Option<TermTotals>,
}

/// Labels a report may take from an earlier report of the same selection: those of
/// entries whose subjects' labels, comments and version info did not change.
struct Reuse<'a> {
    old: &'a SchemaReport,
    /// ids of the subjects whose labels, comments or version info changed
    changed: FxHashSet<u64>,
}

/// Compute the schema report of `snap`.
pub fn discover(snap: &Arc<Snapshot>, opts: &SchemaOptions) -> Result<SchemaReport, SchemaError> {
    let snap: &Snapshot = snap;
    let budget = Budget {
        deadline: opts.deadline,
        cancel: opts.cancel.as_deref(),
    };
    in_phase(budget.check(), || "starting".into())?;
    let sel = Selected::resolve(snap, opts)?;
    let src = in_phase(
        Src::for_filters(snap, &[&sel.observed, &sel.declared], &budget),
        || "reading the selected graphs".into(),
    )?;
    let observed = &sel.observed;
    let rdf_type = snap.lookup_iri(RDF_TYPE).map(|i| i.0);

    // -- observed: two passes per predicate -------------------------------------------
    let preds = in_phase(src.distinct_first(Perm::Pso), || {
        "listing predicates".into()
    })?;
    let mut accs: FxHashMap<u64, PredAcc> = FxHashMap::default();
    let mut class_instances: FxHashMap<u64, u64> = FxHashMap::default();
    let types = match rdf_type.filter(|_| opts.subject_classes) {
        Some(t) => in_phase(subject_types(&src, t, observed, &budget), || {
            "reading the classes of subjects".into()
        })?,
        None => Vec::new(),
    };
    let mut joins: Joins = FxHashMap::default();
    for (i, &p) in preds.iter().enumerate() {
        let phase = || format!("scanning predicates ({}/{})", i + 1, preds.len());
        let mut acc = PredAcc::default();
        let mut join = opts.subject_classes.then(|| ClassJoin::new(&types));
        in_phase(
            subject_pass(&src, p, observed, &budget, &mut acc, join.as_mut()),
            phase,
        )?;
        if acc.triples == 0 {
            continue;
        }
        if let Some(j) = join {
            joins.insert(p, (j.by_class, j.untyped));
        }
        let ci = (Some(p) == rdf_type).then_some(&mut class_instances);
        in_phase(object_pass(&src, p, observed, &budget, &mut acc, ci), phase)?;
        accs.insert(p, acc);
    }
    let term_totals = if opts.term_totals {
        Some(in_phase(term_totals(&src, observed, &budget), || {
            "counting distinct subjects and objects".into()
        })?)
    } else {
        None
    };
    let obs = Observed {
        preds: accs,
        class_instances,
        joins: opts.subject_classes.then_some(joins),
        term_totals,
    };
    assemble(&src, opts, &sel, &budget, obs, None)
}

/// The declared layer and the report: declarations read from the declared graphs,
/// labels (read, or taken from `reuse`), the hierarchy, and the lists in IRI order.
fn assemble(
    src: &Src,
    opts: &SchemaOptions,
    sel: &Selected,
    budget: &Budget,
    obs: Observed,
    reuse: Option<&Reuse>,
) -> Result<SchemaReport, SchemaError> {
    let snap: &Snapshot = src;
    let declared = &sel.declared;
    let rdf_type = snap.lookup_iri(RDF_TYPE).map(|i| i.0);
    let total_triples: u64 = obs.preds.values().map(|a| a.triples).sum();
    let anon_types = rdf_type
        .and_then(|t| obs.preds.get(&t))
        .map_or(0, |a| a.blank.distinct);
    let maintenance = (!opts.subject_classes).then(|| {
        Arc::new(Maintenance {
            commit: snap.commit,
            preds: obs
                .preds
                .iter()
                .filter_map(|(p, a)| Some((snap.key(Id(*p))?.into_owned(), a.clone())))
                .collect(),
        })
    });
    let mut props: FxHashMap<u64, PropDecl> = FxHashMap::default();
    for (p, acc) in &obs.preds {
        if is_iri(snap, *p) {
            props.entry(*p).or_default().observed = Some(acc.observed());
        }
    }
    let mut joins = obs.joins.unwrap_or_default();
    let mut classes: FxHashMap<u64, ClassDecl> = obs
        .class_instances
        .into_iter()
        .map(|(c, n)| {
            (
                c,
                ClassDecl {
                    instances: n,
                    ..Default::default()
                },
            )
        })
        .collect();

    // -- declarations ---------------------------------------------------------------
    let decl_phase = || "reading declarations".to_string();
    let mut anon_exprs: FxHashSet<u64> = FxHashSet::default();
    let triples_of = |p: &str| -> Result<Vec<(u64, u64)>, SchemaError> {
        let Some(p) = snap.lookup_iri(p) else {
            return Ok(Vec::new());
        };
        let mut out: Vec<(u64, u64)> = Vec::new();
        in_phase(
            src.for_each_key(Perm::Pso, &[p.0], budget, |k| {
                if declared.accepts(k[3]) && out.last() != Some(&(k[1], k[2])) {
                    out.push((k[1], k[2]));
                }
            }),
            decl_phase,
        )?;
        Ok(out)
    };
    for (iri, rel) in CLASS_RELS {
        for (s, o) in triples_of(iri)? {
            if !is_iri(snap, s) {
                continue;
            }
            if is_blank(snap, o) {
                anon_exprs.insert(o);
            }
            let o_iri = is_iri(snap, o);
            if o_iri {
                classes.entry(o).or_default();
            }
            let c = classes.entry(s).or_default();
            if !o_iri {
                if is_blank(snap, o) {
                    match rel {
                        ClassRel::SubClassOf => c.super_exprs.insert(o),
                        ClassRel::EquivalentClass => c.equivalent_exprs.insert(o),
                        ClassRel::DisjointWith => false,
                    };
                }
                continue;
            }
            match rel {
                ClassRel::SubClassOf if o != s => {
                    c.supers.insert(o);
                }
                ClassRel::SubClassOf => {}
                ClassRel::EquivalentClass => {
                    c.equivalents.insert(o);
                }
                ClassRel::DisjointWith => {
                    c.disjoint.insert(o);
                }
            }
        }
    }
    for (iri, rel) in PROP_RELS {
        for (s, o) in triples_of(iri)? {
            if !is_iri(snap, s) {
                continue;
            }
            if matches!(rel, PropRel::Domain | PropRel::Range) && is_blank(snap, o) {
                anon_exprs.insert(o);
            }
            if !is_iri(snap, o) {
                let e = props.entry(s).or_default();
                if is_blank(snap, o) {
                    match rel {
                        PropRel::Domain => e.domain_exprs.insert(o),
                        PropRel::Range => e.range_exprs.insert(o),
                        _ => false,
                    };
                }
                continue;
            }
            if matches!(rel, PropRel::SubPropertyOf | PropRel::InverseOf) {
                props.entry(o).or_default();
            }
            let e = props.entry(s).or_default();
            match rel {
                PropRel::Domain => e.domains.insert(o),
                PropRel::Range => e.ranges.insert(o),
                PropRel::SubPropertyOf => e.supers.insert(o),
                PropRel::InverseOf => e.inverse.insert(o),
            };
        }
    }
    let typed = |t: &str| -> Result<Vec<u64>, SchemaError> {
        let (Some(ty), Some(t)) = (rdf_type, snap.lookup_iri(t)) else {
            return Ok(Vec::new());
        };
        let mut out: Vec<u64> = Vec::new();
        in_phase(
            src.for_each_key(Perm::Pos, &[ty, t.0], budget, |k| {
                if declared.accepts(k[3]) && out.last() != Some(&k[2]) && is_iri(snap, k[2]) {
                    out.push(k[2]);
                }
            }),
            decl_phase,
        )?;
        Ok(out)
    };
    for t in CLASS_TYPES {
        for s in typed(t)? {
            classes.entry(s).or_default().types.insert(t);
        }
    }
    for t in PROPERTY_TYPES {
        for s in typed(t)? {
            props.entry(s).or_default().types.insert(t);
        }
    }
    let ontologies = typed(OWL_ONTOLOGY)?;

    for (kind, n) in [("classes", classes.len()), ("predicates", props.len())] {
        if n > opts.max_entries {
            return Err(SchemaError::TooManyEntries {
                kind,
                count: n,
                limit: opts.max_entries,
            });
        }
    }

    // -- labels and assembly ----------------------------------------------------------
    let label = snap.lookup_iri(RDFS_LABEL);
    let comment = snap.lookup_iri(RDFS_COMMENT);
    let version_info = snap.lookup_iri(OWL_VERSION_INFO);
    let n_entries = classes.len() + props.len();
    let mut done = 0usize;
    let lits = |s: u64, p: Option<Id>, done: usize| {
        in_phase(literals_of(src, s, p, declared, budget), || {
            format!("reading labels ({done}/{n_entries})")
        })
    };
    // an earlier report's entry for `iri`, when its labels can be taken over
    let kept = |id: u64| reuse.filter(|r| !r.changed.contains(&id)).map(|r| r.old);
    let mut iris = Iris {
        snap,
        cache: FxHashMap::default(),
    };
    let mut renderer = expressions::Renderer::new(src, declared, budget);
    let mut render = |ids: &BTreeSet<u64>| -> Result<Vec<String>, SchemaError> {
        let mut v = Vec::with_capacity(ids.len());
        for &id in ids {
            v.push(in_phase(renderer.render(id), || {
                "rendering class expressions".into()
            })?);
        }
        v.sort();
        v.dedup();
        Ok(v)
    };

    let mut class_list = Vec::with_capacity(classes.len());
    for (id, c) in classes {
        done += 1;
        let Some(iri) = iris.get(id) else { continue };
        let old = kept(id).and_then(|o| find(&o.classes, &iri));
        let (labels, comments) = match old {
            Some(o) => (o.declared.labels.clone(), o.declared.comments.clone()),
            None => (lits(id, label, done)?, lits(id, comment, done)?),
        };
        class_list.push(ClassEntry {
            builtin: builtin(&iri),
            iri,
            observed: ClassObserved {
                instances: c.instances,
            },
            declared: ClassDeclared {
                types: c.types.iter().map(|t| t.to_string()).collect(),
                super_classes: iris.all(&c.supers),
                equivalent_classes: iris.all(&c.equivalents),
                disjoint_with: iris.all(&c.disjoint),
                super_class_expressions: render(&c.super_exprs)?,
                equivalent_class_expressions: render(&c.equivalent_exprs)?,
                labels,
                comments,
            },
        });
    }
    class_list.sort_by(|a, b| a.iri.cmp(&b.iri));

    let mut pred_list = Vec::with_capacity(props.len());
    for (id, p) in props {
        done += 1;
        let Some(iri) = iris.get(id) else { continue };
        let mut observed = p.observed.unwrap_or_default();
        if opts.subject_classes {
            let (by_class, untyped) = joins.remove(&id).unwrap_or_default();
            let mut list: Vec<SubjectClass> = by_class
                .into_iter()
                .filter_map(|(c, n)| {
                    Some(SubjectClass {
                        class: iris.get(c)?,
                        triples: n.triples,
                        subjects: n.subjects,
                    })
                })
                .collect();
            list.sort_by(|a, b| a.class.cmp(&b.class));
            observed.subject_classes = Some(list);
            observed.untyped_subjects = Some(untyped);
        }
        let old = kept(id).and_then(|o| find(&o.predicates, &iri));
        let (labels, comments) = match old {
            Some(o) => (o.declared.labels.clone(), o.declared.comments.clone()),
            None => (lits(id, label, done)?, lits(id, comment, done)?),
        };
        pred_list.push(PredicateEntry {
            builtin: builtin(&iri),
            iri,
            observed,
            declared: PredicateDeclared {
                types: p.types.iter().map(|t| t.to_string()).collect(),
                domains: iris.all(&p.domains),
                ranges: iris.all(&p.ranges),
                super_properties: iris.all(&p.supers),
                inverse_of: iris.all(&p.inverse),
                domain_expressions: render(&p.domain_exprs)?,
                range_expressions: render(&p.range_exprs)?,
                labels,
                comments,
            },
        });
    }
    pred_list.sort_by(|a, b| a.iri.cmp(&b.iri));

    let mut ontology = Vec::new();
    for id in ontologies {
        let Some(iri) = iris.get(id) else { continue };
        let old = kept(id).and_then(|o| {
            o.ontology
                .binary_search_by(|e| e.iri.as_str().cmp(&iri))
                .ok()
                .map(|i| &o.ontology[i])
        });
        ontology.push(match old {
            Some(o) => OntologyEntry { iri, ..o.clone() },
            None => OntologyEntry {
                iri,
                labels: lits(id, label, done)?,
                version_info: lits(id, version_info, done)?,
                comments: lits(id, comment, done)?,
            },
        });
    }
    ontology.sort_by(|a, b| a.iri.cmp(&b.iri));

    let hierarchy = hierarchy(&class_list);
    Ok(SchemaReport {
        schema_format: SCHEMA_FORMAT,
        snapshot: SnapshotInfo {
            version: snapshot_identity(snap),
            commit: snap.commit,
            generation: snap.generation.name.clone(),
            computed_at: crate::builder::now_rfc3339(),
        },
        selection: Selection {
            graph: opts.graph.name().to_string(),
            declared_graph: sel.declared_sel.name().to_string(),
            reasoning: opts.include_inferred,
            declared: if opts.declared_from_inferred {
                "all"
            } else {
                "asserted"
            },
        },
        totals: Totals {
            triples: total_triples,
            classes: class_list.len(),
            predicates: pred_list.len(),
            anonymous_type_targets: anon_types,
            anonymous_class_expressions: anon_exprs.len() as u64,
        },
        ontology,
        hierarchy,
        classes: class_list,
        predicates: pred_list,
        term_totals: obs.term_totals,
        maintenance,
    })
}

/// The entry for `iri` in an IRI-sorted list.
fn find<'a, T: HasIri>(items: &'a [T], iri: &str) -> Option<&'a T> {
    items
        .binary_search_by(|e| e.iri().cmp(iri))
        .ok()
        .map(|i| &items[i])
}

/// Distinct subjects (one SPO pass, also telling IRIs apart) and distinct objects (one
/// OSP pass) of the selected graphs.
fn term_totals(src: &Src, filter: &GraphFilter, budget: &Budget) -> crate::Result<TermTotals> {
    let mut t = TermTotals::default();
    let mut prev: Option<u64> = None;
    src.for_each_key(Perm::Spo, &[], budget, |k| {
        if !filter.accepts(k[3]) || prev == Some(k[0]) {
            return;
        }
        prev = Some(k[0]);
        t.distinct_subjects += 1;
        if is_iri(src, k[0]) {
            t.entities += 1;
        }
    })?;
    prev = None;
    src.for_each_key(Perm::Osp, &[], budget, |k| {
        if filter.accepts(k[3]) && prev != Some(k[0]) {
            prev = Some(k[0]);
            t.distinct_objects += 1;
        }
    })?;
    Ok(t)
}

/// Roots and cycles of the declared `rdfs:subClassOf` graph (`classes` sorted by IRI).
pub fn hierarchy(classes: &[ClassEntry]) -> Hierarchy {
    let n = classes.len();
    let index: FxHashMap<&str, usize> = classes
        .iter()
        .enumerate()
        .map(|(i, c)| (c.iri.as_str(), i))
        .collect();
    let supers: Vec<Vec<usize>> = classes
        .iter()
        .enumerate()
        .map(|(i, c)| {
            c.declared
                .super_classes
                .iter()
                .filter_map(|s| index.get(s.as_str()).copied())
                .filter(|&j| j != i)
                .collect()
        })
        .collect();
    let comp = strongly_connected(&supers);
    let ncomp = comp.iter().copied().max().map_or(0, |m| m + 1);
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); ncomp];
    for (i, &c) in comp.iter().enumerate() {
        members[c].push(i); // ascending, so members[c][0] has the smallest IRI
    }
    let mut subclasses = vec![0usize; n];
    for ss in &supers {
        for &s in ss {
            subclasses[s] += 1;
        }
    }
    let mut roots: Vec<usize> = members
        .iter()
        .filter(|m| {
            !m.is_empty()
                && m.iter()
                    .all(|&i| supers[i].iter().all(|&s| comp[s] == comp[i]))
        })
        .map(|m| m[0])
        .collect();
    roots.sort_by(|&a, &b| {
        subclasses[b]
            .cmp(&subclasses[a])
            .then_with(|| classes[a].iri.cmp(&classes[b].iri))
    });
    let mut cycles: Vec<Vec<String>> = members
        .iter()
        .filter(|m| m.len() > 1)
        .map(|m| m.iter().map(|&i| classes[i].iri.clone()).collect())
        .collect();
    cycles.sort();
    Hierarchy {
        roots: roots.into_iter().map(|i| classes[i].iri.clone()).collect(),
        cycles,
    }
}

/// Tarjan's strongly connected components (iterative): the component number of each node.
fn strongly_connected(adj: &[Vec<usize>]) -> Vec<usize> {
    const UNSEEN: usize = usize::MAX;
    let n = adj.len();
    let mut index = vec![UNSEEN; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut comp = vec![UNSEEN; n];
    let (mut next, mut ncomp) = (0usize, 0usize);
    for root in 0..n {
        if index[root] != UNSEEN {
            continue;
        }
        let mut call: Vec<(usize, usize)> = vec![(root, 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&(v, e)) = call.last() {
            if e < adj[v].len() {
                call.last_mut().unwrap().1 += 1;
                let w = adj[v][e];
                if index[w] == UNSEEN {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    call.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                call.pop();
                if let Some(&(u, _)) = call.last() {
                    low[u] = low[u].min(low[v]);
                }
                if low[v] == index[v] {
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        comp[w] = ncomp;
                        if w == v {
                            break;
                        }
                    }
                    ncomp += 1;
                }
            }
        }
    }
    comp
}

mod source;
pub use source::SMALL_SELECTION_MAX;
use source::Src;

mod expressions;

mod maintain;
pub use maintain::{max_changes, update};

pub mod profile;
pub use profile::{ClassProfiles, ProfileOptions, profiles};

pub mod compare;
pub use compare::{SchemaDiff, compare, diff_text};

mod void;
pub use void::{VOID_NS, VoidOptions, description_iri, void_text, void_triples};

pub mod constraints;
pub use constraints::ConstraintsLayer;

pub mod draft;
pub use draft::{DraftOptions, ShapesDraft, draft_shapes};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod maintain_tests;

#[cfg(test)]
mod profile_tests;

#[cfg(test)]
mod compare_tests;
