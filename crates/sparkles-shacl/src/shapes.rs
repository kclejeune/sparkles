//! The shapes model and its parser (SHACL §2–4, Jena `org.apache.jena.shacl.parser`).
//!
//! A [`Shapes`] value is independent of any store: constants used during validation are
//! kept in a small term table ([`Tid`] indices) that [`crate::validate`] resolves to
//! store ids once per validation run.

use crate::nodeexpr::{CustomFn, Expr, ExprId, FnIndex};
use crate::path::PropertyPath;
use crate::sparql::{ComponentConstraint, SparqlComponent, SparqlConstraint};
use crate::syntax::ShapesSyntax;
use crate::vocab::{rdf, rdfs, sh};
use anyhow::{Context as _, Result, anyhow, bail};
use oxrdf::vocab::xsd;
use oxrdf::{Graph, Literal, NamedNode, NamedNodeRef, NamedOrBlankNodeRef, Term, TermRef, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_core::io::Source;
use sparkles_core::sparql::value::Value;
use std::fmt;
use std::time::Instant;

/// The guard may repair invalid syntax, but cannot validate an unreadable schema.
pub(crate) enum GraphShapesError {
    Read(anyhow::Error),
    Parse(anyhow::Error),
}

impl GraphShapesError {
    fn into_error(self) -> anyhow::Error {
        match self {
            Self::Read(e) | Self::Parse(e) => e,
        }
    }
}

/// Index into the term table of a [`Shapes`].
pub type Tid = u32;
/// Index of a shape in [`Shapes::shapes`].
pub type ShapeId = usize;

/// Target declarations (SHACL §2.1.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// `sh:targetNode` with an IRI, a literal or a triple term
    Node(Tid),
    /// `sh:targetNode` with a node expression (SHACL 1.2 Core), evaluated with the shape
    /// as the focus node
    Expr(ExprId),
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
    /// `sh:expression` (SHACL 1.2 Node Expressions): for each value node, the expression
    /// must evaluate to `true`
    Expression(ExprId),
    /// `sh:nodeByExpression` (SHACL 1.2 Node Expressions): each value node conforms to
    /// each shape the expression computes
    NodeByExpression(ExprId),
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
            Constraint::Expression(_) => sh::EXPRESSION_CC,
            Constraint::NodeByExpression(_) => sh::NODE_BY_EXPRESSION_CC,
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
    /// `sh:deactivated` with a node expression that is not a constant: the shape is
    /// deactivated at the focus nodes where it evaluates to `true`
    pub deactivated_expr: Option<ExprId>,
    /// `sh:values` (property shapes): nodes added to the value nodes
    pub values: Option<ExprId>,
    /// `sh:defaultValue` (property shapes): the value nodes when there are none
    pub default_value: Option<ExprId>,
    pub severity: NamedNode,
    pub messages: Vec<Literal>,
    pub constraints: Vec<Constraint>,
    /// the shape's non-validating characteristics, when it has any
    pub info: Option<Box<ShapeInfo>>,
}

impl Shape {
    pub fn is_property_shape(&self) -> bool {
        self.path.is_some()
    }

    /// The shape's non-validating characteristics (empty when it has none).
    pub fn info(&self) -> &ShapeInfo {
        static NONE: std::sync::LazyLock<ShapeInfo> = std::sync::LazyLock::new(Default::default);
        self.info.as_deref().unwrap_or(&NONE)
    }
}

/// What a shape says about how its values are shown and edited, which validation
/// ignores: the non-validating property shape characteristics of SHACL §2.3.2 and the
/// DASH hints `dash:editor`, `dash:viewer`, `dash:singleLine` and `dash:readOnly`.
/// Forms read them (spec U02).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapeInfo {
    /// `sh:name`, every language
    pub names: Vec<Literal>,
    /// `sh:description`, every language
    pub descriptions: Vec<Literal>,
    /// `sh:order`
    pub order: Option<f64>,
    /// `sh:group`, a `sh:PropertyGroup`
    pub group: Option<Term>,
    /// `sh:defaultValue` when it is a constant (an IRI or a literal)
    pub default_value: Option<Term>,
    /// `dash:editor`
    pub editor: Option<NamedNode>,
    /// `dash:viewer`
    pub viewer: Option<NamedNode>,
    /// `dash:singleLine`
    pub single_line: Option<bool>,
    /// `dash:readOnly true`
    pub read_only: bool,
}

impl ShapeInfo {
    fn is_empty(&self) -> bool {
        *self == ShapeInfo::default()
    }
}

/// A `sh:PropertyGroup` that property shapes name with `sh:group`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroupInfo {
    /// `rdfs:label`, every language
    pub labels: Vec<Literal>,
    /// `sh:order`
    pub order: Option<f64>,
}

/// A parsed shapes graph.
#[derive(Clone, Debug, Default)]
pub struct Shapes {
    pub(crate) terms: Vec<Term>,
    tindex: FxHashMap<Term, Tid>,
    pub(crate) shapes: Vec<Shape>,
    pub(crate) by_node: FxHashMap<Term, ShapeId>,
    /// blank nodes of this shapes graph are store blank nodes (read via
    /// [`Shapes::from_store`]) and may be matched against data nodes
    pub(crate) bnodes_in_store: bool,
    /// The shapes themselves are a stored graph, independently of constant identity.
    pub(crate) binds_shapes_graph: bool,
    /// the store graph the shapes were read from (`None` = default graph), for
    /// `$shapesGraph` in SHACL-SPARQL
    pub(crate) source_graph: Option<String>,
    /// the node expressions of the shapes graph (see [`crate::nodeexpr`])
    pub(crate) exprs: Vec<Expr>,
    /// the node of the shapes graph each expression was read from
    pub(crate) expr_nodes: Vec<Term>,
    /// the custom node expression functions
    pub(crate) functions: Vec<CustomFn>,
    /// the property groups the shapes name with `sh:group`
    pub(crate) groups: FxHashMap<Term, GroupInfo>,
    /// the SHACL-AF rules of the shapes graph (see [`crate::rules`])
    pub(crate) rules: Vec<crate::rules::Rule>,
    /// why the rules could not be read, which only a caller of [`Shapes::rules`] sees
    pub(crate) rule_error: Option<String>,
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

