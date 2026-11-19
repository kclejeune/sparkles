//! Ontology files read without a store (spec F24): the terminology and the assertions of
//! one file, as an open ontology of a database holds them after the file is loaded into
//! an empty database, kept in memory. A command checks or classifies an [`OntologyFile`]
//! with the answers of [`Ontologies::check`](super::Ontologies::check) and
//! [`Ontologies::classify`](super::Ontologies::classify), and a command that transforms an
//! ontology can hand the next one an `OntologyFile` without writing a file.
//!
//! [`OntologyFile::read`] reads a file of one graph in a W3C syntax (RDF/XML, Turtle,
//! N-Triples or N3) and a functional-style document in a lenient read. Each answers
//! `None` for what the store path reads instead: a syntax that holds named graphs, Jena's
//! syntaxes, a strict read of a functional-style document, a functional-style document
//! with an anonymous individual, a request for graphs other than the default graph, and a
//! file that fails to parse, whose error the loader then reports as it always has.
//!
//! - A file of triples is parsed by the loader's parser on several threads
//!   ([`crate::io::parse_triples`]), its duplicate triples are dropped as a store drops
//!   them, and its terminology is selected as an open ontology selects it: the
//!   structural predicates, facets and built-in annotation properties, `rdf:type` with a
//!   structural type, the predicates of the annotation properties the file declares,
//!   `x rdf:type _:b` for each blank node a selected triple is about, and the main
//!   triples of annotated axioms. The reverse mapping then reads it as it reads a store's.
//!   The other triples are the assertions, mapped one triple at a time as a store's are.
//! - A functional-style document is parsed on several threads
//!   ([`sparkles_owl::ofn_read::parse_parallel`]) and its terminology is kept as written.
//!   An assertion that a store would stream stays out of the terminology and becomes its
//!   triples: a class assertion of a named class, a property assertion, `SameIndividual`,
//!   a `DifferentIndividuals` of two individuals and an annotation assertion whose
//!   property is neither built in nor declared, each without annotations, since a store
//!   reads an annotated axiom's main triple with the terminology. The declared typing
//!   gains the declarations that Table 6 of the mapping implies
//!   ([`read::Ontology::from_axioms`]).
//!
//! The triples a read hands the reverse mapping and the assertions come in the order of a
//! store's scans, and a blank node gets the label a store that numbers the file's blank
//! nodes in their order gives it, so checks list their violations as a store read lists
//! them. A file has only the default graph, so every `owl:imports` is unresolved, with the
//! store's warning. One thing can differ from a read of the loaded file: an n-ary
//! `EquivalentClasses`, `EquivalentObjectProperties`, `EquivalentDataProperties` or
//! annotated `SameIndividual` of a functional-style document stays one
//! axiom where the mapping to triples makes one axiom of each pair. The taxonomy, the
//! consistency and the unsatisfiable classes are the same.

use super::classify::{ClassifyRequest, classify_once, logical_axioms};
use super::{CheckRequest, Inferences, OntologyRequest, ReadView, check};
use crate::Dataset;
use crate::error::{Error, Result};
use crate::io::{RdfSyntax, Source};
use crate::sparql::QueryOptions;
use oxrdf::{NamedOrBlankNode, Term, Triple};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_owl::classify::{Classification, Excluded, logical};
use sparkles_owl::ofn::Renderer;
use sparkles_owl::profiles::{CheckOptions, Checker, Report};
use sparkles_owl::read::{self, Mode, ReadOptions};
use sparkles_owl::{Axiom, AxiomKind, ClassExpression, Entity, ObjectPropertyExpression, vocab};
use sparkles_reasoner::inputs::{GraphRef, Import, ImportMode, MAX_IMPORTS};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

/// The commit a load of the file into an empty database makes, which answers name.
const COMMIT: u64 = 1;

/// An ontology read from one file, held in memory.
pub struct OntologyFile {
    /// the file's name in answers
    pub name: String,
    pub syntax: RdfSyntax,
    /// the terminology, as an open ontology of the loaded file holds it
    pub ontology: Arc<read::Ontology>,
    /// the graphs read, which is the default graph
    pub graphs: Vec<GraphRef>,
    pub imports: Vec<Import>,
    pub warnings: Vec<String>,
    /// the triples outside the terminology, in the order a store scans them
    assertions: Vec<Triple>,
    /// an empty database with the file's prefixes, for the dataset defaults and the
    /// renderer a load of the file gives
    ds: Dataset,
}

/// A classification of an [`OntologyFile`].
pub struct FileClassification {
    pub classification: Classification,
    /// the axioms left out, as outside OWL 2 EL, or used only in part
    pub excluded: Vec<Excluded>,
}

