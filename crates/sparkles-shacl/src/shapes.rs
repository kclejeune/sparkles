//! The shapes model and its parser (SHACL §2–4, Jena `org.apache.jena.shacl.parser`).
//!
//! A [`Shapes`] value is independent of any store: constants used during validation are
//! kept in a small term table ([`Tid`] indices) that [`crate::validate`] resolves to
//! store ids once per validation run.

use crate::path::PropertyPath;
use crate::sparql::{ComponentConstraint, SparqlComponent, SparqlConstraint};
use crate::syntax::ShapesSyntax;
use crate::vocab::{rdf, rdfs, sh};
use anyhow::{Context as _, Result, anyhow, bail};
use oxrdf::vocab::xsd;
use oxrdf::{Graph, Literal, NamedNode, NamedNodeRef, NamedOrBlankNodeRef, Term, TermRef, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles::io::Source;
use sparkles::sparql::value::Value;
use std::fmt;

/// Index into the term table of a [`Shapes`].
pub type Tid = u32;
/// Index of a shape in [`Shapes::shapes`].
pub type ShapeId = usize;

/// Target declarations (SHACL §2.1.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// `sh:targetNode`
    Node(Tid),
    /// `sh:targetClass`, or an implicit class target (the shape is also an `rdfs:Class`)
    Class(Tid),
    /// `sh:targetSubjectsOf`
    SubjectsOf(Tid),
    /// `sh:targetObjectsOf`
    ObjectsOf(Tid),
    /// `sh:targetWhere` (SHACL 1.2 Core §3.1.3.6): the nodes of the data graph that
    /// conform to the shape
    Where(ShapeId),
}

/// Where the nodes that may conform to a shape are found (see [`Shapes::candidates`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Candidates {
    /// every node of the data graph
    All,
    /// the SHACL instances of a class
    Instances(Tid),
    /// these terms
    Terms(Vec<Tid>),
    /// the subjects of a predicate
    SubjectsOf(NamedNode),
    /// the objects of a predicate
    ObjectsOf(NamedNode),
}

/// Values of `sh:nodeKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    BlankNode,
    Iri,
    Literal,
    BlankNodeOrIri,
    BlankNodeOrLiteral,
    IriOrLiteral,
}

impl NodeKind {
    fn from_iri(iri: &str) -> Option<NodeKind> {
        Some(match iri.strip_prefix(crate::vocab::SH_NS)? {
            "BlankNode" => NodeKind::BlankNode,
            "IRI" => NodeKind::Iri,
            "Literal" => NodeKind::Literal,
            "BlankNodeOrIRI" => NodeKind::BlankNodeOrIri,
            "BlankNodeOrLiteral" => NodeKind::BlankNodeOrLiteral,
            "IRIOrLiteral" => NodeKind::IriOrLiteral,
            _ => return None,
        })
    }

    pub fn iri(self) -> NamedNodeRef<'static> {
        match self {
            NodeKind::BlankNode => sh::BLANK_NODE,
            NodeKind::Iri => sh::IRI,
            NodeKind::Literal => sh::LITERAL,
            NodeKind::BlankNodeOrIri => sh::BLANK_NODE_OR_IRI,
            NodeKind::BlankNodeOrLiteral => sh::BLANK_NODE_OR_LITERAL,
            NodeKind::IriOrLiteral => sh::IRI_OR_LITERAL,
        }
    }

    /// Does a term of kind (iri, bnode, literal) match?
    pub fn matches(self, iri: bool, bnode: bool, literal: bool) -> bool {
        match self {
            NodeKind::BlankNode => bnode,
            NodeKind::Iri => iri,
            NodeKind::Literal => literal,
            NodeKind::BlankNodeOrIri => bnode || iri,
            NodeKind::BlankNodeOrLiteral => bnode || literal,
            NodeKind::IriOrLiteral => iri || literal,
        }
    }
}

/// A compiled `sh:pattern` (with `sh:flags`).
#[derive(Clone, Debug)]
pub struct Pattern {
    pub pattern: String,
    pub flags: Option<String>,
    pub regex: regex::Regex,
}

/// Qualified value shape parameters (`sh:qualifiedValueShape` and friends).
#[derive(Clone, Debug)]
pub struct Qualified {
    pub shape: ShapeId,
    pub min: Option<u64>,
    pub max: Option<u64>,
    pub disjoint: bool,
    /// sibling qualified value shapes (only when `disjoint`)
    pub siblings: Vec<ShapeId>,
}

