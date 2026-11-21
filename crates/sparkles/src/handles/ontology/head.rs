//! The head of an ontology (roadmap item 735): the header, the counts of declared
//! entities, the commit, the graphs and the imports closure that the summary answers,
//! read from the index without mapping the whole graph into axioms.
//!
//! The header comes from a fragment, the quads of the ontology nodes and the blank nodes
//! and reifications they reach, with the types of the IRIs it uses, mapped by the same
//! reverse mapping as the full read. The entity counts come from scans of the `rdf:type`
//! triples of each declaring type, with the reverse mapping's OWL 1 rules, and the
//! subjects of reified `rdf:type` triples are counted by mapping their own fragment,
//! since an annotated declaration is an axiom of its own. Both read only the quads the
//! full read would read. A read that passes the bounds below opens the ontology instead,
//! so the head always answers what the summary does.

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
        let r = Reader::new(&view, &graph_ids, requested, req.mode, opts)?;
        let read = match r.header()? {
            Some(h) => r.entities()?.map(|e| (h, e)),
            None => None,
        };
        let (header, entities, opened) = match read {
            Some((h, e)) => (h, e, false),
            None => {
                let o = self.open_at(snap, req, opts)?;
                let s = o.summary();
                (s.header, s.entities, true)
            }
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
            opened,
        })
    }
}

/// The quads of a fragment, each once, in the order added.
#[derive(Default)]
struct Fragment {
    quads: Vec<[Id; 4]>,
    seen: FxHashSet<[Id; 4]>,
}

impl Fragment {
    fn add(&mut self, q: [Id; 4]) {
        if self.seen.insert(q) {
            self.quads.push(q);
        }
    }
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

    /// Add the quads of `subject` and of the blank nodes they reach to `f`, or `false`
    /// past [`MAX_FRAGMENT`].
    fn add_closure(&self, f: &mut Fragment, subject: Id) -> Result<bool> {
        let mut stack = vec![subject];
        let mut seen: FxHashSet<Id> = FxHashSet::default();
        seen.insert(subject);
        while let Some(s) = stack.pop() {
            let Some(qs) = self.kept_quads(Perm::Spo, &[s.0], MAX_FRAGMENT)? else {
                return Ok(false);
            };
            for q in qs {
                if self.is_bnode(q[2]) && seen.insert(q[2]) {
                    stack.push(q[2]);
                }
                f.add(q);
            }
            if f.quads.len() > MAX_FRAGMENT {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Add the reifications whose source is `x` to `f`, or `false` past the bounds.
    fn add_reifications(&self, f: &mut Fragment, x: Id) -> Result<bool> {
        let Some(src) = self.id(vocab::OWL_ANNOTATED_SOURCE) else {
            return Ok(true);
        };
        let Some(qs) = self.kept_quads(Perm::Pos, &[src.0, x.0], MAX_REIFIED)? else {
            return Ok(false);
        };
        for q in qs {
            if !self.add_closure(f, q[0])? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Add the kept `rdf:type` quads of every IRI the fragment uses, as context, or
    /// `false` past the bounds.
    fn add_types(&self, f: &mut Fragment) -> Result<bool> {
        let Some(t) = self.rdf_type else {
            return Ok(true);
        };
        let mut used: Vec<Id> = f
            .quads
            .iter()
            .flat_map(|q| [q[0], q[1], q[2]])
            .filter(|id| self.is_iri(*id))
            .collect();
        used.sort_unstable();
        used.dedup();
        for id in used {
            let Some(qs) = self.kept_quads(Perm::Spo, &[id.0, t.0], MAX_FRAGMENT)? else {
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
        for n in nodes {
            if !self.add_closure(&mut f, n)? || !self.add_reifications(&mut f, n)? {
                return Ok(None);
            }
        }
        if !self.add_types(&mut f)? {
            return Ok(None);
        }
        Ok(Some(self.map(&f).header))
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
                let Some(qs) = self.kept_quads(Perm::Spo, &[s.0, t.0], MAX_FRAGMENT)? else {
                    return Ok(None);
                };
                for q in qs {
                    f.add(q);
                }
                if !self.add_reifications(&mut f, *s)? || f.quads.len() > MAX_FRAGMENT {
                    return Ok(None);
                }
            }
            if !self.add_types(&mut f)? {
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