    /// Compile inline shapes for candidate validation, with the gate's strict target
    /// syntax and SPARQL restricted to the selected validation data.
    pub fn parse_for_gate(
        text: &str,
        syntax: impl Into<ShapesSyntax>,
        base: Option<&str>,
        deadline: Option<Instant>,
    ) -> Result<Shapes> {
        let doc = crate::syntax::read_document_until(text, syntax.into(), base, deadline)?;
        Parser::new(&doc.graph).for_gate(deadline).parse(false)
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
        let (quads, _) = sparkles_core::io::parse_to_vec(&src).context("parsing shapes graph")?;
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

    /// Parse an inline RDF draft whose blank-node constants retain stored identities.
    ///
    /// Existing store labels resolve in the validation snapshot; unknown labels remain
    /// local. Unlike `from_store`, this graph is not already in the snapshot, so it does
    /// not bind `$shapesGraph`. Callers must keep the source dataset and revision when
    /// carrying labels from a stored graph into a draft.
    pub fn from_graph_with_stored_blank_nodes(graph: Graph) -> Result<Shapes> {
        let mut shapes = Self::from_graph(graph)?;
        shapes.bnodes_in_store = true;
        Ok(shapes)
    }

    /// Parse the shapes of an RDF graph, and the node expression `expr` of it, for
    /// [`crate::eval_expression`].
    pub fn from_graph_with_expression(
        graph: Graph,
        bnodes_in_store: bool,
        expr: &Term,
    ) -> Result<(Shapes, ExprId)> {
        let (shapes, id) = Parser::new(&graph).parse_with(bnodes_in_store, Some(expr))?;
        Ok((shapes, id.expect("an expression was asked for")))
    }

    /// Read the shapes graph from a graph of the store (`None` = the default graph; the
    /// special IRI `urn:x-arq:DefaultGraph` also names the default graph). Blank nodes
    /// keep their store identity, so shapes may refer to blank nodes of the data graph
    /// (e.g. when the shapes graph and the data graph are the same graph).
    pub fn from_store(
        snap: &sparkles_core::store::Snapshot,
        graph: Option<&str>,
    ) -> Result<Shapes> {
        let g = crate::data::read_graph(snap, graph)?;
        let mut shapes = Parser::new(&g).parse(true)?;
        shapes.source_graph = graph
            .filter(|g| *g != sparkles_core::sparql::ctx::DEFAULT_GRAPH_IRI)
            .map(str::to_string);
        Ok(shapes)
    }

    /// Compile the pinned shapes graph for a gate. Stored blank nodes retain their
    /// identity, as in [`Shapes::from_store`]. An absent or empty graph returns
    /// `None`. The absolute deadline covers reading and compilation.
    pub fn from_store_for_gate(
        snap: &sparkles_core::store::Snapshot,
        graph: &str,
        deadline: Option<Instant>,
    ) -> Result<Option<Shapes>> {
        crate::data::check_deadline(deadline)?;
        let Some(id) = sparkles_core::validation::graph_id(snap, graph) else {
            return Ok(None);
        };
        let g = crate::data::read_graph_by_id(snap, id, deadline)?;
        if g.is_empty() {
            return Ok(None);
        }
        let mut shapes = Parser::new(&g).for_gate(deadline).parse(true)?;
        if graph != sparkles_core::sparql::ctx::DEFAULT_GRAPH_IRI {
            shapes.source_graph = Some(graph.to_string());
        }
        Ok(Some(shapes))
    }

    /// Read and merge several graphs of the store into one shapes graph (a graph that
    /// does not exist contributes nothing). `$shapesGraph` names the first.
    pub fn from_store_graphs(
        snap: &sparkles_core::store::Snapshot,
        graphs: &[String],
    ) -> Result<Shapes> {
        Shapes::from_store_graphs_with(snap, graphs, None)
    }

    /// [`Shapes::from_store_graphs`], merged with the triples of `extra` (shapes read
    /// from a file, see [`Shapes::read_graph`]).
    pub fn from_store_graphs_with(
        snap: &sparkles_core::store::Snapshot,
        graphs: &[String],
        extra: Option<&Graph>,
    ) -> Result<Shapes> {
        Self::from_store_graphs_classified(snap, graphs, extra)
            .map_err(GraphShapesError::into_error)
    }

    pub(crate) fn from_store_graphs_classified(
        snap: &sparkles_core::store::Snapshot,
        graphs: &[String],
        extra: Option<&Graph>,
    ) -> std::result::Result<Shapes, GraphShapesError> {
        let mut g = extra.cloned().unwrap_or_default();
        for iri in graphs {
            if sparkles_core::validation::graph_id(snap, iri).is_none() {
                continue;
            }
            let part = crate::data::read_graph(snap, Some(iri)).map_err(GraphShapesError::Read)?;
            g.extend(part.iter());
        }
        let mut shapes = Parser::new(&g)
            .parse(true)
            .map_err(GraphShapesError::Parse)?;
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

    /// The property group a shape names with `sh:group`.
    pub fn group(&self, node: &Term) -> Option<&GroupInfo> {
        self.groups.get(node)
    }

    /// The index of a shape in [`Shapes::shapes`].
    pub fn shape_id(&self, node: &Term) -> Option<ShapeId> {
        self.by_node.get(node).copied()
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
                    Target::Expr(e) => {
                        writeln!(f, "    targetNode {}", self.expr_nodes[*e])?;
                        continue;
                    }
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
#[derive(Clone, Copy)]
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

    pub fn objects_until(
        &self,
        s: &Term,
        p: NamedNodeRef<'_>,
        deadline: Option<Instant>,
    ) -> Result<Vec<Term>> {
        self.values_until(s, p, deadline).collect()
    }

    pub fn values_until<'a>(
        self,
        s: &'a Term,
        p: NamedNodeRef<'a>,
        deadline: Option<Instant>,
    ) -> impl Iterator<Item = Result<Term>> + 'a
    where
        'g: 'a,
    {
        as_subject(s)
            .into_iter()
            .flat_map(move |s| self.g.objects_for_subject_predicate(s, p))
            .map(move |value| {
                crate::data::check_deadline(deadline)?;
                Ok(value.into_owned())
            })
    }

    pub fn literals_until(
        &self,
        s: &Term,
        p: NamedNodeRef<'_>,
        deadline: Option<Instant>,
    ) -> Result<Vec<Literal>> {
        let mut values = Vec::new();
        for value in self.values_until(s, p, deadline) {
            if let Term::Literal(literal) = value? {
                values.push(literal);
            }
        }
        Ok(values)
    }

    pub fn is_true_until(
        &self,
        s: &Term,
        p: NamedNodeRef<'_>,
        deadline: Option<Instant>,
    ) -> Result<bool> {
        for value in self.values_until(s, p, deadline) {
            if literal_bool(&value?) == Some(true) {
                return Ok(true);
            }
        }
        Ok(false)
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

    fn subjects_until(
        &self,
        p: NamedNodeRef<'_>,
        o: &Term,
        deadline: Option<Instant>,
    ) -> Result<Vec<Term>> {
        self.g
            .subjects_for_predicate_object(p, o.as_ref())
            .map(|s| {
                crate::data::check_deadline(deadline)?;
                Ok(Term::from(s.into_owned()))
            })
            .collect()
    }

    pub fn has(&self, s: &Term, p: NamedNodeRef<'_>) -> bool {
        self.object(s, p).is_some()
    }

    /// Elements of an RDF list.
    pub fn list(&self, head: &Term, deadline: Option<Instant>) -> Result<Vec<Term>> {
        let mut out = Vec::new();
        let mut cur = head.clone();
        let mut seen = FxHashSet::default();
        loop {
            if let Term::NamedNode(n) = &cur
                && n.as_ref() == rdf::NIL
            {
                crate::data::check_deadline(deadline)?;
                return Ok(out);
            }
            #[cfg(test)]
            compile_control_tests::observe(compile_control_tests::Phase::ReadList);
            crate::data::check_deadline(deadline)?;
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
        self.is_instance_with(t, class, || Ok(()))
            .expect("an unchecked instance lookup cannot time out")
    }

    pub fn is_instance_until(
        &self,
        t: &Term,
        class: NamedNodeRef<'_>,
        deadline: Option<Instant>,
    ) -> Result<bool> {
        self.is_instance_with(t, class, || {
            crate::data::check_deadline(deadline)?;
            Ok(())
        })
    }

    fn is_instance_with(
        &self,
        t: &Term,
        class: NamedNodeRef<'_>,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<bool> {
        let class = Term::from(class.into_owned());
        let mut stack = Vec::new();
        let mut queue = |node: &Term, predicate, stack: &mut Vec<Term>| {
            if let Some(subject) = as_subject(node) {
                for value in self.g.objects_for_subject_predicate(subject, predicate) {
                    check()?;
                    stack.push(value.into_owned());
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        queue(t, rdf::TYPE, &mut stack)?;
        let mut seen = FxHashSet::default();
        while let Some(c) = stack.pop() {
            if c == class {
                return Ok(true);
            }
            if seen.insert(c.clone()) {
                queue(&c, rdfs::SUB_CLASS_OF, &mut stack)?;
            }
        }
        Ok(false)
    }
}

pub(crate) fn literal_bool(t: &Term) -> Option<bool> {
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
    sh::EXPRESSION,
    sh::NODE_BY_EXPRESSION,
    sh::VALUES,
    sh::DEFAULT_VALUE,
];

pub(crate) struct Parser<'g> {
    pub(crate) g: G<'g>,
    pub(crate) out: Shapes,
    pub(crate) gate: bool,
    pub(crate) deadline: Option<Instant>,
    components: Vec<std::sync::Arc<SparqlComponent>>,
    pub(crate) fns: FnIndex,
    pub(crate) expr_memo: FxHashMap<Term, ExprId>,
    /// the SHACL-AF expressions of rules, which read blank nodes differently
    pub(crate) af_memo: FxHashMap<Term, ExprId>,
    pub(crate) expr_stack: Vec<Term>,
}

impl<'g> Parser<'g> {
    pub(crate) fn new(graph: &'g Graph) -> Parser<'g> {
        Parser {
            g: G { g: graph },
            out: Shapes::default(),
            gate: false,
            deadline: None,
            components: Vec::new(),
            fns: FnIndex::default(),
            expr_memo: FxHashMap::default(),
            af_memo: FxHashMap::default(),
            expr_stack: Vec::new(),
        }
    }

    fn for_gate(mut self, deadline: Option<Instant>) -> Self {
        self.gate = true;
        self.deadline = deadline;
        self
    }

    pub(crate) fn check_deadline(&self) -> Result<()> {
        crate::data::check_deadline(self.deadline).map_err(Into::into)
    }

    pub(crate) fn parse(self, bnodes_in_store: bool) -> Result<Shapes> {
        Ok(self.parse_with(bnodes_in_store, None)?.0)
    }

    /// Parse the shapes, and the node expression `expr` besides them (for
    /// [`Shapes::from_graph_with_expression`]).
    pub(crate) fn parse_with(
        mut self,
        bnodes_in_store: bool,
        expr: Option<&Term>,
    ) -> Result<(Shapes, Option<ExprId>)> {
        self.out.bnodes_in_store = bnodes_in_store;
        self.out.binds_shapes_graph = bnodes_in_store;
        self.check_deadline()?;
        if self.gate {
            if let Some(t) = self.g.g.triples_for_predicate(sh::TARGET).next() {
                bail!(
                    "sh:target on {} is not supported by the gate's SHACL profile",
                    t.subject
                );
            }
            crate::sparql::check_gate_declarations(&self.g, self.deadline)?;
        }
        self.components = crate::sparql::parse_components(&self.g, self.deadline)?;
        self.check_deadline()?;
        self.parse_functions()?;
        self.check_deadline()?;
        // candidate shapes, in a deterministic order
        let mut roots: Vec<Term> = Vec::new();
        let mut seen = FxHashSet::default();
        let mut add = |t: Term, roots: &mut Vec<Term>| {
            if as_subject(&t).is_some() && seen.insert(t.clone()) {
                roots.push(t);
            }
        };
        for class in [sh::NODE_SHAPE, sh::PROPERTY_SHAPE] {
            for s in self.g.g.subjects_for_predicate_object(rdf::TYPE, class) {
                self.check_deadline()?;
                add(Term::from(s.into_owned()), &mut roots);
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
                self.check_deadline()?;
                add(Term::from(t.subject.into_owned()), &mut roots);
            }
        }
        // nodes declared as shapes through subclasses of sh:Shape classes
        for t in self.g.g.triples_for_predicate(rdf::TYPE) {
            self.check_deadline()?;
            let s = Term::from(t.subject.into_owned());
            if self
                .g
                .is_instance_until(&s, sh::NODE_SHAPE, self.deadline)?
                || self
                    .g
                    .is_instance_until(&s, sh::PROPERTY_SHAPE, self.deadline)?
            {
                add(s, &mut roots);
            }
        }
        for p in SHAPE_PARAMS {
            for t in self.g.g.triples_for_predicate(*p) {
                self.check_deadline()?;
                add(Term::from(t.subject.into_owned()), &mut roots);
            }
        }
        for r in roots {
            self.shape(&r)?;
        }
        if let Err(e) = self.parse_rules() {
            if matches!(
                e.downcast_ref::<sparkles_core::Error>(),
                Some(sparkles_core::Error::Timeout)
            ) {
                return Err(e);
            }
            self.out.rules.clear();
            self.out.rule_error = Some(format!("{e:#}"));
        }
        let expr = match expr {
            Some(node) => Some(
                self.expr(node)
                    .with_context(|| format!("node expression {node}"))?,
            ),
            None => None,
        };
        if self.gate {
            crate::sparql::check_gate_queries(&self.out, self.deadline)?;
        }
        self.check_deadline()?;
        Ok((self.out, expr))
    }

    pub(crate) fn shape(&mut self, node: &Term) -> Result<ShapeId> {
        self.check_deadline()?;
        if let Some(&i) = self.out.by_node.get(node) {
            return Ok(i);
        }
        // Leave room for constraint parsing and its nested property paths
        // before the next shape reaches this stack check.
        stacker::maybe_grow(1 << 20, 4 << 20, || self.parse_shape(node))
    }

    #[inline(never)]
    fn parse_shape(&mut self, node: &Term) -> Result<ShapeId> {
        if as_subject(node).is_none() {
            bail!("a literal cannot be a shape: {node}");
        }
        let id = self.out.shapes.len();
        let g = self.g;
        let path = match g.object(node, sh::PATH) {
            Some(p) => Some(
                PropertyPath::from_graph(&g, &p, self.deadline)
                    .with_context(|| format!("sh:path of shape {node}"))?,
            ),
            None => None,
        };
        let deactivated = g.is_true_until(node, sh::DEACTIVATED, self.deadline)?;
        let severity = match g.object(node, sh::SEVERITY) {
            Some(Term::NamedNode(n)) => n,
            _ => sh::VIOLATION.into_owned(),
        };
        let messages = g.literals_until(node, sh::MESSAGE, self.deadline)?;
        self.out.shapes.push(Shape {
            node: node.clone(),
            path,
            targets: Vec::new(),
            deactivated,
            deactivated_expr: None,
            values: None,
            default_value: None,
            severity,
            messages,
            constraints: Vec::new(),
            info: None,
        });
        self.out.by_node.insert(node.clone(), id);
        let info = self.info(node)?;
        if !info.is_empty() {
            self.out.shapes[id].info = Some(Box::new(info));
        }

        // node expressions of the shape, read once the shape has its place
        for t in g.values_until(node, sh::DEACTIVATED, self.deadline) {
            let t = t?;
            if self.is_function_expr(&t) {
                let e = self
                    .expr(&t)
                    .with_context(|| format!("sh:deactivated of shape {node}"))?;
                self.out.shapes[id].deactivated_expr = Some(e);
            }
        }
        if let Some(t) = self.g.object(node, sh::VALUES) {
            let e = self
                .expr(&t)
                .with_context(|| format!("sh:values of shape {node}"))?;
            self.out.shapes[id].values = Some(e);
        }
        if let Some(t) = self.g.object(node, sh::DEFAULT_VALUE) {
            let e = self
                .expr(&t)
                .with_context(|| format!("sh:defaultValue of shape {node}"))?;
            self.out.shapes[id].default_value = Some(e);
        }
        let targets = self.targets(node)?;
        self.out.shapes[id].targets = targets;
        let constraints = self
            .constraints(node, id)
            .with_context(|| format!("shape {node}"))?;
        self.out.shapes[id].constraints = constraints;
        Ok(id)
    }

    /// The non-validating characteristics of a shape, and the property group it names.
    fn info(&mut self, node: &Term) -> Result<ShapeInfo> {
        let g = &self.g;
        let number = |t: Option<Term>| match t {
            Some(Term::Literal(l)) => l
                .value()
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|f| f.is_finite()),
            _ => None,
        };
        let iri = |p| match g.object(node, p) {
            Some(Term::NamedNode(n)) => Some(n),
            _ => None,
        };
        let group = g
            .object(node, sh::GROUP)
            .filter(|t| as_subject(t).is_some());
        let info = ShapeInfo {
            names: g.literals_until(node, sh::NAME, self.deadline)?,
            descriptions: g.literals_until(node, sh::DESCRIPTION, self.deadline)?,
            order: number(g.object(node, sh::ORDER)),
            group: group.clone(),
            default_value: g
                .object(node, sh::DEFAULT_VALUE)
                .filter(|t| matches!(t, Term::NamedNode(_) | Term::Literal(_))),
            editor: iri(crate::vocab::dash::EDITOR),
            viewer: iri(crate::vocab::dash::VIEWER),
            single_line: g
                .object(node, crate::vocab::dash::SINGLE_LINE)
                .and_then(|t| literal_bool(&t)),
            read_only: g.is_true_until(node, crate::vocab::dash::READ_ONLY, self.deadline)?,
        };
        if let Some(gr) = group
            && !self.out.groups.contains_key(&gr)
        {
            let labels = g.literals_until(&gr, rdfs::LABEL, self.deadline)?;
            let order = number(g.object(&gr, sh::ORDER));
            self.out.groups.insert(gr, GroupInfo { labels, order });
        }
        Ok(info)
    }

    fn targets(&mut self, node: &Term) -> Result<Vec<Target>> {
        let mut out = Vec::new();
        let g = self.g;
        for t in g.values_until(node, sh::TARGET_NODE, self.deadline) {
            let t = t?;
            // a blank node that is a node expression (SHACL 1.2); other blank nodes are
            // plain nodes, as in SHACL 1.0
            if self.gate && self.has_expression_syntax(&t)? {
                let e = self
                    .expr(&t)
                    .with_context(|| format!("sh:targetNode of shape {node}"))?;
                out.push(Target::Expr(e));
                continue;
            }
            if !self.gate
                && self.is_function_expr(&t)
                && let Ok(e) = self.expr(&t)
            {
                out.push(Target::Expr(e));
                continue;
            }
            out.push(Target::Node(self.out.intern(t)));
        }
        for t in g.values_until(node, sh::TARGET_CLASS, self.deadline) {
            out.push(Target::Class(self.target_iri(
                node,
                sh::TARGET_CLASS,
                t?,
            )?));
        }
        // implicit class target: the shape is also an rdfs:Class
        if matches!(node, Term::NamedNode(_))
            && self.g.is_instance_until(node, rdfs::CLASS, self.deadline)?
            && (self
                .g
                .is_instance_until(node, sh::NODE_SHAPE, self.deadline)?
                || self
                    .g
                    .is_instance_until(node, sh::PROPERTY_SHAPE, self.deadline)?)
        {
            let t = self.out.intern(node.clone());
            if !out.contains(&Target::Class(t)) {
                out.push(Target::Class(t));
            }
        }
        for t in g.values_until(node, sh::TARGET_SUBJECTS_OF, self.deadline) {
            out.push(Target::SubjectsOf(self.target_iri(
                node,
                sh::TARGET_SUBJECTS_OF,
                t?,
            )?));
        }
        for t in g.values_until(node, sh::TARGET_OBJECTS_OF, self.deadline) {
            out.push(Target::ObjectsOf(self.target_iri(
                node,
                sh::TARGET_OBJECTS_OF,
                t?,
            )?));
        }
        for t in g.values_until(node, sh::TARGET_WHERE, self.deadline) {
            let w = self
                .shape(&t?)
                .with_context(|| format!("sh:targetWhere of shape {node}"))?;
            out.push(Target::Where(w));
        }
        Ok(out)
    }

    fn target_iri(&mut self, node: &Term, predicate: NamedNodeRef<'_>, term: Term) -> Result<Tid> {
        if self.gate && !matches!(term, Term::NamedNode(_)) {
            let name = predicate.as_str().rsplit('#').next().unwrap_or_default();
            bail!("sh:{name} of shape {node} requires an IRI, found {term}");
        }
        Ok(self.out.intern(term))
    }

    fn shape_list(&mut self, head: &Term) -> Result<Vec<ShapeId>> {
        let items = self.g.list(head, self.deadline)?;
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
        for t in g.values_until(node, sh::CLASS, self.deadline) {
            let t = t?;
            cs.push(Constraint::Class(self.out.intern(t)));
        }
        for t in g.values_until(node, sh::DATATYPE, self.deadline) {
            let t = t?;
            match t {
                Term::NamedNode(n) => cs.push(Constraint::Datatype(n)),
                t => bail!("sh:datatype expects an IRI, found {t}"),
            }
        }
        for t in g.values_until(node, sh::NODE_KIND, self.deadline) {
            let t = t?;
            let k = match &t {
                Term::NamedNode(n) => NodeKind::from_iri(n.as_str()),
                _ => None,
            };
            cs.push(Constraint::NodeKind(
                k.ok_or_else(|| anyhow!("invalid sh:nodeKind {t}"))?,
            ));
        }
        for t in g.values_until(node, sh::MIN_COUNT, self.deadline) {
            let t = t?;
            cs.push(Constraint::MinCount(literal_u64(&t, "sh:minCount")?));
        }
        for t in g.values_until(node, sh::MAX_COUNT, self.deadline) {
            let t = t?;
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
            for t in g.values_until(node, p, self.deadline) {
                let t = t?;
                let v = Value::from_term(&t);
                cs.push(mk(t, v));
            }
        }
        for t in g.values_until(node, sh::MIN_LENGTH, self.deadline) {
            let t = t?;
            cs.push(Constraint::MinLength(literal_u64(&t, "sh:minLength")?));
        }
        for t in g.values_until(node, sh::MAX_LENGTH, self.deadline) {
            let t = t?;
            cs.push(Constraint::MaxLength(literal_u64(&t, "sh:maxLength")?));
        }
        let flags = g
            .object(node, sh::FLAGS)
            .and_then(|f| literal_str(&f).map(str::to_string));
        for t in g.values_until(node, sh::PATTERN, self.deadline) {
            let t = t?;
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
        for t in g.values_until(node, sh::LANGUAGE_IN, self.deadline) {
            let t = t?;
            let mut langs = Vec::new();
            for term in g.list(&t, self.deadline)? {
                self.check_deadline()?;
                if let Some(lang) = literal_str(&term) {
                    langs.push(lang.to_owned());
                }
            }
            cs.push(Constraint::LanguageIn(langs));
        }
        if g.is_true_until(node, sh::UNIQUE_LANG, self.deadline)? {
            cs.push(Constraint::UniqueLang);
        }
        for t in g.values_until(node, sh::EQUALS, self.deadline) {
            let t = t?;
            cs.push(Constraint::Equals(self.predicate(t, "sh:equals")?));
        }
        for t in g.values_until(node, sh::DISJOINT, self.deadline) {
            let t = t?;
            cs.push(Constraint::Disjoint(self.predicate(t, "sh:disjoint")?));
        }
        for t in g.values_until(node, sh::LESS_THAN, self.deadline) {
            let t = t?;
            cs.push(Constraint::LessThan(self.predicate(t, "sh:lessThan")?));
        }
        for t in g.values_until(node, sh::LESS_THAN_OR_EQUALS, self.deadline) {
            let t = t?;
            cs.push(Constraint::LessThanOrEquals(
                self.predicate(t, "sh:lessThanOrEquals")?,
            ));
        }
        for t in g.values_until(node, sh::NOT, self.deadline) {
            let t = t?;
            cs.push(Constraint::Not(self.shape(&t)?));
        }
        for t in g.values_until(node, sh::AND, self.deadline) {
            let t = t?;
            cs.push(Constraint::And(self.shape_list(&t)?));
        }
        for t in g.values_until(node, sh::OR, self.deadline) {
            let t = t?;
            cs.push(Constraint::Or(self.shape_list(&t)?));
        }
        for t in g.values_until(node, sh::XONE, self.deadline) {
            let t = t?;
            cs.push(Constraint::Xone(self.shape_list(&t)?));
        }
        for t in g.values_until(node, sh::NODE, self.deadline) {
            let t = t?;
            cs.push(Constraint::Node(self.shape(&t)?));
        }
        let mut property_shapes = Vec::new();
        for t in g.values_until(node, sh::PROPERTY, self.deadline) {
            let t = t?;
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
            let disjoint =
                g.is_true_until(node, sh::QUALIFIED_VALUE_SHAPES_DISJOINT, self.deadline)?;
            let mut siblings = Vec::new();
            if disjoint {
                let mut seen = FxHashSet::default();
                for parent in g.subjects_until(sh::PROPERTY, node, self.deadline)? {
                    self.check_deadline()?;
                    for sib_ps in g.values_until(&parent, sh::PROPERTY, self.deadline) {
                        let sib_ps = sib_ps?;
                        if &sib_ps == node {
                            continue;
                        }
                        for sq in g.values_until(&sib_ps, sh::QUALIFIED_VALUE_SHAPE, self.deadline)
                        {
                            let s = self.shape(&sq?)?;
                            if seen.insert(s) {
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
        if g.is_true_until(node, sh::CLOSED, self.deadline)? {
            let mut ignored = Vec::new();
            for list in g.values_until(node, sh::IGNORED_PROPERTIES, self.deadline) {
                for term in g.list(&list?, self.deadline)? {
                    self.check_deadline()?;
                    ignored.push(term);
                }
            }
            let mut allowed = Vec::new();
            for &ps in &property_shapes {
                self.check_deadline()?;
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
                self.check_deadline()?;
                allowed.push(self.out.intern(t.clone()));
            }
            cs.push(Constraint::Closed { allowed, ignored });
        }
        for t in g.values_until(node, sh::HAS_VALUE, self.deadline) {
            let t = t?;
            cs.push(Constraint::HasValue(self.out.intern(t)));
        }
        for t in g.values_until(node, sh::IN, self.deadline) {
            let t = t?;
            let mut ids = Vec::new();
            for item in g.list(&t, self.deadline)? {
                #[cfg(test)]
                compile_control_tests::observe(compile_control_tests::Phase::InternMember);
                self.check_deadline()?;
                ids.push(self.out.intern(item));
            }
            cs.push(Constraint::In(ids));
        }
        for t in g.values_until(node, sh::MEMBER_SHAPE, self.deadline) {
            let t = t?;
            cs.push(Constraint::MemberShape(self.shape(&t)?));
        }
        for t in g.values_until(node, sh::MIN_LIST_LENGTH, self.deadline) {
            let t = t?;
            cs.push(Constraint::MinListLength(literal_u64(
                &t,
                "sh:minListLength",
            )?));
        }
        for t in g.values_until(node, sh::MAX_LIST_LENGTH, self.deadline) {
            let t = t?;
            cs.push(Constraint::MaxListLength(literal_u64(
                &t,
                "sh:maxListLength",
            )?));
        }
        for t in g.values_until(node, sh::UNIQUE_MEMBERS, self.deadline) {
            let t = t?;
            let b = literal_bool(&t)
                .ok_or_else(|| anyhow!("sh:uniqueMembers expects a boolean literal, found {t}"))?;
            cs.push(Constraint::UniqueMembers(b));
        }
        for t in g.values_until(node, sh::EXPRESSION, self.deadline) {
            let t = t?;
            let e = self
                .expr(&t)
                .with_context(|| format!("sh:expression of shape {node}"))?;
            cs.push(Constraint::Expression(e));
        }
        for t in g.values_until(node, sh::NODE_BY_EXPRESSION, self.deadline) {
            let t = t?;
            let e = self
                .expr(&t)
                .with_context(|| format!("sh:nodeByExpression of shape {node}"))?;
            cs.push(Constraint::NodeByExpression(e));
        }
        let path = self.out.shapes[id].path.as_ref();
        for t in g.values_until(node, sh::SPARQL, self.deadline) {
            let t = t?;
            self.check_deadline()?;
            if let Some(c) =
                crate::sparql::parse_sparql_constraint(&g, &t, path, self.gate, self.deadline)?
            {
                cs.push(Constraint::Sparql(Box::new(c)));
            }
        }
        for comp in &self.components {
            self.check_deadline()?;
            for inst in comp.instances(&g, node, path, self.deadline)? {
                #[cfg(test)]
                compile_control_tests::observe(compile_control_tests::Phase::AppendComponent);
                self.check_deadline()?;
                cs.push(Constraint::Component(Box::new(inst)));
            }
        }
        Ok(cs)
    }
}

/// Compile an XPath-style regular expression with `sh:flags` / SPARQL REGEX flags.
pub(crate) fn compile_regex(pattern: &str, flags: Option<&str>) -> Result<regex::Regex> {
    let flags = flags.unwrap_or("");
    for f in flags.chars() {
        if !matches!(f, 'i' | 'm' | 's' | 'x' | 'q') {
            bail!("unsupported regex flag '{f}'");
        }
    }
    let translated = sparkles_core::sparql::expr::translate_xpath_pattern(pattern, flags)
        .ok_or_else(|| anyhow::anyhow!("invalid sh:pattern {pattern:?}"))?;
    regex::RegexBuilder::new(&translated)
        .build()
        .with_context(|| format!("invalid sh:pattern {pattern:?}"))
}

#[cfg(test)]
mod compile_control_tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum Phase {
        ReadList,
        InternMember,
        AppendComponent,
    }

    thread_local! {
        static PHASE: Cell<Option<(Phase, usize)>> = const { Cell::new(None) };
    }

    pub(super) fn observe(phase: Phase) {
        PHASE.with(|state| {
            if let Some((active, seen)) = state.get()
                && active == phase
            {
                if seen == 4 {
                    crate::data::set_deadline_checks_left(Some(0));
                }
                state.set(Some((active, seen + 1)));
            }
        });
    }

    struct Observe;

    impl Drop for Observe {
        fn drop(&mut self) {
            PHASE.set(None);
            crate::data::set_deadline_checks_left(None);
        }
    }

    fn assert_expires_in(graph: &Graph, phase: Phase) {
        let _guard = Observe;
        PHASE.set(Some((phase, 0)));
        let error = Parser::new(graph).for_gate(None).parse(false).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<sparkles_core::Error>(),
            Some(sparkles_core::Error::Timeout)
        ));
        let (_, seen) = PHASE.get().unwrap();
        assert!((5..=6).contains(&seen), "compiler visited {seen} entries");
    }

    #[test]
    fn controlled_in_list_expires_inside_one_shape() {
        let list = (0..1024)
            .map(|i| (i % 23).to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let graph = Shapes::read_graph(
            &format!(
                "@prefix sh: <http://www.w3.org/ns/shacl#> .
                 <urn:S> sh:targetNode <urn:x> ; sh:in ({list}) ."
            ),
            ShapesSyntax::default(),
            None,
        )
        .unwrap();
        let compiled = Parser::new(&graph).for_gate(None).parse(false).unwrap();
        let Constraint::In(ids) = &compiled.shapes[0].constraints[0] else {
            panic!("expected the sh:in constraint");
        };
        let values: Vec<_> = ids
            .iter()
            .map(|&id| compiled.term(id).to_string())
            .collect();
        let expected: Vec<_> = (0..1024)
            .map(|i| Literal::from(i % 23).to_string())
            .collect();
        assert_eq!(values, expected);
        assert_expires_in(&graph, Phase::ReadList);
    }

    #[test]
    fn controlled_in_member_conversion_expires_inside_one_shape() {
        let list = (0..32).map(|i| i.to_string()).collect::<Vec<_>>().join(" ");
        let graph = Shapes::read_graph(
            &format!(
                "@prefix sh: <http://www.w3.org/ns/shacl#> .
                 <urn:S> sh:targetNode <urn:x> ; sh:in ({list}) ."
            ),
            ShapesSyntax::default(),
            None,
        )
        .unwrap();
        assert_expires_in(&graph, Phase::InternMember);
    }

    #[test]
    fn controlled_component_append_expires_during_boxing() {
        let values = (0..300)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let graph = Shapes::read_graph(
            &format!(
                "@prefix sh: <http://www.w3.org/ns/shacl#> .
                 <urn:C> a sh:ConstraintComponent ;
                    sh:parameter [ sh:path <urn:arg> ] ;
                    sh:nodeValidator [ sh:select 'SELECT $this WHERE {{}}' ] .
                 <urn:S> sh:targetNode <urn:x> ; <urn:arg> {values} ."
            ),
            ShapesSyntax::default(),
            None,
        )
        .unwrap();
        let compiled = Parser::new(&graph).for_gate(None).parse(false).unwrap();
        assert_eq!(compiled.shapes[0].constraints.len(), 300);
        assert_expires_in(&graph, Phase::AppendComponent);
    }
}

#[cfg(test)]
mod graph_read_tests {
    use super::*;
    use sparkles_core::id::Id;
    use sparkles_core::index::Perm;
    use sparkles_core::sparql::{QueryOptions, update::update};
    use sparkles_core::store::Store;

    #[test]
    fn inline_stored_constants_do_not_bind_a_stored_shapes_graph() {
        let shapes = Shapes::from_graph_with_stored_blank_nodes(Graph::new()).unwrap();
        assert!(shapes.bnodes_in_store);
        assert!(!shapes.binds_shapes_graph);
        let inline = Shapes::from_graph(Graph::new()).unwrap();
        assert!(!inline.bnodes_in_store);
        assert!(!inline.binds_shapes_graph);
        let store = Store::in_memory(Default::default());
        let stored = Shapes::from_store(&store.snapshot(), None).unwrap();
        assert!(stored.bnodes_in_store);
        assert!(stored.binds_shapes_graph);
    }

    #[test]
    fn a_shapes_graph_read_error_is_not_an_empty_schema() {
        let store = Store::in_memory(Default::default());
        update(&store, "INSERT DATA { GRAPH <urn:shapes> { <urn:Shape> a <http://www.w3.org/ns/shacl#NodeShape> } }", &QueryOptions::default()).unwrap();
        let mut snap = (*store.snapshot()).clone();
        let g = snap.lookup_iri("urn:shapes").unwrap();
        let p = snap.lookup_iri(rdf::TYPE.as_str()).unwrap();
        let o = snap.lookup_iri(sh::NODE_SHAPE.as_str()).unwrap();
        let bad = [Id::vocab(1 << 40), p, o, g];
        snap.delta.ins[Perm::Gspo.index()].insert(Perm::Gspo.to_key(&bad));
        let direct = crate::data::read_graph(&snap, Some("urn:shapes")).unwrap_err();
        assert!(
            direct.to_string().contains("cannot decode quad"),
            "{direct:#}"
        );
        let shapes = Shapes::from_store_graphs_with(&snap, &["urn:shapes".into()], None);
        assert!(
            shapes.is_err(),
            "a failed read must not compile a partial shapes graph"
        );
    }

    #[test]
    fn an_unknown_shapes_graph_contributes_no_shapes() {
        let store = Store::in_memory(Default::default());
        let shapes =
            Shapes::from_store_graphs_with(&store.snapshot(), &["urn:absent".into()], None)
                .unwrap();
        assert!(shapes.is_empty());
        update(&store, "INSERT DATA { GRAPH <urn:present> { <urn:graphShape> a <http://www.w3.org/ns/shacl#NodeShape> } }", &QueryOptions::default()).unwrap();
        let extra = Shapes::read_graph(
            "<urn:fileShape> a <http://www.w3.org/ns/shacl#NodeShape> .",
            ShapesSyntax::default(),
            None,
        )
        .unwrap();
        let merged = Shapes::from_store_graphs_with(
            &store.snapshot(),
            &["urn:absent".into(), "urn:present".into()],
            Some(&extra),
        )
        .unwrap();
        assert_eq!(
            merged.len(),
            2,
            "missing graphs do not discard valid graph or file sources"
        );
    }

    #[test]
    fn shacl_pattern_xpath_dot_flags_and_compiler_limits() {
        let re = compile_regex("^a.b$", None).unwrap();
        assert!(re.is_match("axb"));
        assert!(!re.is_match("a\rb"));
        assert!(!re.is_match("a\nb"));

        let re_s = compile_regex("^a.b$", Some("s")).unwrap();
        assert!(re_s.is_match("a\rb"));
        assert!(re_s.is_match("a\nb"));

        let re_x = compile_regex("^[ ]a.b$", Some("x")).unwrap();
        assert!(re_x.is_match(" axb"));
        assert!(!re_x.is_match(" a\rb"));

        let scoped_on = compile_regex("^(?s:.)$", None).unwrap();
        assert!(scoped_on.is_match("\r"));
        let scoped_off = compile_regex("^(?-s:.)$", Some("s")).unwrap();
        assert!(!scoped_off.is_match("\r"));

        // Preserves underlying regex compiler diagnostic.
        let err = compile_regex("[", None).unwrap_err();
        assert!(format!("{err:#}").contains("unclosed"), "{err:#}");

        // Preserves the regex crate's default 10 MiB NFA limit above expr::compile_regex's 4 MiB.
        let big = [150usize, 200, 250, 300, 400, 500]
            .into_iter()
            .map(|n| format!(r"\w{{{n}}}"))
            .find(|pat| sparkles_core::sparql::expr::compile_regex(pat, "").is_err())
            .expect("large repetition exceeds 4 MiB query helper limit");
        assert!(
            compile_regex(&big, None).is_ok(),
            "SHACL keeps the default 10 MiB regex compilation limit for {big}"
        );
    }
}