/// One constraint (an instance of a constraint component with its parameter values).
#[derive(Clone, Debug)]
pub enum Constraint {
    Class(Tid),
    Datatype(NamedNode),
    NodeKind(NodeKind),
    MinCount(u64),
    MaxCount(u64),
    MinExclusive(Term, Value),
    MinInclusive(Term, Value),
    MaxExclusive(Term, Value),
    MaxInclusive(Term, Value),
    MinLength(u64),
    MaxLength(u64),
    Pattern(Box<Pattern>),
    LanguageIn(Vec<String>),
    UniqueLang,
    Equals(Tid),
    Disjoint(Tid),
    LessThan(Tid),
    LessThanOrEquals(Tid),
    Not(ShapeId),
    And(Vec<ShapeId>),
    Or(Vec<ShapeId>),
    Xone(Vec<ShapeId>),
    Node(ShapeId),
    Property(ShapeId),
    /// `sh:qualifiedMinCount` part of a qualified value shape
    QualifiedMin(Box<Qualified>),
    /// `sh:qualifiedMaxCount` part of a qualified value shape
    QualifiedMax(Box<Qualified>),
    /// `sh:closed true`: allowed predicates (property shape paths + ignored properties)
    Closed {
        allowed: Vec<Tid>,
        ignored: Vec<Term>,
    },
    HasValue(Tid),
    In(Vec<Tid>),
    /// `sh:sparql` (SHACL-SPARQL §5)
    Sparql(Box<SparqlConstraint>),
    /// SPARQL-based constraint component instance (SHACL-SPARQL §6)
    Component(Box<ComponentConstraint>),
    /// `sh:memberShape` (SHACL 1.2 Core): every member of each value node, a SHACL list,
    /// conforms to the shape
    MemberShape(ShapeId),
    /// `sh:minListLength` (SHACL 1.2 Core)
    MinListLength(u64),
    /// `sh:maxListLength` (SHACL 1.2 Core)
    MaxListLength(u64),
    /// `sh:uniqueMembers` (SHACL 1.2 Core): with `true`, no member repeats; with either
    /// value, each value node must be a SHACL list
    UniqueMembers(bool),
}

impl Constraint {
    /// IRI of the constraint component.
    pub fn component(&self) -> NamedNode {
        let c = match self {
            Constraint::Class(_) => sh::CLASS_CC,
            Constraint::Datatype(_) => sh::DATATYPE_CC,
            Constraint::NodeKind(_) => sh::NODE_KIND_CC,
            Constraint::MinCount(_) => sh::MIN_COUNT_CC,
            Constraint::MaxCount(_) => sh::MAX_COUNT_CC,
            Constraint::MinExclusive(..) => sh::MIN_EXCLUSIVE_CC,
            Constraint::MinInclusive(..) => sh::MIN_INCLUSIVE_CC,
            Constraint::MaxExclusive(..) => sh::MAX_EXCLUSIVE_CC,
            Constraint::MaxInclusive(..) => sh::MAX_INCLUSIVE_CC,
            Constraint::MinLength(_) => sh::MIN_LENGTH_CC,
            Constraint::MaxLength(_) => sh::MAX_LENGTH_CC,
            Constraint::Pattern(_) => sh::PATTERN_CC,
            Constraint::LanguageIn(_) => sh::LANGUAGE_IN_CC,
            Constraint::UniqueLang => sh::UNIQUE_LANG_CC,
            Constraint::Equals(_) => sh::EQUALS_CC,
            Constraint::Disjoint(_) => sh::DISJOINT_CC,
            Constraint::LessThan(_) => sh::LESS_THAN_CC,
            Constraint::LessThanOrEquals(_) => sh::LESS_THAN_OR_EQUALS_CC,
            Constraint::Not(_) => sh::NOT_CC,
            Constraint::And(_) => sh::AND_CC,
            Constraint::Or(_) => sh::OR_CC,
            Constraint::Xone(_) => sh::XONE_CC,
            Constraint::Node(_) => sh::NODE_CC,
            Constraint::Property(_) => sh::PROPERTY_CC,
            Constraint::QualifiedMin(_) => sh::QUALIFIED_MIN_COUNT_CC,
            Constraint::QualifiedMax(_) => sh::QUALIFIED_MAX_COUNT_CC,
            Constraint::Closed { .. } => sh::CLOSED_CC,
            Constraint::HasValue(_) => sh::HAS_VALUE_CC,
            Constraint::In(_) => sh::IN_CC,
            Constraint::Sparql(_) => sh::SPARQL_CC,
            Constraint::MemberShape(_) => sh::MEMBER_SHAPE_CC,
            Constraint::MinListLength(_) => sh::MIN_LIST_LENGTH_CC,
            Constraint::MaxListLength(_) => sh::MAX_LIST_LENGTH_CC,
            Constraint::UniqueMembers(_) => sh::UNIQUE_MEMBERS_CC,
            Constraint::Component(c) => return c.component.iri.clone(),
        };
        c.into_owned()
    }
}

/// A node shape or a property shape.
#[derive(Clone, Debug)]
pub struct Shape {
    /// the shape's node in the shapes graph (IRI or blank node)
    pub node: Term,
    /// `sh:path` (property shapes only)
    pub path: Option<PropertyPath>,
    pub targets: Vec<Target>,
    pub deactivated: bool,
    pub severity: NamedNode,
    pub messages: Vec<Literal>,
    pub constraints: Vec<Constraint>,
}

