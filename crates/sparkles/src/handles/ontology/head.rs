//! The head of an ontology (roadmap item 735): the header, the counts of declared
//! entities, the commit, the graphs and the imports closure that the summary answers,
//! read from the index without mapping the whole graph into axioms.
//!
//! The header comes from a fragment, the quads of the ontology nodes and of the
//! `owl:Axiom` and `owl:Annotation` nodes that reify their triples, nested ones included,
//! with the types of the IRIs it uses, mapped by the same reverse mapping as the full read.
//! The entity counts come from scans of the `rdf:type` triples of each declaring type, with
//! the reverse mapping's OWL 1 rules, and the subjects of reified `rdf:type` triples are
//! counted by mapping their own fragment, since an annotated declaration is an axiom of its
//! own.
//!
//! The head answers only when it can show that the full read's answer is a function of
//! its fragment, and opens the ontology otherwise. The rule is an allow-list on every node
//! the answer rests on. An ontology node holds only its type, imports, version IRIs and
//! annotations (rule A1). A reifier is a blank node with one source and one type, which
//! nothing else names, holding only its source, property, target and annotations (A2), and
//! the property it reifies is an annotation predicate or a root's role (A3). Any other
//! triple on such a node, another type or a reification of another kind opens the
//! ontology, as do the bounds below. The objects of annotations are not read, since the
//! full read takes an annotation's value as it is.

use super::{
    GraphRef, INFERRED_GRAPH, Import, ImportEdge, Inferences, Ontologies, OntologyRequest,
    ReadView, check, triple_of,
};
use crate::error::Result;
use crate::id::Id;
use crate::index::Perm;
use crate::sparql::QueryOptions;
use crate::store::{Chunk, Snapshot, TermKind};
use oxrdf::Term;
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_owl::read::{self, Mode, ReadOptions};
use sparkles_owl::{AxiomKind, EntityKind, Header, vocab};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The most ontology nodes the header fragment reads.
pub const MAX_NODES: usize = 1000;
/// The most quads the header fragment holds, its context included.
pub const MAX_FRAGMENT: usize = 100_000;
/// The most reified `rdf:type` triples the counts map.
pub const MAX_REIFIED: usize = 10_000;

/// The types that make an IRI a property of OWL, directly or by the OWL 1 rules.
const PROPERTY_TYPES: [&str; 11] = [
    vocab::OWL_ANNOTATION_PROPERTY,
    vocab::OWL_ONTOLOGY_PROPERTY,
    vocab::OWL_OBJECT_PROPERTY,
    vocab::OWL_DATATYPE_PROPERTY,
    vocab::OWL_FUNCTIONAL_PROPERTY,
    vocab::OWL_INVERSE_FUNCTIONAL_PROPERTY,
    vocab::OWL_TRANSITIVE_PROPERTY,
    vocab::OWL_SYMMETRIC_PROPERTY,
    vocab::OWL_ASYMMETRIC_PROPERTY,
    vocab::OWL_REFLEXIVE_PROPERTY,
    vocab::OWL_IRREFLEXIVE_PROPERTY,
];

/// What the head of an ontology answers.
#[derive(Clone, Debug)]
pub struct Head {
    pub commit: u64,
    /// the graphs read, as [`super::Opened::graphs`] numbers them
    pub graphs: Vec<GraphRef>,
    pub imports: Vec<Import>,
    pub closure: Vec<ImportEdge>,
    pub warnings: Vec<String>,
    pub inferences: Option<Inferences>,
    pub header: Header,
    /// the declarations by kind, as [`super::Summary::entities`] counts them
    pub entities: BTreeMap<EntityKind, usize>,
    /// whether a bound made the head open the whole ontology
    pub opened: bool,
}

impl Head {
    /// The members of the read that answers carry.
    pub fn view(&self) -> ReadView<'_> {
        ReadView {
            commit: self.commit,
            graphs: &self.graphs,
            imports: &self.imports,
            closure: &self.closure,
            warnings: &self.warnings,
            inferences: self.inferences.as_ref(),
        }
    }
}