impl OntologyFile {
    /// Read `path` as `req` asks, parsing on up to `threads` threads (0: one per core),
    /// or `None` when the store path reads the file instead (see the module).
    pub fn read(
        path: &Path,
        req: &OntologyRequest,
        threads: usize,
    ) -> Result<Option<OntologyFile>> {
        req.validate()?;
        if req.at.is_some() || req.inferences || req.graphs() != [GraphRef::Default] {
            return Ok(None);
        }
        let Ok(src) = Source::from_path(path, None) else {
            return Ok(None);
        };
        let threads = match threads {
            0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
            n => n,
        };
        let name = path.display().to_string();
        let file = match src.format {
            RdfSyntax::OwlFunctional if req.mode == Mode::Lenient => {
                // the store labels anonymous individuals in its own order, which a read
                // of the document does not reproduce
                let Ok((doc, false)) = crate::io::read_ofn(&src, threads) else {
                    return Ok(None);
                };
                let prefixes: BTreeMap<String, String> = doc.prefixes.iter().cloned().collect();
                let imports: Vec<String> = doc
                    .header
                    .imports
                    .iter()
                    .map(|i| i.as_str().to_string())
                    .collect();
                let Some((ontology, assertions)) = from_document(doc, req.mode) else {
                    return Ok(None);
                };
                OntologyFile::new(
                    name,
                    src.format,
                    ontology,
                    assertions,
                    prefixes,
                    (imports, Vec::new()),
                    req,
                )?
            }
            RdfSyntax::Rdf(f) if !f.supports_datasets() => {
                let Ok((triples, prefixes)) = crate::io::parse_triples(&src, threads) else {
                    return Ok(None);
                };
                // the store's labels for blank nodes in triple terms and composite
                // literals, and its warnings for imports that are not IRIs, which name
                // its labels, are left to it
                let nested = |t: &Term| match t {
                    Term::Triple(_) => true,
                    Term::Literal(l) => crate::sparql::cdt::may_name_bnodes(l),
                    _ => false,
                };
                if triples.par_iter().any(|t| nested(&t.object)) {
                    return Ok(None);
                }
                let triples = distinct(triples, threads);
                let Some(imports) = imports_of(&triples) else {
                    return Ok(None);
                };
                let Some((ontology, assertions)) = from_triples(triples, req.mode, threads) else {
                    return Ok(None);
                };
                OntologyFile::new(
                    name, src.format, ontology, assertions, prefixes, imports, req,
                )?
            }
            _ => return Ok(None),
        };
        Ok(Some(file))
    }

    fn new(
        name: String,
        syntax: RdfSyntax,
        ontology: read::Ontology,
        assertions: Vec<Triple>,
        prefixes: BTreeMap<String, String>,
        (iris, mut warnings): (Vec<String>, Vec<String>),
        req: &OntologyRequest,
    ) -> Result<OntologyFile> {
        let ds = Dataset::memory();
        ds.store().add_prefixes(prefixes)?;
        // the store's resolution of the imports of the default graph, which is the only
        // graph: none resolves
        let mut imports = Vec::new();
        if req.imports != ImportMode::None {
            let mut seen = BTreeSet::new();
            for iri in iris {
                if !seen.insert(iri.clone()) {
                    continue;
                }
                if imports.len() >= MAX_IMPORTS {
                    warnings.push(format!(
                        "more than {MAX_IMPORTS} imports: <{iri}> and later ones were not followed"
                    ));
                    break;
                }
                warnings.push(format!("owl:imports <{iri}> did not resolve to a graph"));
                imports.push(Import {
                    iri,
                    location: None,
                    graph: None,
                });
            }
        } else {
            warnings.clear();
        }
        Ok(OntologyFile {
            name,
            syntax,
            ontology: Arc::new(ontology),
            graphs: vec![GraphRef::Default],
            imports,
            warnings,
            assertions,
            ds,
        })
    }

    /// An ontology file of a document's axioms held in memory, as a chained command
    /// hands it on, with `prefixes` for the renderer. The axioms are split as a read of a
    /// functional-style document splits them: an assertion that a store would stream
    /// becomes its triples, which `assertions` options then include or leave out, and
    /// every other axiom is in the terminology as it is. The imports of the header do not
    /// resolve, as a file's do not.
    pub fn from_document(
        name: String,
        doc: sparkles_owl::transform::Document,
        prefixes: BTreeMap<String, String>,
    ) -> Result<OntologyFile> {
        let imports: Vec<String> = doc
            .header
            .imports
            .iter()
            .map(|i| i.as_str().to_string())
            .collect();
        let declared: FxHashSet<String> = doc
            .axioms
            .iter()
            .filter_map(|a| match &a.kind {
                AxiomKind::Declaration(Entity::AnnotationProperty(p)) => {
                    Some(p.as_str().to_string())
                }
                _ => None,
            })
            .collect();
        let mut fresh = sparkles_owl::write::counter("file");
        let mut terminology = Vec::with_capacity(doc.axioms.len());
        let mut assertions = Vec::new();
        for ax in doc.axioms {
            if streamed(&ax, &declared) {
                assertions.extend(sparkles_owl::write::axiom_triples(&ax, &mut fresh));
            } else {
                terminology.push(ax);
            }
        }
        let order = StoreOrder::new(&assertions);
        assertions.par_sort_by_cached_key(|t| {
            (
                order.key(&t.subject.clone().into()),
                order.key(&Term::NamedNode(t.predicate.clone())),
                order.key(&t.object),
            )
        });
        assertions.dedup();
        let ontology = read::Ontology::from_axioms(doc.header, terminology, Mode::Lenient);
        OntologyFile::new(
            name,
            RdfSyntax::OwlFunctional,
            ontology,
            assertions,
            prefixes,
            (imports, Vec::new()),
            &OntologyRequest::default(),
        )
    }