impl Shape {
    pub fn is_property_shape(&self) -> bool {
        self.path.is_some()
    }
}

/// A parsed shapes graph.
#[derive(Clone, Debug, Default)]
pub struct Shapes {
    pub(crate) terms: Vec<Term>,
    tindex: FxHashMap<Term, Tid>,
    pub(crate) shapes: Vec<Shape>,
    by_node: FxHashMap<Term, ShapeId>,
    /// blank nodes of this shapes graph are store blank nodes (read via
    /// [`Shapes::from_store`]) and may be matched against data nodes
    pub(crate) bnodes_in_store: bool,
    /// the store graph the shapes were read from (`None` = default graph), for
    /// `$shapesGraph` in SHACL-SPARQL
    pub(crate) source_graph: Option<String>,
}

impl Shapes {
    /// Parse a shapes graph from text: an RDF syntax (Turtle, N-Triples, RDF/XML,
    /// JSON-LD, TriG, N-Quads; all graphs of a quad format are merged), or SHACLC.
    pub fn parse(
        text: &str,
        syntax: impl Into<ShapesSyntax>,
        base: Option<&str>,
    ) -> Result<Shapes> {
        let doc = crate::syntax::read_document(text, syntax.into(), base)?;
        Shapes::from_graph(doc.graph)
    }

    /// Read shapes text (RDF or SHACLC) into a graph for
    /// [`Shapes::from_store_graphs_with`]. Its blank nodes get fresh labels, so they
    /// never name a blank node of the store.
    pub fn read_graph(
        text: &str,
        syntax: impl Into<ShapesSyntax>,
        base: Option<&str>,
    ) -> Result<Graph> {
        let syntax = syntax.into();
        if syntax == ShapesSyntax::Compact {
            // the reader's blank nodes are fresh already
            return Ok(crate::compact::parse(text, base)?.graph);
        }
        let ShapesSyntax::Rdf(format) = syntax else {
            unreachable!()
        };
        let mut src = Source::from_bytes(text.as_bytes().to_vec(), format, None);
        src.base = base.map(str::to_string);
        src.name = "<shapes>".into();
        let (quads, _) = sparkles::io::parse_to_vec(&src).context("parsing shapes graph")?;
        let mut fresh: FxHashMap<oxrdf::BlankNode, oxrdf::BlankNode> = FxHashMap::default();
        let mut relabel = |b: oxrdf::BlankNode| fresh.entry(b).or_default().clone();
        let mut g = Graph::new();
        for q in quads {
            let s: oxrdf::NamedOrBlankNode = match q.subject {
                oxrdf::NamedOrBlankNode::BlankNode(b) => relabel(b).into(),
                s => s,
            };
            let o: Term = match q.object {
                Term::BlankNode(b) => relabel(b).into(),
                o => o,
            };
            g.insert(&Triple::new(s, q.predicate, o));
        }
        Ok(g)
    }

    /// Parse the shapes of an RDF graph.
    pub fn from_graph(graph: Graph) -> Result<Shapes> {
        Parser::new(&graph).parse(false)
    }

    /// Read the shapes graph from a graph of the store (`None` = the default graph; the
    /// special IRI `urn:x-arq:DefaultGraph` also names the default graph). Blank nodes
    /// keep their store identity, so shapes may refer to blank nodes of the data graph
    /// (e.g. when the shapes graph and the data graph are the same graph).
    pub fn from_store(snap: &sparkles::store::Snapshot, graph: Option<&str>) -> Result<Shapes> {
        let g = crate::data::read_graph(snap, graph)?;
        let mut shapes = Parser::new(&g).parse(true)?;
        shapes.source_graph = graph
            .filter(|g| *g != sparkles::sparql::ctx::DEFAULT_GRAPH_IRI)
            .map(str::to_string);
        Ok(shapes)
    }

    /// Read and merge several graphs of the store into one shapes graph (a graph that
    /// does not exist contributes nothing). `$shapesGraph` names the first.
    pub fn from_store_graphs(
        snap: &sparkles::store::Snapshot,
        graphs: &[String],
    ) -> Result<Shapes> {
        Shapes::from_store_graphs_with(snap, graphs, None)
    }

    /// [`Shapes::from_store_graphs`], merged with the triples of `extra` (shapes read
    /// from a file, see [`Shapes::read_graph`]).
    pub fn from_store_graphs_with(
        snap: &sparkles::store::Snapshot,
        graphs: &[String],
        extra: Option<&Graph>,
    ) -> Result<Shapes> {
        let mut g = extra.cloned().unwrap_or_default();
        for iri in graphs {
            if let Ok(part) = crate::data::read_graph(snap, Some(iri)) {
                g.extend(part.iter());
            }
        }
        let mut shapes = Parser::new(&g).parse(true)?;
        shapes.source_graph = graphs.first().cloned();
        Ok(shapes)
    }