impl Ontologies {
    /// The head of the ontology `req` names, as the caller of `opts` may read it: what
    /// [`open`](Self::open) and [`Opened::summary`](super::Opened::summary) give for the
    /// header, the entity counts, the graphs and the imports, from bounded reads.
    pub fn head(&self, req: &OntologyRequest, opts: &QueryOptions) -> Result<Head> {
        req.validate()?;
        let (snap, opts) = self.state(req, opts)?;
        self.head_at(&snap, req, &opts)
    }

    /// [`head`](Self::head) at a state the caller has.
    pub fn head_at(
        &self,
        snap: &Arc<Snapshot>,
        req: &OntologyRequest,
        opts: &QueryOptions,
    ) -> Result<Head> {
        req.validate()?;
        // the view, the graphs and the inferences as `open_at` reads them
        let access = opts.graphs.as_ref().map(|a| a.bound(snap));
        let view = match &access {
            Some(a) => a.masked(snap)?,
            None => snap.clone(),
        };
        let record = if req.inferences {
            self.ds.reasoning_record()
        } else {
            None
        };
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
        let inferences = self.inferences_of(&graphs, &graph_ids, view.commit, record.as_ref());
        check(opts)?;
        let r = Reader::new(&view, &graph_ids, requested, req.mode, opts)?;
        let read = match r.usual()? {
            false => None,
            true => match r.header()? {
                Some(h) => r.entities()?.map(|e| (h, e)),
                None => None,
            },
        };
        check(opts)?;
        let Some((header, entities)) = read else {
            // the whole answer from the open ontology, whose reasoning record can be newer
            // than the one read above
            let o = self.open_at(snap, req, opts)?;
            let s = o.summary();
            return Ok(Head {
                commit: o.snapshot.commit,
                graphs: o.graphs.clone(),
                imports: o.imports.clone(),
                closure: o.closure.clone(),
                warnings: o.warnings.clone(),
                inferences: o.inferences.clone(),
                header: s.header,
                entities: s.entities,
                opened: true,
            });
        };
        Ok(Head {
            commit: view.commit,
            graphs,
            imports: resolved.imports,
            closure,
            warnings: resolved.warnings,
            inferences,
            header,
            entities,
            opened: false,
        })
    }
}

/// The quads of a fragment, each once, in the order added.
#[derive(Default)]
struct Fragment {
    quads: Vec<[Id; 4]>,
    seen: FxHashSet<[Id; 4]>,
    /// the subjects whose quads and reifications were read
    visited: FxHashSet<Id>,
}

impl Fragment {
    fn add(&mut self, q: [Id; 4]) {
        if self.seen.insert(q) {
            self.quads.push(q);
        }
    }
}

/// The kind of a root of a fragment: an ontology node of the header or the subject of a
/// reified `rdf:type` triple of the counts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Root {
    Ontology,
    Subject,
}

/// Bounded reads of the quads the full read would read (`read_store`).
struct Reader<'a> {
    snap: &'a Snapshot,
    /// the graph number of each graph read, as `read_store` numbers them
    graphs: FxHashMap<Id, u32>,
    graph_count: usize,
    requested: usize,
    mode: Mode,
    opts: &'a QueryOptions,
    /// the predicates `read_store` reads every triple of
    terminological: FxHashSet<Id>,
    /// the types whose `rdf:type` triples `read_store` reads
    structural: FxHashSet<Id>,
    /// the structural predicates and the facets, which are terminological but never
    /// annotation predicates
    reserved: FxHashSet<Id>,
    rdf_type: Option<Id>,
    n: Cell<usize>,
}