    /// The header and every axiom of the file, the terminology and the assertions that
    /// map to axioms, as a document a command transforms. A triple that maps to no axiom
    /// is left out, as the reverse mapping leaves it out of the ontology.
    pub fn into_document(self, opts: &QueryOptions) -> Result<sparkles_owl::transform::Document> {
        let mut axioms: Vec<Axiom> = Vec::new();
        self.assertions(opts, |ax, _| {
            if let Some(ax) = ax {
                axioms.push(ax.into_owned());
            }
            Ok(true)
        })?;
        let (header, terminology): (_, Vec<Axiom>) = match Arc::try_unwrap(self.ontology) {
            Ok(o) => (o.header, o.axioms.into_keys().collect()),
            Err(o) => (o.header.clone(), o.axioms.keys().cloned().collect()),
        };
        axioms.extend(terminology);
        Ok(sparkles_owl::transform::Document::new(header, axioms))
    }

    /// The members of the read that answers carry, as a read of the loaded file gives them.
    pub fn view(&self) -> ReadView<'_> {
        const NO_INFERENCES: Option<&Inferences> = None;
        ReadView {
            commit: COMMIT,
            graphs: &self.graphs,
            imports: &self.imports,
            closure: &[],
            warnings: &self.warnings,
            inferences: NO_INFERENCES,
        }
    }

    /// A renderer with the prefixes a load of the file gives a database.
    pub fn renderer(&self) -> Renderer {
        Renderer::new(self.ds.prefixes())
    }

    /// The prefixes a load of the file gives a database.
    pub fn prefixes(&self) -> BTreeMap<String, String> {
        self.ds.prefixes()
    }

    /// Stream the assertions as [`Opened::assertions`](super::Opened::assertions) does:
    /// `f` gets the axiom, or `None` for a triple that maps to no axiom, and returns
    /// `false` to stop.
    pub fn assertions(
        &self,
        opts: &QueryOptions,
        mut f: impl FnMut(Option<std::borrow::Cow<'_, Axiom>>, Option<&Triple>) -> Result<bool>,
    ) -> Result<()> {
        let opts = self.ds.ontology().anchored(opts)?;
        for (n, t) in self.assertions.iter().enumerate() {
            if (n + 1).is_multiple_of(4096) {
                check(&opts)?;
            }
            let ax = self.ontology.map_assertion(t).map(std::borrow::Cow::Owned);
            if !f(ax, Some(t))? {
                break;
            }
        }
        Ok(())
    }

    /// Check the ontology against `req.profiles`, as
    /// [`Ontologies::check`](super::Ontologies::check) checks the loaded file.
    pub fn check(&self, req: &CheckRequest, opts: &QueryOptions) -> Result<Vec<Report>> {
        let copts = CheckOptions {
            max_violations: req.max_violations,
        };
        let mut c = Checker::new(&self.ontology, &req.profiles, copts);
        for (ax, origins) in &self.ontology.axioms {
            c.axiom(ax, origins.first().map(|x| x.graph));
        }
        if req.assertions {
            self.assertions(opts, |ax, t| {
                match (ax, t) {
                    (Some(ax), _) => c.axiom(&ax, Some(0)),
                    (None, Some(t)) => c.unmapped(t, Some(0)),
                    (None, None) => {}
                }
                Ok(true)
            })?;
        }
        Ok(c.finish())
    }

    /// Classify the ontology by OWL 2 EL, as
    /// [`Ontologies::classify`](super::Ontologies::classify) classifies the loaded file,
    /// with the threads, deadline and budgets of `c` and `opts`. A file classification
    /// writes nothing, so `c.target` is refused.
    pub fn classify(&self, c: &ClassifyRequest, opts: &QueryOptions) -> Result<FileClassification> {
        if c.target.is_some() {
            return Err(Error::invalid(
                "a classification of a file writes nothing: leave out 'target'",
            ));
        }
        let opts = self.ds.ontology().anchored(opts)?;
        let mut owned: Vec<Axiom> = Vec::new();
        let mut axioms = logical_axioms(&self.ontology);
        if c.assertions {
            self.assertions(&opts, |ax, _| {
                if let Some(ax) = ax {
                    owned.push(ax.into_owned());
                }
                Ok(true)
            })?;
            axioms.extend(owned.iter().map(|a| &a.kind).filter(|k| logical(k)));
        }
        let (classification, excluded) = classify_once(&axioms, c, &opts)?;
        if let Some(about) = &c.about
            && classification
                .taxonomy
                .node_of(&sparkles_owl::Class::new(about.as_str()))
                .is_none()
        {
            return Err(Error::NotFound(format!(
                "the taxonomy has no class <{about}>"
            )));
        }
        Ok(FileClassification {
            classification,
            excluded,
        })
    }
}