    /// All shapes (node and property shapes, including nested ones).
    pub fn shapes(&self) -> &[Shape] {
        &self.shapes
    }

    pub fn len(&self) -> usize {
        self.shapes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }

    /// The shape declared by a node of the shapes graph.
    pub fn shape(&self, node: &Term) -> Option<&Shape> {
        self.by_node.get(node).map(|&i| &self.shapes[i])
    }

    /// Shapes that have targets (the ones validation starts from).
    pub fn targeted(&self) -> impl Iterator<Item = &Shape> {
        self.shapes.iter().filter(|s| !s.targets.is_empty())
    }

    /// The nodes that can conform to shape `si`, found from its constraints: a node
    /// conforms only if it is an instance of its `sh:class`, one of its `sh:hasValue` or
    /// `sh:in` values, or the subject (object) of a predicate (inverse) path with
    /// `sh:minCount` 1 or more. [`Candidates::All`] when no constraint narrows them.
    pub fn candidates(&self, si: ShapeId) -> Candidates {
        self.candidates_at(si, 0)
    }

    fn candidates_at(&self, si: ShapeId, depth: usize) -> Candidates {
        let shape = &self.shapes[si];
        if shape.deactivated || depth > 16 {
            return Candidates::All;
        }
        // the value nodes of a property shape are not the node itself
        let own = shape.path.is_none();
        for c in &shape.constraints {
            let narrowed = match c {
                Constraint::Class(t) if own => Candidates::Instances(*t),
                Constraint::HasValue(t) if own => Candidates::Terms(vec![*t]),
                Constraint::In(ts) if own => Candidates::Terms(ts.clone()),
                Constraint::MinCount(n) if *n > 0 => match &shape.path {
                    Some(PropertyPath::Predicate(p)) => Candidates::SubjectsOf(p.clone()),
                    Some(PropertyPath::Inverse(x)) => match x.as_ref() {
                        PropertyPath::Predicate(p) => Candidates::ObjectsOf(p.clone()),
                        _ => continue,
                    },
                    _ => continue,
                },
                Constraint::Property(s) | Constraint::Node(s) if own => {
                    self.candidates_at(*s, depth + 1)
                }
                Constraint::And(ss) if own => ss
                    .iter()
                    .map(|s| self.candidates_at(*s, depth + 1))
                    .find(|c| *c != Candidates::All)
                    .unwrap_or(Candidates::All),
                _ => continue,
            };
            if narrowed != Candidates::All {
                return narrowed;
            }
        }
        Candidates::All
    }

    /// A term of the term table.
    pub fn term(&self, t: Tid) -> &Term {
        &self.terms[t as usize]
    }

    pub(crate) fn intern(&mut self, t: Term) -> Tid {
        if let Some(&i) = self.tindex.get(&t) {
            return i;
        }
        let i = self.terms.len() as Tid;
        self.terms.push(t.clone());
        self.tindex.insert(t, i);
        i
    }
}

impl fmt::Display for Shapes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for s in &self.shapes {
            write!(f, "{} ", s.node)?;
            match &s.path {
                Some(p) => write!(f, "(property shape, path {p})")?,
                None => write!(f, "(node shape)")?,
            }
            if s.deactivated {
                write!(f, " deactivated")?;
            }
            writeln!(f)?;
            for t in &s.targets {
                let (k, v) = match t {
                    Target::Node(t) => ("targetNode", t),
                    Target::Class(t) => ("targetClass", t),
                    Target::SubjectsOf(t) => ("targetSubjectsOf", t),
                    Target::ObjectsOf(t) => ("targetObjectsOf", t),
                    Target::Where(w) => {
                        writeln!(f, "    targetWhere {}", self.shapes[*w].node)?;
                        continue;
                    }
                };
                writeln!(f, "    {k} {}", self.term(*v))?;
            }
            for c in &s.constraints {
                writeln!(f, "    {}", c.component())?;
            }
        }
        Ok(())
    }
}

// =============================================================================== parser

pub(crate) fn as_subject(t: &Term) -> Option<NamedOrBlankNodeRef<'_>> {
    match t {
        Term::NamedNode(n) => Some(n.as_ref().into()),
        Term::BlankNode(b) => Some(b.as_ref().into()),
        _ => None,
    }
}

/// Read helpers over an oxrdf graph.
pub(crate) struct G<'g> {
    pub g: &'g Graph,
}