impl<'a> Reader<'a> {
    fn new(
        snap: &'a Snapshot,
        graph_ids: &[Option<Id>],
        requested: usize,
        mode: Mode,
        opts: &'a QueryOptions,
    ) -> Result<Reader<'a>> {
        let graphs: FxHashMap<Id, u32> = graph_ids
            .iter()
            .enumerate()
            .filter_map(|(n, g)| g.map(|g| (g, n as u32)))
            .collect();
        let mut r = Reader {
            snap,
            graphs,
            graph_count: graph_ids.len(),
            requested,
            mode,
            opts,
            terminological: FxHashSet::default(),
            structural: FxHashSet::default(),
            reserved: FxHashSet::default(),
            rdf_type: snap.lookup_iri(vocab::RDF_TYPE),
            n: Cell::new(0),
        };
        for p in vocab::STRUCTURAL_PREDICATES
            .iter()
            .chain(vocab::FACETS.iter())
            .chain(vocab::BUILTIN_ANNOTATION_PROPERTIES.iter())
        {
            if let Some(id) = snap.lookup_iri(p) {
                r.terminological.insert(id);
            }
        }
        for p in vocab::STRUCTURAL_PREDICATES
            .iter()
            .chain(vocab::FACETS.iter())
        {
            if let Some(id) = snap.lookup_iri(p) {
                r.reserved.insert(id);
            }
        }
        for t in vocab::STRUCTURAL_TYPES {
            if let Some(id) = snap.lookup_iri(t) {
                r.structural.insert(id);
            }
        }
        // the annotation properties declared in the graphs read
        if let (Some(t), Some(ap)) = (r.rdf_type, r.id(vocab::OWL_ANNOTATION_PROPERTY)) {
            let mut declared = Vec::new();
            r.each(Perm::Pos, &[t.0, ap.0], |q| {
                declared.push(q[0]);
                Ok(true)
            })?;
            r.terminological.extend(declared);
        }
        Ok(r)
    }

    /// Whether the reverse mapping reads the vocabulary as usual: no graph read types
    /// `rdf:type` or another structural predicate as a property of OWL, directly or by a
    /// reified type triple, which makes the reader take its triples as annotations or
    /// assertions (the head then opens the ontology).
    fn usual(&self) -> Result<bool> {
        let Some(t) = self.rdf_type else {
            return Ok(true);
        };
        let kinds: FxHashSet<Id> = PROPERTY_TYPES.iter().filter_map(|p| self.id(p)).collect();
        let (src, prop, tgt) = (
            self.id(vocab::OWL_ANNOTATED_SOURCE),
            self.id(vocab::OWL_ANNOTATED_PROPERTY),
            self.id(vocab::OWL_ANNOTATED_TARGET),
        );
        for p in vocab::STRUCTURAL_PREDICATES
            .iter()
            .chain([&vocab::RDF_TYPE])
        {
            let Some(p) = self.id(p) else { continue };
            let mut typed = false;
            self.each(Perm::Spo, &[p.0, t.0], |q| {
                typed |= kinds.contains(&q[2]);
                Ok(!typed)
            })?;
            if typed {
                return Ok(false);
            }
            let (Some(src), Some(prop), Some(tgt)) = (src, prop, tgt) else {
                continue;
            };
            let mut reifiers = Vec::new();
            self.each(Perm::Pos, &[src.0, p.0], |q| {
                reifiers.push(q[0]);
                Ok(reifiers.len() <= MAX_REIFIED)
            })?;
            if reifiers.len() > MAX_REIFIED {
                return Ok(false);
            }
            for r in reifiers {
                let mut typing = false;
                let mut target = false;
                self.each(Perm::Spo, &[r.0], |q| {
                    typing |= q[1] == prop && q[2] == t;
                    target |= q[1] == tgt && kinds.contains(&q[2]);
                    Ok(true)
                })?;
                if typing && target {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn id(&self, iri: &str) -> Option<Id> {
        self.snap.lookup_iri(iri)
    }

    fn is_iri(&self, id: Id) -> bool {
        self.snap.term_kind(id) == TermKind::Iri
    }

    fn is_bnode(&self, id: Id) -> bool {
        self.snap.term_kind(id) == TermKind::BNode
    }

    /// Whether the full read reads `q`, a quad of a graph read.
    fn kept(&self, q: &[Id; 4]) -> bool {
        self.terminological.contains(&q[1])
            || (Some(q[1]) == self.rdf_type && self.structural.contains(&q[2]))
    }

    /// Each quad of the graphs read whose key in `perm` starts with `prefix`, until `f`
    /// returns `false`.
    fn each(
        &self,
        perm: Perm,
        prefix: &[u64],
        mut f: impl FnMut([Id; 4]) -> Result<bool>,
    ) -> Result<()> {
        check(self.opts)?;
        let (graphs, opts, n) = (&self.graphs, self.opts, &self.n);
        let mut one = |q: [Id; 4]| -> Result<bool> {
            n.set(n.get() + 1);
            if n.get().is_multiple_of(4096) {
                check(opts)?;
            }
            if graphs.contains_key(&q[3]) {
                f(q)
            } else {
                Ok(true)
            }
        };
        self.snap.scan(perm, prefix, |c| match c {
            Chunk::Block(b, s, e) => {
                for i in s..e {
                    if !one(perm.to_quad(&b.key(i)))? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Chunk::Row(k) => one(perm.to_quad(&k)),
        })
    }

    /// The quads the full read reads whose key in `perm` starts with `prefix`, or `None`
    /// past `limit`.
    fn kept_quads(&self, perm: Perm, prefix: &[u64], limit: usize) -> Result<Option<Vec<[Id; 4]>>> {
        let mut out = Vec::new();
        let mut over = false;
        self.each(perm, prefix, |q| {
            if self.kept(&q) {
                if out.len() >= limit {
                    over = true;
                    return Ok(false);
                }
                out.push(q);
            }
            Ok(true)
        })?;
        Ok((!over).then_some(out))
    }

    /// Add the kept `rdf:type` quads of every IRI the fragment uses, as context, or
    /// `false` past the bounds. It is also `false` when a used IRI is the source of a
    /// reified `rdf:type` triple outside the fragment, since the full read takes that
    /// reification for a declaration of its own.
    fn add_types(&self, f: &mut Fragment) -> Result<bool> {
        let Some(t) = self.rdf_type else {
            return Ok(true);
        };
        let src = self.id(vocab::OWL_ANNOTATED_SOURCE);
        let prop = self.id(vocab::OWL_ANNOTATED_PROPERTY);
        let mut used: Vec<Id> = f
            .quads
            .iter()
            .flat_map(|q| [q[0], q[1], q[2]])
            .filter(|id| self.is_iri(*id))
            .collect();
        used.sort_unstable();
        used.dedup();
        for id in used {
            if let (Some(src), Some(prop)) = (src, prop) {
                let mut reifiers = Vec::new();
                self.each(Perm::Pos, &[src.0, id.0], |q| {
                    if !f.visited.contains(&q[0]) {
                        reifiers.push(q[0]);
                    }
                    Ok(reifiers.len() <= MAX_REIFIED)
                })?;
                if reifiers.len() > MAX_REIFIED {
                    return Ok(false);
                }
                for r in reifiers {
                    let mut typing = false;
                    self.each(Perm::Spo, &[r.0, prop.0, t.0], |_| {
                        typing = true;
                        Ok(false)
                    })?;
                    if typing {
                        return Ok(false);
                    }
                }
            }
            let room = MAX_FRAGMENT.saturating_sub(f.quads.len());
            let Some(qs) = self.kept_quads(Perm::Spo, &[id.0, t.0], room)? else {
                return Ok(false);
            };
            for q in qs {
                f.add(q);
            }
            if f.quads.len() > MAX_FRAGMENT {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The ontology the reverse mapping reads from `f`.
    fn map(&self, f: &Fragment) -> read::Ontology {
        let mut triples = Vec::with_capacity(f.quads.len());
        let mut graph_of = Vec::with_capacity(f.quads.len());
        for q in &f.quads {
            if let Some(t) = triple_of(self.snap, q) {
                triples.push(t);
                graph_of.push(self.graphs[&q[3]]);
            }
        }
        let primary: Vec<u32> = (0..self.requested.min(self.graph_count) as u32).collect();
        read::read(
            triples,
            graph_of,
            &primary,
            ReadOptions {
                mode: self.mode,
                terminology_only: true,
                max_diagnostics: 0,
            },
        )
    }

    /// The header the full read gives, or `None` past the bounds.
    fn header(&self) -> Result<Option<Header>> {
        let (Some(t), Some(onto)) = (self.rdf_type, self.id(vocab::OWL_ONTOLOGY)) else {
            return Ok(Some(Header::default()));
        };
        let Some(typed) = self.kept_quads(Perm::Pos, &[t.0, onto.0], MAX_NODES)? else {
            return Ok(None);
        };
        let mut nodes: Vec<Id> = typed.iter().map(|q| q[0]).collect();
        nodes.dedup();
        let mut f = Fragment::default();
        for n in &nodes {
            if !self.ontology_node(*n, onto)? {
                return Ok(None);
            }
            let room = MAX_FRAGMENT.saturating_sub(f.quads.len());
            let Some(qs) = self.kept_quads(Perm::Spo, &[n.0], room)? else {
                return Ok(None);
            };
            for q in qs {
                f.add(q);
            }
        }
        let roots: FxHashMap<Id, Root> = nodes.iter().map(|n| (*n, Root::Ontology)).collect();
        if !self.add_reifiers(&mut f, &roots)? || !self.add_types(&mut f)? {
            return Ok(None);
        }
        Ok(Some(self.map(&f).header))
    }

    /// An annotation predicate: one the full read reads that is neither structural nor a
    /// facet, so a built-in annotation property or one declared in the graphs read.
    fn annotation_predicate(&self, p: Id) -> bool {
        self.terminological.contains(&p) && !self.reserved.contains(&p)
    }

    /// Whether `x`, when it is a blank node, is the object of no triple of the graphs read
    /// other than `owl:annotatedSource`, so that no other axiom reaches it.
    fn unreferenced(&self, x: Id) -> Result<bool> {
        if !self.is_bnode(x) {
            return Ok(true);
        }
        let src = self.id(vocab::OWL_ANNOTATED_SOURCE);
        let mut ok = true;
        self.each(Perm::Osp, &[x.0], |q| {
            ok = Some(q[1]) == src;
            Ok(ok)
        })?;
        Ok(ok)
    }

    /// Rule A1 of the closure: the ontology node `n` has no other part in the mapping. Its
    /// triples that the full read reads are `rdf:type owl:Ontology`, `owl:imports`,
    /// `owl:versionIRI` and annotations, and a blank node is the object of nothing but
    /// `owl:annotatedSource`. A second part, such as an `owl:AllDisjointClasses` node, can
    /// take its annotations for an axiom's on triples outside the head's reads.
    fn ontology_node(&self, n: Id, onto: Id) -> Result<bool> {
        let role: FxHashSet<Id> = [vocab::OWL_IMPORTS, vocab::OWL_VERSION_IRI]
            .iter()
            .filter_map(|p| self.id(p))
            .collect();
        let mut ok = true;
        self.each(Perm::Spo, &[n.0], |q| {
            ok = if Some(q[1]) == self.rdf_type {
                // a blank class is read as a class assertion's class
                !self.is_bnode(q[2]) && (!self.kept(&q) || q[2] == onto)
            } else {
                !self.kept(&q) || role.contains(&q[1]) || self.annotation_predicate(q[1])
            };
            Ok(ok)
        })?;
        Ok(ok && self.unreferenced(n)?)
    }

    /// Rules A2 and A3 of the closure for `r`, a node that names a root or another
    /// reifier as `owl:annotatedSource`, whose source is the root of kind `root` or a
    /// reifier (`None`). `r` is a blank node with one source that nothing else names, typed
    /// `owl:Annotation`, or `owl:Axiom` when it reifies a root's triple, and nothing else.
    /// Its other triples that the full read reads are annotations, and the property it
    /// reifies is an annotation predicate, `rdf:type` of a root with a structural type, or
    /// `owl:imports` or `owl:versionIRI` of an ontology node, so that its main triple is
    /// one the fragment holds.
    fn reifier(&self, r: Id, root: Option<Root>) -> Result<bool> {
        if !self.is_bnode(r) || !self.unreferenced(r)? {
            return Ok(false);
        }
        let (src, prop, tgt) = (
            self.id(vocab::OWL_ANNOTATED_SOURCE),
            self.id(vocab::OWL_ANNOTATED_PROPERTY),
            self.id(vocab::OWL_ANNOTATED_TARGET),
        );
        let (axiom, annotation) = (self.id(vocab::OWL_AXIOM), self.id(vocab::OWL_ANNOTATION));
        let header_roles: FxHashSet<Id> = [vocab::OWL_IMPORTS, vocab::OWL_VERSION_IRI]
            .iter()
            .filter_map(|p| self.id(p))
            .collect();
        let mut types: FxHashSet<Id> = FxHashSet::default();
        let mut sources: FxHashSet<Id> = FxHashSet::default();
        let mut targets: Vec<Id> = Vec::new();
        let mut typing = false;
        let mut ok = true;
        self.each(Perm::Spo, &[r.0], |q| {
            ok = if Some(q[1]) == self.rdf_type {
                if self.is_bnode(q[2]) {
                    false
                } else {
                    if self.kept(&q) {
                        types.insert(q[2]);
                    }
                    true
                }
            } else if Some(q[1]) == src {
                sources.insert(q[2]);
                true
            } else if Some(q[1]) == prop {
                typing |= Some(q[2]) == self.rdf_type;
                self.annotation_predicate(q[2])
                    || (root.is_some() && Some(q[2]) == self.rdf_type)
                    || (root == Some(Root::Ontology) && header_roles.contains(&q[2]))
            } else if Some(q[1]) == tgt {
                targets.push(q[2]);
                true
            } else {
                !self.kept(&q) || self.annotation_predicate(q[1])
            };
            Ok(ok)
        })?;
        if !ok || sources.len() != 1 || types.len() != 1 {
            return Ok(false);
        }
        // a reified type triple whose type is not structural is a main triple that
        // `read_store` reads outside the fragment, and the choice of the header reads every
        // triple of an ontology node
        if typing && !targets.iter().all(|t| self.structural.contains(t)) {
            return Ok(false);
        }
        let ty = types.into_iter().next();
        Ok(ty == annotation && annotation.is_some()
            || ty == axiom && axiom.is_some() && root.is_some())
    }

    /// Add to `f` the kept quads of every node that names a root of `roots` or another
    /// such node as `owl:annotatedSource`, the closure R, or `false` when one of them
    /// breaks rules A2 and A3 or the bounds pass. The objects of annotations are not
    /// visited, since the full read takes an annotation's value as it is.
    fn add_reifiers(&self, f: &mut Fragment, roots: &FxHashMap<Id, Root>) -> Result<bool> {
        let Some(src) = self.id(vocab::OWL_ANNOTATED_SOURCE) else {
            return Ok(true);
        };
        let mut stack: Vec<Id> = roots.keys().copied().collect();
        stack.sort_unstable();
        f.visited.extend(stack.iter().copied());
        while let Some(x) = stack.pop() {
            let mut reifiers = Vec::new();
            self.each(Perm::Pos, &[src.0, x.0], |q| {
                reifiers.push(q[0]);
                Ok(reifiers.len() <= MAX_REIFIED)
            })?;
            if reifiers.len() > MAX_REIFIED {
                return Ok(false);
            }
            reifiers.sort_unstable();
            reifiers.dedup();
            for r in reifiers {
                if f.visited.contains(&r) {
                    // a reifier of a root or a reifier of another one, already checked
                    if roots.contains_key(&r) {
                        return Ok(false);
                    }
                    continue;
                }
                if !self.reifier(r, roots.get(&x).copied())? {
                    return Ok(false);
                }
                let room = MAX_FRAGMENT.saturating_sub(f.quads.len());
                let Some(qs) = self.kept_quads(Perm::Spo, &[r.0], room)? else {
                    return Ok(false);
                };
                for q in qs {
                    f.add(q);
                }
                if f.quads.len() > MAX_FRAGMENT {
                    return Ok(false);
                }
                f.visited.insert(r);
                stack.push(r);
            }
        }
        Ok(true)
    }

    /// The distinct IRI subjects of `rdf:type ty` in the graphs read, in order, each
    /// passed to `f`.
    fn subjects(&self, ty: Option<Id>, mut f: impl FnMut(Id) -> Result<()>) -> Result<()> {
        let (Some(t), Some(ty)) = (self.rdf_type, ty) else {
            return Ok(());
        };
        let mut last: Option<Id> = None;
        let snap = self.snap;
        self.each(Perm::Pos, &[t.0, ty.0], |q| {
            if last != Some(q[0]) {
                last = Some(q[0]);
                if snap.term_kind(q[0]) == TermKind::Iri {
                    f(q[0])?;
                }
            }
            Ok(true)
        })
    }

    /// The set of [`subjects`](Self::subjects) of each type of `types`.
    fn subject_set(&self, types: &[&str]) -> Result<FxHashSet<Id>> {
        let mut out = FxHashSet::default();
        for ty in types {
            let id = self.id(ty);
            self.subjects(id, |s| {
                out.insert(s);
                Ok(())
            })?;
        }
        Ok(out)
    }

    /// Whether a graph read types `s` with `ty`.
    fn typed(&self, s: Id, ty: Option<Id>) -> Result<bool> {
        let (Some(t), Some(ty)) = (self.rdf_type, ty) else {
            return Ok(false);
        };
        let mut found = false;
        self.each(Perm::Spo, &[s.0, t.0, ty.0], |_| {
            found = true;
            Ok(false)
        })?;
        Ok(found)
    }

    /// The declarations by kind that the full read gives, or `None` past the bounds.
    fn entities(&self) -> Result<Option<BTreeMap<EntityKind, usize>>> {
        let mut out: BTreeMap<EntityKind, usize> = BTreeMap::new();
        // the subjects of reified type triples: an annotated declaration is an axiom of
        // its own, so these are counted by mapping their fragment
        let Some(reified) = self.reified()? else {
            return Ok(None);
        };
        if !reified.is_empty() {
            let mut f = Fragment::default();
            for s in &reified {
                let Some(t) = self.rdf_type else { break };
                let room = MAX_FRAGMENT.saturating_sub(f.quads.len());
                let Some(qs) = self.kept_quads(Perm::Spo, &[s.0, t.0], room)? else {
                    return Ok(None);
                };
                for q in qs {
                    f.add(q);
                }
            }
            // the reifications of the subjects' triples, whose closure rule keeps every
            // annotation of a declaration inside the fragment
            let roots: FxHashMap<Id, Root> = reified.iter().map(|s| (*s, Root::Subject)).collect();
            if !self.add_reifiers(&mut f, &roots)? || !self.add_types(&mut f)? {
                return Ok(None);
            }
            let own: FxHashSet<String> = reified
                .iter()
                .filter_map(|s| match self.snap.term(*s) {
                    Some(Term::NamedNode(n)) => Some(n.into_string()),
                    _ => None,
                })
                .collect();
            for ax in self.map(&f).axiom_set() {
                if let AxiomKind::Declaration(e) = &ax.kind
                    && own.contains(e.iri().as_str())
                {
                    *out.entry(e.kind()).or_insert(0) += 1;
                }
            }
        }
        let lenient = self.mode == Mode::Lenient;
        // the types that declare each kind, the first one's subjects counted at once and
        // each later one's when no earlier type declares them, with the subjects that the
        // OWL 1 rules (tables 5 and 6) leave out
        let restrictions = self.subject_set(&[vocab::OWL_RESTRICTION])?;
        let rdfs_class_removed = [
            vocab::OWL_CLASS,
            vocab::RDFS_DATATYPE,
            vocab::OWL_DATA_RANGE,
            vocab::OWL_RESTRICTION,
        ];
        type Rule = (EntityKind, &'static [&'static str]);
        let mut rules: Vec<Rule> = vec![
            (EntityKind::Class, &[vocab::OWL_CLASS]),
            (
                EntityKind::ObjectProperty,
                &[
                    vocab::OWL_OBJECT_PROPERTY,
                    vocab::OWL_INVERSE_FUNCTIONAL_PROPERTY,
                    vocab::OWL_TRANSITIVE_PROPERTY,
                    vocab::OWL_SYMMETRIC_PROPERTY,
                ],
            ),
            (EntityKind::DataProperty, &[vocab::OWL_DATATYPE_PROPERTY]),
            (
                EntityKind::AnnotationProperty,
                &[vocab::OWL_ANNOTATION_PROPERTY, vocab::OWL_ONTOLOGY_PROPERTY],
            ),
            (EntityKind::Datatype, &[vocab::RDFS_DATATYPE]),
            (EntityKind::NamedIndividual, &[vocab::OWL_NAMED_INDIVIDUAL]),
        ];
        if lenient {
            rules[0].1 = &[vocab::OWL_CLASS, vocab::RDFS_CLASS];
        }
        for (kind, types) in rules {
            let ids: Vec<Option<Id>> = types.iter().map(|t| self.id(t)).collect();
            let mut n = 0usize;
            for (i, ty) in types.iter().enumerate() {
                let mut candidates = Vec::new();
                let first = i == 0;
                self.subjects(ids[i], |s| {
                    if reified.contains(&s) {
                        return Ok(());
                    }
                    if *ty == vocab::OWL_CLASS && restrictions.contains(&s) {
                        return Ok(());
                    }
                    if first {
                        n += 1;
                    } else {
                        candidates.push(s);
                    }
                    Ok(())
                })?;
                'next: for s in candidates {
                    for earlier in &ids[..i] {
                        if self.typed(s, *earlier)? {
                            continue 'next;
                        }
                    }
                    if *ty == vocab::RDFS_CLASS {
                        for x in rdfs_class_removed {
                            let x = self.id(x);
                            if self.typed(s, x)? {
                                continue 'next;
                            }
                        }
                    }
                    n += 1;
                }
            }
            if n > 0 {
                *out.entry(kind).or_insert(0) += n;
            }
        }
        Ok(Some(out))
    }

    /// The IRI sources of the reified `rdf:type` triples in the graphs read, or `None`
    /// past [`MAX_REIFIED`].
    fn reified(&self) -> Result<Option<FxHashSet<Id>>> {
        let (Some(t), Some(prop), Some(src)) = (
            self.rdf_type,
            self.id(vocab::OWL_ANNOTATED_PROPERTY),
            self.id(vocab::OWL_ANNOTATED_SOURCE),
        ) else {
            return Ok(Some(FxHashSet::default()));
        };
        let Some(reifiers) = self.kept_quads(Perm::Pos, &[prop.0, t.0], MAX_REIFIED)? else {
            return Ok(None);
        };
        let mut out = FxHashSet::default();
        for r in reifiers {
            let Some(qs) = self.kept_quads(Perm::Spo, &[r[0].0, src.0], 16)? else {
                return Ok(None);
            };
            for q in qs {
                if self.is_iri(q[2]) {
                    out.insert(q[2]);
                }
            }
        }
        Ok(Some(out))
    }
}