/// The triples, each once, in the order of their first appearance, as a store holds
/// them. The triples are hashed in parallel chunks, and each of `threads` partitions of
/// the hashes finds the repeats among its own triples.
fn distinct(triples: Vec<Triple>, threads: usize) -> Vec<Triple> {
    use std::hash::BuildHasher;
    let hasher = rustc_hash::FxBuildHasher;
    let hashes: Vec<u64> = triples.par_iter().map(|t| hasher.hash_one(t)).collect();
    let parts = threads.clamp(1, 64) as u64;
    let repeats: Vec<Vec<usize>> = (0..parts)
        .into_par_iter()
        .map(|p| {
            let mut first: FxHashMap<u64, Vec<usize>> = FxHashMap::default();
            let mut out = Vec::new();
            for (i, &h) in hashes.iter().enumerate() {
                if h % parts != p {
                    continue;
                }
                let seen = first.entry(h).or_default();
                if seen.iter().any(|&j| triples[j] == triples[i]) {
                    out.push(i);
                } else {
                    seen.push(i);
                }
            }
            out
        })
        .collect();
    if repeats.iter().all(Vec::is_empty) {
        return triples;
    }
    let mut repeat = vec![false; triples.len()];
    for i in repeats.into_iter().flatten() {
        repeat[i] = true;
    }
    triples
        .into_iter()
        .zip(repeat)
        .filter_map(|(t, r)| (!r).then_some(t))
        .collect()
}

/// The `owl:imports` IRIs of the triples, sorted, and the store's warning for one import
/// that is a literal, or `None` when imports that are not IRIs need the store's labels or
/// order: a blank node, or more than one literal.
fn imports_of(triples: &[Triple]) -> Option<(Vec<String>, Vec<String>)> {
    let mut iris = BTreeSet::new();
    let mut warnings = Vec::new();
    for t in triples {
        if t.predicate.as_str() != vocab::OWL_IMPORTS {
            continue;
        }
        match &t.object {
            Term::NamedNode(n) => {
                iris.insert(n.as_str().to_string());
            }
            Term::Literal(l) if warnings.is_empty() => {
                warnings.push(format!("owl:imports {l} is not an IRI and was ignored"))
            }
            _ => return None,
        }
    }
    Some((iris.into_iter().collect(), warnings))
}