impl<'g> G<'g> {
    pub fn objects(&self, s: &Term, p: NamedNodeRef<'_>) -> Vec<Term> {
        match as_subject(s) {
            Some(sub) => self
                .g
                .objects_for_subject_predicate(sub, p)
                .map(TermRef::into_owned)
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn object(&self, s: &Term, p: NamedNodeRef<'_>) -> Option<Term> {
        let sub = as_subject(s)?;
        self.g
            .object_for_subject_predicate(sub, p)
            .map(TermRef::into_owned)
    }

    pub fn subjects(&self, p: NamedNodeRef<'_>, o: &Term) -> Vec<Term> {
        self.g
            .subjects_for_predicate_object(p, o.as_ref())
            .map(|s| Term::from(s.into_owned()))
            .collect()
    }

    pub fn has(&self, s: &Term, p: NamedNodeRef<'_>) -> bool {
        self.object(s, p).is_some()
    }

    /// Elements of an RDF list.
    pub fn list(&self, head: &Term) -> Result<Vec<Term>> {
        let mut out = Vec::new();
        let mut cur = head.clone();
        let mut seen = FxHashSet::default();
        loop {
            if let Term::NamedNode(n) = &cur
                && n.as_ref() == rdf::NIL
            {
                return Ok(out);
            }
            if !seen.insert(cur.clone()) {
                bail!("cyclic RDF list at {head}");
            }
            let first = self
                .object(&cur, rdf::FIRST)
                .ok_or_else(|| anyhow!("malformed RDF list at {head}: missing rdf:first"))?;
            out.push(first);
            cur = self
                .object(&cur, rdf::REST)
                .ok_or_else(|| anyhow!("malformed RDF list at {head}: missing rdf:rest"))?;
        }
    }

    /// Is `t` a SHACL instance of `class` (rdf:type/rdfs:subClassOf*) in this graph?
    pub fn is_instance(&self, t: &Term, class: NamedNodeRef<'_>) -> bool {
        let class = Term::from(class.into_owned());
        let mut stack = self.objects(t, rdf::TYPE);
        let mut seen = FxHashSet::default();
        while let Some(c) = stack.pop() {
            if c == class {
                return true;
            }
            if seen.insert(c.clone()) {
                stack.extend(self.objects(&c, rdfs::SUB_CLASS_OF));
            }
        }
        false
    }
}

fn literal_bool(t: &Term) -> Option<bool> {
    match t {
        // only the literal `true` activates boolean parameters (W3C test uniqueLang-002:
        // "1"^^xsd:boolean does not)
        Term::Literal(l) if l.datatype() == xsd::BOOLEAN => match l.value() {
            "true" => Some(true),
            _ => Some(false),
        },
        _ => None,
    }
}

fn literal_u64(t: &Term, what: &str) -> Result<u64> {
    if let Term::Literal(l) = t
        && let Ok(v) = l.value().trim().parse::<i64>()
    {
        return Ok(v.max(0) as u64);
    }
    bail!("{what} expects an integer literal, found {t}")
}

fn literal_str(t: &Term) -> Option<&str> {
    match t {
        Term::Literal(l) => Some(l.value()),
        _ => None,
    }
}

/// Parameters that make a node a shape (SHACL §2.1) when it is the subject.
const SHAPE_PARAMS: &[NamedNodeRef<'static>] = &[
    sh::CLASS,
    sh::DATATYPE,
    sh::NODE_KIND,
    sh::MIN_COUNT,
    sh::MAX_COUNT,
    sh::MIN_EXCLUSIVE,
    sh::MIN_INCLUSIVE,
    sh::MAX_EXCLUSIVE,
    sh::MAX_INCLUSIVE,
    sh::MIN_LENGTH,
    sh::MAX_LENGTH,
    sh::PATTERN,
    sh::LANGUAGE_IN,
    sh::UNIQUE_LANG,
    sh::EQUALS,
    sh::DISJOINT,
    sh::LESS_THAN,
    sh::LESS_THAN_OR_EQUALS,
    sh::NOT,
    sh::AND,
    sh::OR,
    sh::XONE,
    sh::NODE,
    sh::PROPERTY,
    sh::QUALIFIED_VALUE_SHAPE,
    sh::CLOSED,
    sh::HAS_VALUE,
    sh::IN,
    sh::SPARQL,
    sh::PATH,
    sh::MEMBER_SHAPE,
    sh::MIN_LIST_LENGTH,
    sh::MAX_LIST_LENGTH,
    sh::UNIQUE_MEMBERS,
];

struct Parser<'g> {
    g: G<'g>,
    out: Shapes,
    components: Vec<std::sync::Arc<SparqlComponent>>,
}

impl<'g> Parser<'g> {
    fn new(graph: &'g Graph) -> Parser<'g> {
        Parser {
            g: G { g: graph },
            out: Shapes::default(),
            components: Vec::new(),
        }
    }

    fn parse(mut self, bnodes_in_store: bool) -> Result<Shapes> {
        self.out.bnodes_in_store = bnodes_in_store;
        self.components = crate::sparql::parse_components(&self.g)?;
        // candidate shapes, in a deterministic order
        let mut roots: Vec<Term> = Vec::new();
        let mut seen = FxHashSet::default();
        let mut add = |t: Term, roots: &mut Vec<Term>| {
            if as_subject(&t).is_some() && seen.insert(t.clone()) {
                roots.push(t);
            }
        };
        for class in [sh::NODE_SHAPE, sh::PROPERTY_SHAPE] {
            for s in self.g.subjects(rdf::TYPE, &class.into_owned().into()) {
                add(s, &mut roots);
            }
        }
        for p in [
            sh::TARGET_NODE,
            sh::TARGET_CLASS,
            sh::TARGET_SUBJECTS_OF,
            sh::TARGET_OBJECTS_OF,
            sh::TARGET_WHERE,
        ] {
            for t in self.g.g.triples_for_predicate(p) {
                add(Term::from(t.subject.into_owned()), &mut roots);
            }
        }
        // nodes declared as shapes through subclasses of sh:Shape classes
        for t in self.g.g.triples_for_predicate(rdf::TYPE) {
            let s = Term::from(t.subject.into_owned());
            if self.g.is_instance(&s, sh::NODE_SHAPE) || self.g.is_instance(&s, sh::PROPERTY_SHAPE)
            {
                add(s, &mut roots);
            }
        }
        for p in SHAPE_PARAMS {
            for t in self.g.g.triples_for_predicate(*p) {
                add(Term::from(t.subject.into_owned()), &mut roots);
            }
        }
        for r in roots {
            self.shape(&r)?;
        }
        Ok(self.out)
    }

    fn shape(&mut self, node: &Term) -> Result<ShapeId> {
        if let Some(&i) = self.out.by_node.get(node) {
            return Ok(i);
        }
        if as_subject(node).is_none() {
            bail!("a literal cannot be a shape: {node}");
        }
        let id = self.out.shapes.len();
        let g = &self.g;
        let path = match g.object(node, sh::PATH) {
            Some(p) => Some(
                PropertyPath::from_graph(g, &p)
                    .with_context(|| format!("sh:path of shape {node}"))?,
            ),
            None => None,
        };
        let deactivated = g
            .objects(node, sh::DEACTIVATED)
            .iter()
            .any(|t| literal_bool(t) == Some(true));
        let severity = match g.object(node, sh::SEVERITY) {
            Some(Term::NamedNode(n)) => n,
            _ => sh::VIOLATION.into_owned(),
        };
        let messages = g
            .objects(node, sh::MESSAGE)
            .into_iter()
            .filter_map(|t| match t {
                Term::Literal(l) => Some(l),
                _ => None,
            })
            .collect();
        self.out.shapes.push(Shape {
            node: node.clone(),
            path,
            targets: Vec::new(),
            deactivated,
            severity,
            messages,
            constraints: Vec::new(),
        });
        self.out.by_node.insert(node.clone(), id);

        let targets = self.targets(node)?;
        self.out.shapes[id].targets = targets;
        let constraints = self
            .constraints(node, id)
            .with_context(|| format!("shape {node}"))?;
        self.out.shapes[id].constraints = constraints;
        Ok(id)
    }

    fn targets(&mut self, node: &Term) -> Result<Vec<Target>> {
        let mut out = Vec::new();
        for t in self.g.objects(node, sh::TARGET_NODE) {
            out.push(Target::Node(self.out.intern(t)));
        }
        for t in self.g.objects(node, sh::TARGET_CLASS) {
            out.push(Target::Class(self.out.intern(t)));
        }
        // implicit class target: the shape is also an rdfs:Class
        if matches!(node, Term::NamedNode(_))
            && self.g.is_instance(node, rdfs::CLASS)
            && (self.g.is_instance(node, sh::NODE_SHAPE)
                || self.g.is_instance(node, sh::PROPERTY_SHAPE))
        {
            let t = self.out.intern(node.clone());
            if !out.contains(&Target::Class(t)) {
                out.push(Target::Class(t));
            }
        }
        for t in self.g.objects(node, sh::TARGET_SUBJECTS_OF) {
            out.push(Target::SubjectsOf(self.out.intern(t)));
        }
        for t in self.g.objects(node, sh::TARGET_OBJECTS_OF) {
            out.push(Target::ObjectsOf(self.out.intern(t)));
        }
        for t in self.g.objects(node, sh::TARGET_WHERE) {
            let w = self
                .shape(&t)
                .with_context(|| format!("sh:targetWhere of shape {node}"))?;
            out.push(Target::Where(w));
        }
        Ok(out)
    }

    fn shape_list(&mut self, head: &Term) -> Result<Vec<ShapeId>> {
        let items = self.g.list(head)?;
        items.iter().map(|t| self.shape(t)).collect()
    }

    fn predicate(&mut self, t: Term, what: &str) -> Result<Tid> {
        match t {
            Term::NamedNode(_) => Ok(self.out.intern(t)),
            _ => bail!("{what} expects an IRI, found {t}"),
        }
    }

    fn constraints(&mut self, node: &Term, id: ShapeId) -> Result<Vec<Constraint>> {
        let mut cs = Vec::new();
        let g = G { g: self.g.g };
        for t in g.objects(node, sh::CLASS) {
            cs.push(Constraint::Class(self.out.intern(t)));
        }
        for t in g.objects(node, sh::DATATYPE) {
            match t {
                Term::NamedNode(n) => cs.push(Constraint::Datatype(n)),
                t => bail!("sh:datatype expects an IRI, found {t}"),
            }
        }
        for t in g.objects(node, sh::NODE_KIND) {
            let k = match &t {
                Term::NamedNode(n) => NodeKind::from_iri(n.as_str()),
                _ => None,
            };
            cs.push(Constraint::NodeKind(
                k.ok_or_else(|| anyhow!("invalid sh:nodeKind {t}"))?,
            ));
        }
        for t in g.objects(node, sh::MIN_COUNT) {
            cs.push(Constraint::MinCount(literal_u64(&t, "sh:minCount")?));
        }
        for t in g.objects(node, sh::MAX_COUNT) {
            cs.push(Constraint::MaxCount(literal_u64(&t, "sh:maxCount")?));
        }
        type Mk = fn(Term, Value) -> Constraint;
        let ranges: [(NamedNodeRef<'_>, Mk); 4] = [
            (sh::MIN_EXCLUSIVE, Constraint::MinExclusive),
            (sh::MIN_INCLUSIVE, Constraint::MinInclusive),
            (sh::MAX_EXCLUSIVE, Constraint::MaxExclusive),
            (sh::MAX_INCLUSIVE, Constraint::MaxInclusive),
        ];
        for (p, mk) in ranges {
            for t in g.objects(node, p) {
                let v = Value::from_term(&t);
                cs.push(mk(t, v));
            }
        }
        for t in g.objects(node, sh::MIN_LENGTH) {
            cs.push(Constraint::MinLength(literal_u64(&t, "sh:minLength")?));
        }
        for t in g.objects(node, sh::MAX_LENGTH) {
            cs.push(Constraint::MaxLength(literal_u64(&t, "sh:maxLength")?));
        }
        let flags = g
            .object(node, sh::FLAGS)
            .and_then(|f| literal_str(&f).map(str::to_string));
        for t in g.objects(node, sh::PATTERN) {
            let pattern = literal_str(&t)
                .ok_or_else(|| anyhow!("sh:pattern expects a literal, found {t}"))?
                .to_string();
            let regex = compile_regex(&pattern, flags.as_deref())?;
            cs.push(Constraint::Pattern(Box::new(Pattern {
                pattern,
                flags: flags.clone(),
                regex,
            })));
        }
        for t in g.objects(node, sh::LANGUAGE_IN) {
            let langs = g
                .list(&t)?
                .iter()
                .filter_map(|l| literal_str(l).map(str::to_string))
                .collect();
            cs.push(Constraint::LanguageIn(langs));
        }
        if g.objects(node, sh::UNIQUE_LANG)
            .iter()
            .any(|t| literal_bool(t) == Some(true))
        {
            cs.push(Constraint::UniqueLang);
        }
        for t in g.objects(node, sh::EQUALS) {
            cs.push(Constraint::Equals(self.predicate(t, "sh:equals")?));
        }
        for t in g.objects(node, sh::DISJOINT) {
            cs.push(Constraint::Disjoint(self.predicate(t, "sh:disjoint")?));
        }
        for t in g.objects(node, sh::LESS_THAN) {
            cs.push(Constraint::LessThan(self.predicate(t, "sh:lessThan")?));
        }
        for t in g.objects(node, sh::LESS_THAN_OR_EQUALS) {
            cs.push(Constraint::LessThanOrEquals(
                self.predicate(t, "sh:lessThanOrEquals")?,
            ));
        }
        for t in g.objects(node, sh::NOT) {
            cs.push(Constraint::Not(self.shape(&t)?));
        }
        for t in g.objects(node, sh::AND) {
            cs.push(Constraint::And(self.shape_list(&t)?));
        }
        for t in g.objects(node, sh::OR) {
            cs.push(Constraint::Or(self.shape_list(&t)?));
        }
        for t in g.objects(node, sh::XONE) {
            cs.push(Constraint::Xone(self.shape_list(&t)?));
        }
        for t in g.objects(node, sh::NODE) {
            cs.push(Constraint::Node(self.shape(&t)?));
        }
        let mut property_shapes = Vec::new();
        for t in g.objects(node, sh::PROPERTY) {
            let ps = self.shape(&t)?;
            property_shapes.push(ps);
            cs.push(Constraint::Property(ps));
        }
        // qualified value shapes
        let qmin = g.object(node, sh::QUALIFIED_MIN_COUNT);
        let qmax = g.object(node, sh::QUALIFIED_MAX_COUNT);
        if let Some(q) = g.object(node, sh::QUALIFIED_VALUE_SHAPE)
            && (qmin.is_some() || qmax.is_some())
        {
            let shape = self.shape(&q)?;
            let disjoint = g
                .objects(node, sh::QUALIFIED_VALUE_SHAPES_DISJOINT)
                .iter()
                .any(|t| literal_bool(t) == Some(true));
            let mut siblings = Vec::new();
            if disjoint {
                for parent in g.subjects(sh::PROPERTY, node) {
                    for sib_ps in g.objects(&parent, sh::PROPERTY) {
                        if &sib_ps == node {
                            continue;
                        }
                        for sq in g.objects(&sib_ps, sh::QUALIFIED_VALUE_SHAPE) {
                            let s = self.shape(&sq)?;
                            if !siblings.contains(&s) {
                                siblings.push(s);
                            }
                        }
                    }
                }
            }
            let min = qmin
                .map(|t| literal_u64(&t, "sh:qualifiedMinCount"))
                .transpose()?;
            let max = qmax
                .map(|t| literal_u64(&t, "sh:qualifiedMaxCount"))
                .transpose()?;
            let q = Qualified {
                shape,
                min,
                max,
                disjoint,
                siblings,
            };
            if min.is_some() {
                cs.push(Constraint::QualifiedMin(Box::new(q.clone())));
            }
            if max.is_some() {
                cs.push(Constraint::QualifiedMax(Box::new(q)));
            }
        }
        if g.objects(node, sh::CLOSED)
            .iter()
            .any(|t| literal_bool(t) == Some(true))
        {
            let mut ignored = Vec::new();
            for l in g.objects(node, sh::IGNORED_PROPERTIES) {
                ignored.extend(g.list(&l)?);
            }
            let mut allowed = Vec::new();
            for &ps in &property_shapes {
                if let Some(PropertyPath::Predicate(p)) = &self.out.shapes[ps].path {
                    allowed.push(self.out.intern(Term::NamedNode(p.clone())));
                } else if self.out.shapes[ps].path.is_none() {
                    // property shape still being parsed (recursion): read its path directly
                    let pnode = self.out.shapes[ps].node.clone();
                    if let Some(Term::NamedNode(p)) = g.object(&pnode, sh::PATH) {
                        allowed.push(self.out.intern(Term::NamedNode(p)));
                    }
                }
            }
            for t in &ignored {
                allowed.push(self.out.intern(t.clone()));
            }
            cs.push(Constraint::Closed { allowed, ignored });
        }
        for t in g.objects(node, sh::HAS_VALUE) {
            cs.push(Constraint::HasValue(self.out.intern(t)));
        }
        for t in g.objects(node, sh::IN) {
            let items = g.list(&t)?;
            let ids = items.into_iter().map(|i| self.out.intern(i)).collect();
            cs.push(Constraint::In(ids));
        }
        for t in g.objects(node, sh::MEMBER_SHAPE) {
            cs.push(Constraint::MemberShape(self.shape(&t)?));
        }
        for t in g.objects(node, sh::MIN_LIST_LENGTH) {
            cs.push(Constraint::MinListLength(literal_u64(
                &t,
                "sh:minListLength",
            )?));
        }
        for t in g.objects(node, sh::MAX_LIST_LENGTH) {
            cs.push(Constraint::MaxListLength(literal_u64(
                &t,
                "sh:maxListLength",
            )?));
        }
        for t in g.objects(node, sh::UNIQUE_MEMBERS) {
            let b = literal_bool(&t)
                .ok_or_else(|| anyhow!("sh:uniqueMembers expects a boolean literal, found {t}"))?;
            cs.push(Constraint::UniqueMembers(b));
        }
        for t in g.objects(node, sh::SPARQL) {
            let path = self.out.shapes[id].path.clone();
            if let Some(c) = crate::sparql::parse_sparql_constraint(&g, &t, path.as_ref())? {
                cs.push(Constraint::Sparql(Box::new(c)));
            }
        }
        let path = self.out.shapes[id].path.clone();
        for comp in self.components.clone() {
            for inst in comp.instances(&g, node, path.as_ref())? {
                cs.push(Constraint::Component(Box::new(inst)));
            }
        }
        Ok(cs)
    }
}

/// Compile an XPath-style regular expression with `sh:flags` / SPARQL REGEX flags.
pub(crate) fn compile_regex(pattern: &str, flags: Option<&str>) -> Result<regex::Regex> {
    let mut b = regex::RegexBuilder::new(pattern);
    let mut literal = false;
    for f in flags.unwrap_or("").chars() {
        match f {
            'i' => {
                b.case_insensitive(true);
            }
            'm' => {
                b.multi_line(true);
            }
            's' => {
                b.dot_matches_new_line(true);
            }
            'x' => {
                b.ignore_whitespace(true);
            }
            'q' => literal = true,
            _ => bail!("unsupported regex flag '{f}'"),
        }
    }
    if literal {
        let escaped = regex::escape(pattern);
        let mut b2 = regex::RegexBuilder::new(&escaped);
        b2.case_insensitive(flags.unwrap_or("").contains('i'));
        return Ok(b2.build()?);
    }
    b.build()
        .with_context(|| format!("invalid sh:pattern {pattern:?}"))
}