/// The terminology of a file's triples and its other triples, the assertions, selected
/// as an open ontology selects them from a store, or `None` when a reified axiom names
/// two sources, properties or targets, of which the store's read takes the one its scan
/// meets last.
fn from_triples(
    triples: Vec<Triple>,
    mode: Mode,
    threads: usize,
) -> Option<(read::Ontology, Vec<Triple>)> {
    let structural: FxHashSet<&str> = vocab::STRUCTURAL_PREDICATES
        .iter()
        .chain(vocab::FACETS.iter())
        .chain(vocab::BUILTIN_ANNOTATION_PROPERTIES.iter())
        .copied()
        .collect();
    let types: FxHashSet<&str> = vocab::STRUCTURAL_TYPES.iter().copied().collect();
    let chunk = triples.len().div_ceil(threads.max(1)).max(4096);
    // the structural predicates and types
    let mut taken: Vec<bool> = triples
        .par_iter()
        .with_min_len(chunk)
        .map(|t| {
            structural.contains(t.predicate.as_str())
                || t.predicate.as_str() == vocab::RDF_TYPE
                    && matches!(&t.object, Term::NamedNode(o) if types.contains(o.as_str()))
        })
        .collect();
    // the predicates of the annotation properties the file declares
    let declared: FxHashSet<&str> = triples
        .iter()
        .zip(&taken)
        .filter(|(t, k)| {
            **k && t.predicate.as_str() == vocab::RDF_TYPE
                && matches!(&t.object, Term::NamedNode(o) if o.as_str() == vocab::OWL_ANNOTATION_PROPERTY)
        })
        .filter_map(|(t, _)| match &t.subject {
            NamedOrBlankNode::NamedNode(n) => Some(n.as_str()),
            NamedOrBlankNode::BlankNode(_) => None,
        })
        .filter(|p| !structural.contains(p))
        .collect();
    let mut terminological: FxHashSet<&str> = structural.clone();
    if !declared.is_empty() {
        let more: Vec<bool> = triples
            .par_iter()
            .with_min_len(chunk)
            .map(|t| declared.contains(t.predicate.as_str()))
            .collect();
        for (k, m) in taken.iter_mut().zip(more) {
            *k |= m;
        }
        terminological.extend(declared.iter().copied());
    }
    // class assertions of the class expressions the selected triples are about
    let nodes: FxHashSet<&oxrdf::BlankNode> = triples
        .iter()
        .zip(&taken)
        .filter(|(_, k)| **k)
        .filter_map(|(t, _)| match &t.subject {
            NamedOrBlankNode::BlankNode(b) => Some(b),
            NamedOrBlankNode::NamedNode(_) => None,
        })
        .collect();
    if !nodes.is_empty() {
        for (t, k) in triples.iter().zip(taken.iter_mut()) {
            if !*k
                && t.predicate.as_str() == vocab::RDF_TYPE
                && matches!(&t.object, Term::BlankNode(b) if nodes.contains(b))
            {
                *k = true;
            }
        }
    }
    // the main triples of annotated axioms whose predicate is not terminological
    let mut parts: FxHashMap<&NamedOrBlankNode, [Option<&Term>; 3]> = FxHashMap::default();
    for (t, _) in triples.iter().zip(&taken).filter(|(_, k)| **k) {
        let slot = match t.predicate.as_str() {
            vocab::OWL_ANNOTATED_SOURCE => 0,
            vocab::OWL_ANNOTATED_PROPERTY => 1,
            vocab::OWL_ANNOTATED_TARGET => 2,
            _ => continue,
        };
        let part = &mut parts.entry(&t.subject).or_default()[slot];
        if part.is_some_and(|o| o != &t.object) {
            return None;
        }
        *part = Some(&t.object);
    }
    let mains: FxHashSet<(&Term, &str, &Term)> = parts
        .values()
        .filter_map(|p| match p {
            [Some(s), Some(Term::NamedNode(p)), Some(o)]
                if !terminological.contains(p.as_str()) =>
            {
                Some((*s, p.as_str(), *o))
            }
            _ => None,
        })
        .collect();
    let mut main = vec![false; triples.len()];
    if !mains.is_empty() {
        for (i, t) in triples.iter().enumerate() {
            if taken[i] {
                continue;
            }
            let s: Term = t.subject.clone().into();
            if mains.contains(&(&s, t.predicate.as_str(), &t.object)) {
                main[i] = true;
            }
        }
    }
    // the assertions: every other triple, but not a class assertion of a structural type
    // or a class expression, which the terminology reads or leaves out
    let rest: Vec<bool> = triples
        .iter()
        .enumerate()
        .map(|(i, t)| {
            !taken[i]
                && !main[i]
                && !(t.predicate.as_str() == vocab::RDF_TYPE
                    && read::is_terminological(t, &|_| false))
        })
        .collect();
    // the order of the store's scans: the terminology by the predicates and types read
    // in turn, each by object and subject, and the assertions by subject, predicate and
    // object
    let order = StoreOrder::new(&triples);
    let preds: Vec<&str> = {
        let mut seen = FxHashSet::default();
        vocab::STRUCTURAL_PREDICATES
            .iter()
            .chain(vocab::FACETS.iter())
            .chain(vocab::BUILTIN_ANNOTATION_PROPERTIES.iter())
            .copied()
            .filter(|p| seen.insert(*p))
            .collect()
    };
    let mut group: FxHashMap<&str, u32> = preds
        .iter()
        .enumerate()
        .map(|(i, p)| (*p, i as u32))
        .collect();
    let type_group: FxHashMap<&str, u32> = vocab::STRUCTURAL_TYPES
        .iter()
        .enumerate()
        .map(|(i, t)| (*t, (preds.len() + i) as u32))
        .collect();
    let mut aps: Vec<&str> = declared.iter().copied().collect();
    aps.sort_unstable();
    let base = (preds.len() + vocab::STRUCTURAL_TYPES.len()) as u32;
    for (i, p) in aps.iter().enumerate() {
        group.insert(p, base + i as u32);
    }
    let classes = base + aps.len() as u32;
    let mut picked: Vec<usize> = (0..triples.len())
        .filter(|&i| taken[i] || main[i])
        .collect();
    picked.par_sort_by_cached_key(|&i| {
        let t = &triples[i];
        let g = if main[i] {
            u64::MAX
        } else if let Some(&g) = group.get(t.predicate.as_str()) {
            g as u64
        } else {
            match &t.object {
                Term::NamedNode(o) => {
                    type_group.get(o.as_str()).copied().unwrap_or(u32::MAX) as u64
                }
                // the class assertions of each class expression in turn
                Term::BlankNode(b) => classes as u64 + order.first[b.as_str()],
                _ => u64::MAX,
            }
        };
        if g == u64::MAX {
            return (g, (i as u64, Vec::new()), (0, Vec::new()));
        }
        (
            g,
            order.key(&t.object),
            order.key(&t.subject.clone().into()),
        )
    });
    let mut assertions: Vec<usize> = (0..triples.len()).filter(|&i| rest[i]).collect();
    assertions.par_sort_by_cached_key(|&i| {
        let t = &triples[i];
        (
            order.key(&t.subject.clone().into()),
            order.key(&Term::NamedNode(t.predicate.clone())),
            order.key(&t.object),
        )
    });
    drop((
        structural,
        types,
        declared,
        terminological,
        nodes,
        parts,
        mains,
        group,
        type_group,
        aps,
    ));
    let mut slots: Vec<Option<Triple>> = triples.into_iter().map(Some).collect();
    let mut terminology: Vec<Triple> = picked.into_iter().filter_map(|i| slots[i].take()).collect();
    let mut assertions: Vec<Triple> = assertions
        .into_iter()
        .filter_map(|i| slots[i].take())
        .collect();
    drop(slots);
    order.relabel(&mut terminology);
    order.relabel(&mut assertions);
    drop(order);
    let n = terminology.len();
    let o = read::read(
        terminology,
        vec![0; n],
        &[0],
        ReadOptions {
            mode,
            terminology_only: true,
            max_diagnostics: 0,
        },
    );
    Some((o, assertions))
}

/// The order of a store's ids, for the order of its scans: inline literals by their
/// value, blank nodes, here in the order the file first names them, and the
/// vocabulary's terms by their keys, with the tags of the ids between them.
struct StoreOrder {
    first: FxHashMap<String, u64>,
}

impl StoreOrder {
    fn new(ts: &[Triple]) -> StoreOrder {
        let mut first: FxHashMap<String, u64> = FxHashMap::default();
        for t in ts {
            let s = match &t.subject {
                NamedOrBlankNode::BlankNode(b) => Some(b),
                NamedOrBlankNode::NamedNode(_) => None,
            };
            let o = match &t.object {
                Term::BlankNode(b) => Some(b),
                _ => None,
            };
            for b in s.into_iter().chain(o) {
                if !first.contains_key(b.as_str()) {
                    let n = first.len() as u64;
                    first.insert(b.as_str().to_string(), n);
                }
            }
        }
        StoreOrder { first }
    }

    /// Give each blank node the label a store that numbered it gives it.
    fn relabel(&self, ts: &mut [Triple]) {
        use oxrdf::BlankNode;
        if self.first.is_empty() {
            return;
        }
        let label = |b: &BlankNode| {
            BlankNode::new_unchecked(crate::id::bnode_label(self.first[b.as_str()]))
        };
        ts.par_iter_mut().for_each(|t| {
            if let NamedOrBlankNode::BlankNode(b) = &t.subject {
                t.subject = label(b).into();
            }
            if let Term::BlankNode(b) = &t.object {
                t.object = label(b).into();
            }
        });
    }

    fn key(&self, t: &Term) -> (u64, Vec<u8>) {
        use crate::id::{Tag, inline_id, term_key};
        let shift = u64::BITS - 4;
        match t {
            Term::BlankNode(b) => (
                ((Tag::BNode as u64) << shift) | self.first[b.as_str()],
                Vec::new(),
            ),
            t => match inline_id(t) {
                Some(id) => (id.0, Vec::new()),
                None => ((Tag::Vocab as u64) << shift, term_key(t)),
            },
        }
    }
}

/// The terminology of a functional-style document and the assertions a store would
/// stream, each once, or `None` when the reverse mapping of the document's triples could
/// read them otherwise (see [`round_trips`]).
fn from_document(
    doc: sparkles_owl::ofn_read::Document,
    mode: Mode,
) -> Option<(read::Ontology, Vec<Triple>)> {
    if !round_trips(&doc) {
        return None;
    }
    let declared: FxHashSet<String> = doc
        .axioms
        .iter()
        .filter_map(|a| match &a.kind {
            AxiomKind::Declaration(Entity::AnnotationProperty(p)) => Some(p.as_str().to_string()),
            _ => None,
        })
        .collect();
    let mut terminology = Vec::with_capacity(doc.axioms.len());
    let mut assertions = Vec::new();
    // a streamed assertion is one triple, or a triple of each pair of `SameIndividual`,
    // and names no blank node the writer makes
    let mut fresh = sparkles_owl::write::counter("file");
    for ax in doc.axioms {
        if streamed(&ax, &declared) {
            assertions.extend(sparkles_owl::write::axiom_triples(&ax, &mut fresh));
        } else {
            pairwise(ax, &mut terminology);
        }
    }
    // an annotated axiom's main triple is also the triple of the same axiom without
    // annotations, which a store then reads only once
    let annotated: FxHashSet<&AxiomKind> = terminology
        .iter()
        .filter(|a| !a.annotations.is_empty())
        .map(|a| &a.kind)
        .collect();
    if !annotated.is_empty()
        && terminology
            .iter()
            .any(|a| a.annotations.is_empty() && annotated.contains(&a.kind))
    {
        return None;
    }
    drop(annotated);
    // the main triple of an annotated assertion is read with the terminology, and the
    // store streams it no more
    let mains: FxHashSet<Triple> = terminology
        .iter()
        .filter(|a| !a.annotations.is_empty())
        .filter_map(|a| {
            let plain = Axiom::from(a.kind.clone());
            streamed(&plain, &declared)
                .then(|| sparkles_owl::write::axiom_triples(&plain, &mut fresh))
        })
        .flatten()
        .collect();
    if !mains.is_empty() {
        assertions.retain(|t| !mains.contains(t));
    }
    // the store's set of triples, in the order it scans them
    let order = StoreOrder::new(&assertions);
    assertions.par_sort_by_cached_key(|t| {
        (
            order.key(&t.subject.clone().into()),
            order.key(&Term::NamedNode(t.predicate.clone())),
            order.key(&t.object),
        )
    });
    assertions.dedup();
    Some((
        read::Ontology::from_axioms(doc.header, terminology, mode),
        assertions,
    ))
}

/// Whether the reverse mapping of a functional-style document's triples gives its
/// axioms back as they are written, but for the n-ary axioms that [`pairwise`] splits.
/// That needs every entity declared with the kind its position gives it, by a
/// declaration or one that Table 6 of the mapping implies, or built in, no
/// IRI declared both as two kinds of property or as a class and a datatype, no entity of
/// the reserved vocabulary that is not built in, no mention of the ontology's IRI or
/// version IRI in an axiom, no `ObjectInverseOf` but in a property assertion without
/// annotations, no two annotations of one subject with the same property and value, no
/// facet outside OWL 2's or nesting near the reader's limits ([`readable`]), and
/// every n-ary axiom with at least two operands, an equivalence of three or more with
/// named operands only. The header's annotations are held to the same rules.
fn round_trips(doc: &sparkles_owl::ofn_read::Document) -> bool {
    use sparkles_owl::{EntityKind, ObjectPropertyExpression as Ope};
    let bit = |k: EntityKind| 1u8 << (k as u8);
    let mut kinds: FxHashMap<&str, u8> = FxHashMap::default();
    for a in &doc.axioms {
        match &a.kind {
            AxiomKind::Declaration(e) => {
                *kinds.entry(e.iri().as_str()).or_default() |= bit(e.kind());
            }
            // the declarations that Table 6 of the mapping implies
            AxiomKind::TransitiveObjectProperty(Ope::Property(p))
            | AxiomKind::SymmetricObjectProperty(Ope::Property(p))
            | AxiomKind::InverseFunctionalObjectProperty(Ope::Property(p)) => {
                *kinds.entry(p.as_str()).or_default() |= bit(EntityKind::ObjectProperty);
            }
            _ => {}
        }
    }
    let properties = bit(EntityKind::ObjectProperty)
        | bit(EntityKind::DataProperty)
        | bit(EntityKind::AnnotationProperty);
    let class_datatype = bit(EntityKind::Class) | bit(EntityKind::Datatype);
    let ambiguous = kinds
        .values()
        .any(|&k| (k & properties).count_ones() > 1 || k & class_datatype == class_datatype);
    if ambiguous {
        return false;
    }
    let header: Vec<&str> = [&doc.header.iri, &doc.header.version_iri]
        .into_iter()
        .flatten()
        .map(|i| i.as_str())
        .collect();
    let operands = |k: &AxiomKind| -> bool {
        match k {
            AxiomKind::EquivalentClasses(cs) => {
                cs.len() == 2
                    || cs.len() > 2 && cs.iter().all(|c| matches!(c, ClassExpression::Class(_)))
            }
            AxiomKind::EquivalentObjectProperties(ps) => {
                ps.len() == 2 || ps.len() > 2 && ps.iter().all(|p| matches!(p, Ope::Property(_)))
            }
            AxiomKind::EquivalentDataProperties(ps) => ps.len() >= 2,
            AxiomKind::SameIndividual(is) | AxiomKind::DifferentIndividuals(is) => is.len() >= 2,
            AxiomKind::DisjointClasses(cs) => cs.len() >= 2,
            AxiomKind::DisjointUnion(_, cs) => cs.len() >= 2,
            AxiomKind::DisjointObjectProperties(ps) => ps.len() >= 2,
            AxiomKind::DisjointDataProperties(ps) => ps.len() >= 2,
            _ => true,
        }
    };
    let typed = |e: &Entity| {
        let iri = e.iri().as_str();
        match read::builtin_kind(iri) {
            Some(k) => k == e.kind(),
            None => {
                !vocab::is_reserved(iri) && kinds.get(iri).is_some_and(|&k| k & bit(e.kind()) != 0)
            }
        }
    };
    // the header's annotations, whose triples the store reads as the header's only when
    // their properties are annotation properties
    let mut header_signature = sparkles_owl::Axiom::from(AxiomKind::Declaration(Entity::Class(
        sparkles_owl::Class::new(vocab::OWL_THING),
    )));
    header_signature.annotations = doc.header.annotations.clone();
    let (signature, shape) = header_signature.signature_and_shape();
    if collapses(&doc.header.annotations) || !readable(&shape) || !signature.iter().all(typed) {
        return false;
    }
    doc.axioms.par_iter().with_min_len(4096).all(|a| {
        let (signature, shape) = a.signature_and_shape();
        // an inverse property only in a plain property assertion, which maps to the
        // triple of the named property
        let plain_assertion =
            a.annotations.is_empty() && matches!(a.kind, AxiomKind::ObjectPropertyAssertion(..));
        operands(&a.kind)
            && (!shape.inverses || plain_assertion)
            && readable(&shape)
            && !collapses(&a.annotations)
            && !header.iter().any(|i| a.mentions(i))
            && signature.iter().all(typed)
    })
}

/// Whether the reverse mapping reads a structure of this shape whole: only OWL 2 facets,
/// expressions nested at most half as deep as [`read::MAX_NESTING`] and annotations
/// nested at most 16 deep, well within the reader's limits.
fn readable(shape: &sparkles_owl::Shape) -> bool {
    !shape.other_facets && shape.depth <= read::MAX_NESTING / 2 && shape.annotation_depth <= 16
}

/// Whether two of the annotations, or two annotations of one of them, at any depth, have
/// the same property and value, which the mapping writes as one triple.
fn collapses(annotations: &BTreeSet<sparkles_owl::Annotation>) -> bool {
    let mut seen = FxHashSet::default();
    annotations
        .iter()
        .any(|a| !seen.insert((&a.property, &a.value)) || collapses(&a.annotations))
}

/// Add the axiom to `out` as the reverse mapping reads its triples back: an equivalence
/// or an annotated `SameIndividual` of three or more operands as one axiom for each two
/// operands next to each other in its order, which the mapping writes as one triple.
fn pairwise(ax: Axiom, out: &mut Vec<Axiom>) {
    use std::collections::BTreeSet;
    fn pairs<T: Ord + Clone>(xs: &BTreeSet<T>) -> Option<Vec<BTreeSet<T>>> {
        if xs.len() <= 2 {
            return None;
        }
        let v: Vec<&T> = xs.iter().collect();
        Some(
            v.windows(2)
                .map(|w| [w[0].clone(), w[1].clone()].into())
                .collect(),
        )
    }
    let split: Option<Vec<AxiomKind>> = match &ax.kind {
        AxiomKind::EquivalentClasses(cs) => {
            pairs(cs).map(|ps| ps.into_iter().map(AxiomKind::EquivalentClasses).collect())
        }
        AxiomKind::EquivalentObjectProperties(ps) => pairs(ps).map(|ps| {
            ps.into_iter()
                .map(AxiomKind::EquivalentObjectProperties)
                .collect()
        }),
        AxiomKind::EquivalentDataProperties(ps) => pairs(ps).map(|ps| {
            ps.into_iter()
                .map(AxiomKind::EquivalentDataProperties)
                .collect()
        }),
        AxiomKind::SameIndividual(is) => {
            pairs(is).map(|ps| ps.into_iter().map(AxiomKind::SameIndividual).collect())
        }
        _ => None,
    };
    match split {
        Some(kinds) => out.extend(kinds.into_iter().map(|kind| Axiom {
            kind,
            annotations: ax.annotations.clone(),
        })),
        None => out.push(ax),
    }
}

/// Whether a store that loaded the axiom streams it as an assertion: its triple is not
/// terminological and, without annotations, it is not reified.
fn streamed(ax: &Axiom, declared: &FxHashSet<String>) -> bool {
    if !ax.annotations.is_empty() {
        return false;
    }
    // a predicate whose triples the terminology reads
    let read = |p: &str| {
        vocab::STRUCTURAL_PREDICATES.contains(&p)
            || vocab::FACETS.contains(&p)
            || vocab::is_builtin_annotation_property(p)
            || declared.contains(p)
    };
    match &ax.kind {
        // `a rdf:type C`, read with the terminology when C is a structural type
        AxiomKind::ClassAssertion(ClassExpression::Class(c), _) => {
            !vocab::STRUCTURAL_TYPES.contains(&c.as_str())
        }
        AxiomKind::ObjectPropertyAssertion(
            ObjectPropertyExpression::Property(p) | ObjectPropertyExpression::InverseOf(p),
            _,
            _,
        ) => !read(p.as_str()),
        AxiomKind::DataPropertyAssertion(p, _, _) => !read(p.as_str()),
        AxiomKind::AnnotationAssertion(p, _, _) => !read(p.as_str()),
        // owl:sameAs and owl:differentFrom; three or more different individuals are an
        // owl:AllDifferent node
        AxiomKind::SameIndividual(_) => true,
        AxiomKind::DifferentIndividuals(is) => is.len() == 2,
        _ => false,
    }
}
