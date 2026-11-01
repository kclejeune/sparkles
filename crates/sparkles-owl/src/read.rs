//! The reverse mapping: triples to axioms, as the OWL 2 Mapping to RDF Graphs §3
//! defines it.
//!
//! The reader follows the mapping's order. It reads the ontology headers, applies the
//! OWL 1 compatibility rewrites of Tables 5 and 6, collects the declarations of
//! Table 7, parses annotations (Table 10), then axioms with annotations (Table 17 and
//! the blank-node axioms), then the remaining axioms of Tables 16 and 18, and finally
//! the annotation assertions. Class expressions, property expressions and data ranges
//! (Tables 11–15) are parsed when an axiom reaches them. Every triple a mapped axiom
//! uses is consumed, and the triples left over are the unmapped triples.
//!
//! In [`Mode::Lenient`], an undeclared IRI is typed from its use where the use decides
//! it, a blank-node structure used more than once is parsed once and shared, and the
//! typing triples that OWL 2 requires on expression nodes may be missing. Each repair is
//! a warning that names the triples involved. In [`Mode::Strict`], none of these repairs
//! is made, so such triples stay unmapped, and each unmapped triple is an error.

use crate::model::*;
use crate::vocab as v;
use oxrdf::{BlankNode, NamedNode, NamedOrBlankNode, Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::{BTreeMap, BTreeSet};

/// How strictly the reverse mapping is applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Repair undeclared entities and shared structures, with a warning for each.
    #[default]
    Lenient,
    /// Map only what the mapping defines, as an OWL 2 DL parser does.
    Strict,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "lenient" => Some(Mode::Lenient),
            "strict" => Some(Mode::Strict),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Lenient => "lenient",
            Mode::Strict => "strict",
        }
    }
}

/// The options of a read.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadOptions {
    pub mode: Mode,
    /// The input holds only the terminological triples (see
    /// [`is_terminological`](crate::is_terminological)); assertions are mapped later, one
    /// triple at a time, with [`Ontology::map_assertion`].
    pub terminology_only: bool,
    /// The most diagnostics kept; later ones are counted only (0: 1,000).
    pub max_diagnostics: usize,
}

/// A note about the read: a repair made in lenient mode, or an error of a strict read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// what the note is about: `undeclared` (an entity typed from its use), `shared` (a
    /// structure used more than once), `untyped` (an expression node without its
    /// `rdf:type`), `header` (several ontology nodes), `missing-main` (an `owl:Axiom`
    /// node whose triple is absent) or `unmapped` (a triple a strict read cannot map)
    pub code: &'static str,
    pub message: String,
    /// indices of the triples involved, into [`Ontology::triples`]
    pub triples: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// Where an axiom was read from: its graph and the triples that encode it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub graph: u32,
    /// indices into [`Ontology::triples`]; empty for a declaration that the mapping
    /// implies (Table 6) and for the built-in declarations
    pub triples: Vec<usize>,
    /// the axiom's main triple, among `triples`, for an axiom that has one (Tables 16
    /// to 18): the triple that belongs to the axiom even when its subject is an
    /// expression another axiom shares
    pub main: Option<usize>,
}

/// The deepest nesting of class expressions and data ranges a read follows.
pub const MAX_NESTING: usize = 128;

/// The entity kinds an IRI is declared with or was typed with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Kinds(u8);

impl Kinds {
    fn bit(k: EntityKind) -> u8 {
        1 << (k as u8)
    }
    pub fn has(self, k: EntityKind) -> bool {
        self.0 & Self::bit(k) != 0
    }
    fn add(&mut self, k: EntityKind) {
        self.0 |= Self::bit(k);
    }
    pub fn kinds(self) -> impl Iterator<Item = EntityKind> {
        EntityKind::ALL.into_iter().filter(move |k| self.has(*k))
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    fn property(self) -> Option<EntityKind> {
        [
            EntityKind::ObjectProperty,
            EntityKind::DataProperty,
            EntityKind::AnnotationProperty,
        ]
        .into_iter()
        .find(|k| self.has(*k))
    }
}

/// The typing of the IRIs of a read: what the declarations say (with the built-in
/// declarations and those the imports closure holds), and what lenient mode inferred.
#[derive(Clone, Debug, Default)]
pub struct Typing {
    pub declared: FxHashMap<String, Kinds>,
    pub inferred: FxHashMap<String, Kinds>,
    pub mode: Mode,
}

impl Typing {
    /// Whether `iri` is declared, built in or (lenient) typed as `k`.
    pub fn is(&self, iri: &str, k: EntityKind) -> bool {
        self.is_declared(iri, k)
            || (self.mode == Mode::Lenient && self.inferred.get(iri).is_some_and(|x| x.has(k)))
    }

    /// Whether `iri` is declared or built in as `k`.
    pub fn is_declared(&self, iri: &str, k: EntityKind) -> bool {
        builtin_kind(iri) == Some(k) || self.declared.get(iri).is_some_and(|x| x.has(k))
    }

    fn kinds(&self, iri: &str) -> Kinds {
        let mut out = self.declared.get(iri).copied().unwrap_or_default();
        if let Some(b) = builtin_kind(iri) {
            out.add(b);
        }
        if self.mode == Mode::Lenient
            && let Some(i) = self.inferred.get(iri)
        {
            out.0 |= i.0;
        }
        out
    }

    /// Infer `k` for `iri` unless it already has a conflicting kind; true if it changed.
    fn infer(&mut self, iri: &str, k: EntityKind) -> bool {
        if self.mode == Mode::Strict || v::is_reserved(iri) && builtin_kind(iri).is_none() {
            return false;
        }
        let have = self.kinds(iri);
        if have.has(k) {
            return false;
        }
        let property = matches!(
            k,
            EntityKind::ObjectProperty | EntityKind::DataProperty | EntityKind::AnnotationProperty
        );
        if property && have.property().is_some() {
            return false;
        }
        if k == EntityKind::Class && have.has(EntityKind::Datatype)
            || k == EntityKind::Datatype && have.has(EntityKind::Class)
        {
            return false;
        }
        self.inferred.entry(iri.to_string()).or_default().add(k);
        true
    }
}

/// The kind of a built-in entity (§5.8 Table 5).
pub fn builtin_kind(iri: &str) -> Option<EntityKind> {
    if iri == v::OWL_THING || iri == v::OWL_NOTHING {
        Some(EntityKind::Class)
    } else if iri == v::OWL_TOP_OBJECT_PROPERTY || iri == v::OWL_BOTTOM_OBJECT_PROPERTY {
        Some(EntityKind::ObjectProperty)
    } else if iri == v::OWL_TOP_DATA_PROPERTY || iri == v::OWL_BOTTOM_DATA_PROPERTY {
        Some(EntityKind::DataProperty)
    } else if v::is_builtin_datatype(iri) {
        Some(EntityKind::Datatype)
    } else if v::is_builtin_annotation_property(iri) {
        Some(EntityKind::AnnotationProperty)
    } else {
        None
    }
}

/// An ontology read from triples.
#[derive(Clone, Debug, Default)]
pub struct Ontology {
    /// the header of the first ontology node of the primary graphs (or an anonymous
    /// ontology)
    pub header: Header,
    /// the graph of the header's node
    pub header_graph: Option<u32>,
    /// the headers of the other ontology nodes, such as those of imported ontologies
    pub other_headers: Vec<(u32, Header)>,
    /// every axiom, with each place it was read from
    pub axioms: BTreeMap<Axiom, Vec<Origin>>,
    /// the triples read, and the graph of each
    pub triples: Vec<Triple>,
    pub graph_of: Vec<u32>,
    /// indices of the triples that map to no axiom
    pub unmapped: Vec<usize>,
    pub diagnostics: Vec<Diagnostic>,
    /// diagnostics past the limit, counted
    pub diagnostics_dropped: usize,
    pub typing: Typing,
    pub mode: Mode,
    pub terminology_only: bool,
    /// the blank nodes of axioms, annotations and negative assertions (RIND), and the
    /// ontology nodes, which are never anonymous individuals
    pub reification_nodes: FxHashSet<BlankNode>,
    pub ontology_nodes: FxHashSet<NamedOrBlankNode>,
}

impl Ontology {
    /// The axioms, in order.
    pub fn axiom_set(&self) -> impl Iterator<Item = &Axiom> {
        self.axioms.keys()
    }

    pub fn len(&self) -> usize {
        self.axioms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.axioms.is_empty()
    }

    pub fn contains(&self, ax: &Axiom) -> bool {
        self.axioms.contains_key(ax)
    }

    /// The warnings and errors that make a strict read differ from this one, or that a
    /// strict read reported: what keeps the ontology from being read as OWL 2 DL.
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter()
    }

    /// The axiom that a triple outside the terminology encodes, for a store ontology
    /// whose assertions are read one triple at a time: a class assertion with a class, an
    /// individual equality or inequality, or a property assertion. `None` for a triple
    /// that maps to nothing.
    pub fn map_assertion(&self, t: &Triple) -> Option<Axiom> {
        let p = t.predicate.as_str();
        let subject_individual = || individual_of(&t.subject);
        let kind = match p {
            v::RDF_TYPE => {
                let Term::NamedNode(c) = &t.object else {
                    return None;
                };
                if !self.typing.is(c.as_str(), EntityKind::Class)
                    && !(self.mode == Mode::Lenient && !v::is_reserved(c.as_str()))
                {
                    return None;
                }
                AxiomKind::ClassAssertion(
                    ClassExpression::Class(Class(c.clone())),
                    subject_individual(),
                )
            }
            v::OWL_SAME_AS | v::OWL_DIFFERENT_FROM => {
                let o = term_individual(&t.object)?;
                let s = subject_individual();
                if p != v::OWL_SAME_AS && s == o {
                    return None;
                }
                let set: BTreeSet<Individual> = [s, o].into_iter().collect();
                if p == v::OWL_SAME_AS {
                    AxiomKind::SameIndividual(set)
                } else {
                    AxiomKind::DifferentIndividuals(set)
                }
            }
            p if v::is_reserved(p) && builtin_kind(p).is_none() => return None,
            p => {
                let kinds = self.typing.kinds(p);
                let lenient = self.mode == Mode::Lenient;
                if kinds.has(EntityKind::AnnotationProperty)
                    || lenient && kinds.is_empty() && self.annotation_like(&t.subject)
                {
                    let s = match &t.subject {
                        NamedOrBlankNode::NamedNode(n) => AnnotationSubject::Iri(n.clone()),
                        NamedOrBlankNode::BlankNode(b) => {
                            AnnotationSubject::Anonymous(AnonymousIndividual::new(b.as_str()))
                        }
                    };
                    AxiomKind::AnnotationAssertion(
                        AnnotationProperty(iri(p)),
                        s,
                        annotation_value(&t.object)?,
                    )
                } else if matches!(&t.object, Term::Literal(_))
                    && (kinds.has(EntityKind::DataProperty) || lenient && kinds.is_empty())
                {
                    let Term::Literal(l) = &t.object else {
                        unreachable!()
                    };
                    AxiomKind::DataPropertyAssertion(
                        DataProperty(iri(p)),
                        subject_individual(),
                        Literal(l.clone()),
                    )
                } else if !matches!(&t.object, Term::Literal(_))
                    && (kinds.has(EntityKind::ObjectProperty) || lenient && kinds.is_empty())
                {
                    AxiomKind::ObjectPropertyAssertion(
                        ObjectPropertyExpression::Property(ObjectProperty(iri(p))),
                        subject_individual(),
                        term_individual(&t.object)?,
                    )
                } else {
                    return None;
                }
            }
        };
        Some(kind.into())
    }

    /// Whether a subject is an entity other than an individual, or an ontology node:
    /// lenient mode reads a triple about it with an undeclared predicate as an
    /// annotation.
    fn annotation_like(&self, s: &NamedOrBlankNode) -> bool {
        if self.ontology_nodes.contains(s) {
            return true;
        }
        match s {
            NamedOrBlankNode::NamedNode(n) => {
                let k = self.typing.kinds(n.as_str());
                k.has(EntityKind::Class) || k.has(EntityKind::Datatype) || k.property().is_some()
            }
            NamedOrBlankNode::BlankNode(b) => self.reification_nodes.contains(b),
        }
    }
}

fn individual_of(s: &NamedOrBlankNode) -> Individual {
    match s {
        NamedOrBlankNode::NamedNode(n) => Individual::Named(NamedIndividual(n.clone())),
        NamedOrBlankNode::BlankNode(b) => {
            Individual::Anonymous(AnonymousIndividual::new(b.as_str()))
        }
    }
}

fn term_individual(t: &Term) -> Option<Individual> {
    match t {
        Term::NamedNode(n) => Some(Individual::Named(NamedIndividual(n.clone()))),
        Term::BlankNode(b) => Some(Individual::Anonymous(AnonymousIndividual::new(b.as_str()))),
        _ => None,
    }
}

fn annotation_value(t: &Term) -> Option<AnnotationValue> {
    match t {
        Term::NamedNode(n) => Some(AnnotationValue::Iri(n.clone())),
        Term::BlankNode(b) => Some(AnnotationValue::Anonymous(AnonymousIndividual::new(
            b.as_str(),
        ))),
        Term::Literal(l) => Some(AnnotationValue::Literal(Literal(l.clone()))),
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

/// Whether a triple belongs to the terminology that a store ontology reads eagerly:
/// structural predicates, facets, `rdf:type` with a structural type, and the
/// predicates in `annotation_properties`. Its complement is what
/// [`Ontology::map_assertion`] maps.
pub fn is_terminological(t: &Triple, annotation_properties: &dyn Fn(&str) -> bool) -> bool {
    let p = t.predicate.as_str();
    if p == v::RDF_TYPE {
        return match &t.object {
            Term::NamedNode(o) => v::STRUCTURAL_TYPES.contains(&o.as_str()),
            // a class assertion with a class expression
            Term::BlankNode(_) => true,
            _ => false,
        };
    }
    v::STRUCTURAL_PREDICATES.contains(&p)
        || v::FACETS.contains(&p)
        || v::is_builtin_annotation_property(p)
        || annotation_properties(p)
}

/// Read an ontology from triples. `graph_of[i]` is the graph of `triples[i]`, and the
/// header is taken from the graphs in `primary` first.
pub fn read(
    triples: Vec<Triple>,
    graph_of: Vec<u32>,
    primary: &[u32],
    opts: ReadOptions,
) -> Ontology {
    assert_eq!(triples.len(), graph_of.len());
    let mut p = Parser::new(&triples, &graph_of, opts);
    p.run(primary);
    let Parser {
        axioms,
        used,
        diags,
        dropped,
        typing,
        headers,
        rind,
        onodes,
        ..
    } = p;
    let mut header = None;
    let mut others = Vec::new();
    for (g, _, h, primary_header) in headers {
        if primary_header && header.is_none() {
            header = Some((g, h));
        } else {
            others.push((g, h));
        }
    }
    let unmapped = (0..triples.len()).filter(|i| !used[*i]).collect();
    let (header_graph, header) = match header {
        Some((g, h)) => (Some(g), h),
        None => (None, Header::default()),
    };
    Ontology {
        header,
        header_graph,
        other_headers: others,
        axioms,
        triples,
        graph_of,
        unmapped,
        diagnostics: diags,
        diagnostics_dropped: dropped,
        typing,
        mode: opts.mode,
        terminology_only: opts.terminology_only,
        reification_nodes: rind,
        ontology_nodes: onodes,
    }
}

/// Read the triples of one graph.
pub fn read_graph(triples: impl IntoIterator<Item = Triple>, opts: ReadOptions) -> Ontology {
    let triples: Vec<Triple> = triples.into_iter().collect();
    let n = triples.len();
    read(triples, vec![0; n], &[0], opts)
}

/// A parsed structure and the triples it consumes.
#[derive(Clone, Debug)]
struct Parsed<T> {
    value: T,
    triples: Vec<usize>,
}

type Key = NamedOrBlankNode;

fn key_of(t: &Term) -> Option<Key> {
    match t {
        Term::NamedNode(n) => Some(n.clone().into()),
        Term::BlankNode(b) => Some(b.clone().into()),
        _ => None,
    }
}

fn term_of(k: &Key) -> Term {
    match k {
        NamedOrBlankNode::NamedNode(n) => n.clone().into(),
        NamedOrBlankNode::BlankNode(b) => b.clone().into(),
    }
}

struct Parser<'a> {
    t: &'a [Triple],
    g: &'a [u32],
    opts: ReadOptions,
    strict: bool,
    by_s: FxHashMap<Key, Vec<usize>>,
    by_p: FxHashMap<&'a str, Vec<usize>>,
    /// triples whose object is a blank node, by that node; references through
    /// `owl:annotatedSource` and `owl:annotatedTarget` are not counted
    refs: FxHashMap<BlankNode, Vec<usize>>,
    used: Vec<bool>,
    /// main triples consumed by an annotated axiom, which another `owl:Axiom` node may
    /// annotate again
    annotated_main: FxHashSet<usize>,
    typing: Typing,
    ce_memo: FxHashMap<BlankNode, Option<Parsed<ClassExpression>>>,
    dr_memo: FxHashMap<BlankNode, Option<Parsed<DataRange>>>,
    /// parsing in progress, to stop on cycles
    active: FxHashSet<BlankNode>,
    nesting_reported: bool,
    shared_warned: FxHashSet<BlankNode>,
    axioms: BTreeMap<Axiom, Vec<Origin>>,
    diags: Vec<Diagnostic>,
    dropped: usize,
    headers: Vec<(u32, Key, Header, bool)>,
    rind: FxHashSet<BlankNode>,
    onodes: FxHashSet<NamedOrBlankNode>,
    /// the annotation triples by subject (Table 10), and the `owl:Annotation` nodes by
    /// (source, property, target)
    ann_triples: FxHashMap<Key, Vec<usize>>,
    ann_nodes: FxHashMap<(Key, String, Term), Vec<BlankNode>>,
    /// annotation subjects whose annotations an axiom or a header took
    ann_taken: FxHashSet<Key>,
}

impl<'a> Parser<'a> {
    fn new(t: &'a [Triple], g: &'a [u32], opts: ReadOptions) -> Parser<'a> {
        let mut by_s: FxHashMap<Key, Vec<usize>> = FxHashMap::default();
        let mut by_p: FxHashMap<&'a str, Vec<usize>> = FxHashMap::default();
        let mut refs: FxHashMap<BlankNode, Vec<usize>> = FxHashMap::default();
        for (i, tr) in t.iter().enumerate() {
            by_s.entry(tr.subject.clone()).or_default().push(i);
            by_p.entry(tr.predicate.as_str()).or_default().push(i);
            if let Term::BlankNode(b) = &tr.object
                && !matches!(
                    tr.predicate.as_str(),
                    v::OWL_ANNOTATED_SOURCE | v::OWL_ANNOTATED_TARGET
                )
            {
                refs.entry(b.clone()).or_default().push(i);
            }
        }
        Parser {
            t,
            g,
            opts,
            strict: opts.mode == Mode::Strict,
            by_s,
            by_p,
            refs,
            used: vec![false; t.len()],
            annotated_main: FxHashSet::default(),
            typing: Typing {
                mode: opts.mode,
                ..Default::default()
            },
            ce_memo: FxHashMap::default(),
            dr_memo: FxHashMap::default(),
            active: FxHashSet::default(),
            nesting_reported: false,
            shared_warned: FxHashSet::default(),
            axioms: BTreeMap::new(),
            diags: Vec::new(),
            dropped: 0,
            headers: Vec::new(),
            rind: FxHashSet::default(),
            onodes: FxHashSet::default(),
            ann_triples: FxHashMap::default(),
            ann_nodes: FxHashMap::default(),
            ann_taken: FxHashSet::default(),
        }
    }

    fn diag(
        &mut self,
        severity: Severity,
        code: &'static str,
        message: String,
        triples: Vec<usize>,
    ) {
        let max = if self.opts.max_diagnostics == 0 {
            1000
        } else {
            self.opts.max_diagnostics
        };
        if self.diags.len() >= max {
            self.dropped += 1;
            return;
        }
        self.diags.push(Diagnostic {
            severity,
            code,
            message,
            triples,
        });
    }

    fn warn(&mut self, code: &'static str, message: String, triples: Vec<usize>) {
        self.diag(Severity::Warning, code, message, triples);
    }

    /// An expression nested deeper than [`MAX_NESTING`]: it stays unmapped, once
    /// reported, so that a long chain of blank nodes cannot exhaust the stack.
    fn too_deep(&mut self, b: &BlankNode) {
        if !self.nesting_reported {
            self.nesting_reported = true;
            self.diag(
                Severity::Error,
                "nesting",
                format!(
                    "the expression at _:{} is nested more than {MAX_NESTING} levels deep and is not read",
                    b.as_str()
                ),
                Vec::new(),
            );
        }
    }

    // ---------------------------------------------------------------- lookups ------

    fn pred(&self, p: &str) -> Vec<usize> {
        self.by_p.get(p).cloned().unwrap_or_default()
    }

    fn about(&self, s: &Key) -> &[usize] {
        self.by_s.get(s).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The triples `s p ?o`.
    fn sp(&self, s: &Key, p: &str) -> Vec<usize> {
        self.about(s)
            .iter()
            .copied()
            .filter(|i| self.t[*i].predicate.as_str() == p)
            .collect()
    }

    /// The single object of `s p ?o`; `None` when there is none or several.
    fn one(&self, s: &Key, p: &str) -> Option<(usize, &'a Term)> {
        let v = self.sp(s, p);
        match v.as_slice() {
            [i] => Some((*i, &self.t[*i].object)),
            _ => None,
        }
    }

    fn has_type(&self, s: &Key, ty: &str) -> Option<usize> {
        self.about(s).iter().copied().find(|i| {
            let tr = &self.t[*i];
            tr.predicate.as_str() == v::RDF_TYPE
                && matches!(&tr.object, Term::NamedNode(o) if o.as_str() == ty)
        })
    }

    /// The number of triples that use blank node `b` as their object.
    fn ref_count(&self, b: &BlankNode) -> usize {
        self.refs.get(b).map_or(0, Vec::len)
    }

    fn graph_of_triples(&self, ts: &[usize]) -> u32 {
        ts.first().map_or(0, |i| self.g[*i])
    }

    // ---------------------------------------------------------------- the run ------

    fn run(&mut self, primary: &[u32]) {
        self.header(primary);
        self.compat();
        self.declarations();
        if !self.strict {
            self.infer();
        }
        self.reification_nodes();
        self.annotations();
        self.annotated_axioms();
        self.blank_axioms();
        self.plain_axioms();
        self.annotation_assertions();
        self.header_annotations();
        if self.strict {
            let unmapped: Vec<usize> = (0..self.t.len()).filter(|i| !self.used[*i]).collect();
            for i in unmapped {
                let msg = format!("the triple {} maps to no axiom", self.t[i]);
                self.diag(Severity::Error, "unmapped", msg, vec![i]);
            }
        }
    }

    /// Table 4: the ontology headers.
    fn header(&mut self, primary: &[u32]) {
        let mut nodes: Vec<(bool, Key, usize)> = Vec::new();
        for i in self.pred(v::RDF_TYPE) {
            if matches!(&self.t[i].object, Term::NamedNode(o) if o.as_str() == v::OWL_ONTOLOGY) {
                let s = self.t[i].subject.clone();
                // the same node can be typed in several graphs
                self.used[i] = true;
                if !nodes.iter().any(|(_, k, _)| *k == s) {
                    nodes.push((!primary.contains(&self.g[i]), s, i));
                }
            }
        }
        // An ontology node that another ontology node points to in a primary graph,
        // such as the object of owl:priorVersion, describes another ontology and is
        // not a candidate for the header.
        let pointed: Vec<bool> = nodes
            .iter()
            .map(|(_, k, _)| {
                let o: Term = k.clone().into();
                nodes.iter().any(|(_, other, _)| {
                    other != k
                        && self
                            .about(other)
                            .iter()
                            .any(|i| self.t[*i].object == o && primary.contains(&self.g[*i]))
                })
            })
            .collect();
        let mut nodes: Vec<(bool, Key, usize)> = nodes
            .into_iter()
            .zip(pointed)
            .map(|((other, k, i), p)| (other || p, k, i))
            .collect();
        // candidates first, then by term order
        nodes.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.to_string().cmp(&b.1.to_string()))
        });
        let primaries = nodes.iter().filter(|n| !n.0).count();
        if primaries > 1 {
            let extra: Vec<usize> = nodes.iter().filter(|n| !n.0).map(|n| n.2).collect();
            self.warn(
                "header",
                format!(
                    "{primaries} ontology nodes in the graphs read; the header is {}",
                    nodes[0].1
                ),
                extra,
            );
        }
        for (other, node, ti) in nodes {
            self.used[ti] = true;
            let mut h = Header {
                iri: match &node {
                    NamedOrBlankNode::NamedNode(n) => Some(n.clone()),
                    NamedOrBlankNode::BlankNode(_) => None,
                },
                ..Default::default()
            };
            for i in self.sp(&node, v::OWL_VERSION_IRI) {
                if let Term::NamedNode(n) = &self.t[i].object
                    && h.iri.is_some()
                    && h.version_iri.is_none()
                {
                    h.version_iri = Some(n.clone());
                    self.used[i] = true;
                }
            }
            for i in self.sp(&node, v::OWL_IMPORTS) {
                if let Term::NamedNode(n) = &self.t[i].object {
                    h.imports.insert(n.clone());
                    self.used[i] = true;
                }
            }
            self.onodes.insert(node.clone());
            self.headers.push((self.g[ti], node, h, !other));
        }
    }

    /// Tables 5 and 6: the triples removed and the declarations added for OWL 1.
    fn compat(&mut self) {
        for i in self.pred(v::RDF_TYPE) {
            let tr = &self.t[i];
            let Term::NamedNode(o) = &tr.object else {
                continue;
            };
            let s = tr.subject.clone();
            let remove = match o.as_str() {
                v::RDFS_CLASS => [
                    v::OWL_CLASS,
                    v::RDFS_DATATYPE,
                    v::OWL_DATA_RANGE,
                    v::OWL_RESTRICTION,
                ]
                .iter()
                .any(|t| self.has_type(&s, t).is_some()),
                v::OWL_CLASS => self.has_type(&s, v::OWL_RESTRICTION).is_some(),
                v::RDF_PROPERTY => [
                    v::OWL_OBJECT_PROPERTY,
                    v::OWL_FUNCTIONAL_PROPERTY,
                    v::OWL_INVERSE_FUNCTIONAL_PROPERTY,
                    v::OWL_TRANSITIVE_PROPERTY,
                    v::OWL_DATATYPE_PROPERTY,
                    v::OWL_ANNOTATION_PROPERTY,
                    v::OWL_ONTOLOGY_PROPERTY,
                ]
                .iter()
                .any(|t| self.has_type(&s, t).is_some()),
                v::RDF_LIST => {
                    !self.sp(&s, v::RDF_FIRST).is_empty() && !self.sp(&s, v::RDF_REST).is_empty()
                }
                _ => false,
            };
            if remove {
                self.used[i] = true;
            }
        }
    }

    /// Table 7 with Table 6's additions: the declared kinds of IRIs, and the
    /// declarations Table 6 implies.
    fn declarations(&mut self) {
        for i in self.pred(v::RDF_TYPE) {
            let tr = &self.t[i];
            let (NamedOrBlankNode::NamedNode(s), Term::NamedNode(o)) = (&tr.subject, &tr.object)
            else {
                continue;
            };
            let kind = match o.as_str() {
                v::OWL_ONTOLOGY_PROPERTY => Some(EntityKind::AnnotationProperty),
                v::OWL_INVERSE_FUNCTIONAL_PROPERTY
                | v::OWL_TRANSITIVE_PROPERTY
                | v::OWL_SYMMETRIC_PROPERTY => {
                    // Table 6 adds the declaration; the triple itself stays for the axiom
                    self.typing
                        .declared
                        .entry(s.as_str().to_string())
                        .or_default()
                        .add(EntityKind::ObjectProperty);
                    let ax =
                        Axiom::declaration(Entity::of_kind(EntityKind::ObjectProperty, s.clone()));
                    let g = self.g[i];
                    self.axioms.entry(ax).or_default().push(Origin {
                        graph: g,
                        triples: Vec::new(),
                        main: None,
                    });
                    None
                }
                o => EntityKind::of_rdf_type(o),
            };
            if let Some(k) = kind {
                self.typing
                    .declared
                    .entry(s.as_str().to_string())
                    .or_default()
                    .add(k);
            }
        }
        // declarations that only an owl:Axiom node states
        for i in self.pred(v::OWL_ANNOTATED_PROPERTY) {
            if !matches!(&self.t[i].object, Term::NamedNode(p) if p.as_str() == v::RDF_TYPE) {
                continue;
            }
            let x = self.t[i].subject.clone();
            if let (Some((_, Term::NamedNode(s))), Some((_, Term::NamedNode(o)))) = (
                self.one(&x, v::OWL_ANNOTATED_SOURCE),
                self.one(&x, v::OWL_ANNOTATED_TARGET),
            ) && let Some(k) = EntityKind::of_rdf_type(o.as_str())
            {
                self.typing
                    .declared
                    .entry(s.as_str().to_string())
                    .or_default()
                    .add(k);
            }
        }
        // remove implied declarations that an explicit declaration repeats
        let implied: Vec<Axiom> = self.axioms.keys().cloned().collect();
        for ax in implied {
            if let AxiomKind::Declaration(e) = &ax.kind
                && self
                    .has_type(&e.iri().clone().into(), v::OWL_OBJECT_PROPERTY)
                    .is_some()
            {
                self.axioms.remove(&ax);
            }
        }
    }

    /// Lenient typing of undeclared IRIs from their use.
    fn infer(&mut self) {
        let mut changed = true;
        let mut rounds = 0;
        let before: FxHashSet<String> = self.typing.declared.keys().cloned().collect();
        while changed && rounds < 8 {
            changed = false;
            rounds += 1;
            for i in 0..self.t.len() {
                changed |= self.infer_from(i);
            }
        }
        // undeclared predicates of the remaining triples
        let mut stats: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
        for i in 0..self.t.len() {
            let tr = &self.t[i];
            let p = tr.predicate.as_str();
            if v::is_reserved(p) || !self.typing.kinds(p).is_empty() {
                continue;
            }
            let e = stats.entry(p.to_string()).or_default();
            if self.annotation_subject(&tr.subject) {
                e.0 += 1;
            } else if matches!(tr.object, Term::Literal(_)) {
                e.1 += 1;
            } else {
                e.2 += 1;
            }
        }
        for (p, (ann, lit, res)) in stats {
            let k = if ann > 0 && lit == 0 && res == 0 {
                EntityKind::AnnotationProperty
            } else if ann == 0 && res == 0 {
                EntityKind::DataProperty
            } else if ann == 0 && lit == 0 {
                EntityKind::ObjectProperty
            } else {
                EntityKind::AnnotationProperty
            };
            self.typing.infer(&p, k);
        }
        let inferred: Vec<(String, Kinds)> = self
            .typing
            .inferred
            .iter()
            .filter(|(k, _)| !before.contains(*k))
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        let mut inferred = inferred;
        inferred.sort_by(|a, b| a.0.cmp(&b.0));
        for (iri, kinds) in inferred {
            let names: Vec<&str> = kinds.kinds().map(EntityKind::name).collect();
            let triples: Vec<usize> = self
                .t
                .iter()
                .enumerate()
                .filter(|(_, t)| mentions(t, &iri))
                .map(|(i, _)| i)
                .take(8)
                .collect();
            self.warn(
                "undeclared",
                format!(
                    "<{iri}> is not declared and was read as {} from its use",
                    names.join(" and ")
                ),
                triples,
            );
        }
    }

    fn annotation_subject(&self, s: &Key) -> bool {
        match s {
            NamedOrBlankNode::NamedNode(n) => {
                let k = self.typing.kinds(n.as_str());
                k.has(EntityKind::Class)
                    || k.has(EntityKind::Datatype)
                    || k.property().is_some()
                    || self.has_type(s, v::OWL_ONTOLOGY).is_some()
            }
            NamedOrBlankNode::BlankNode(_) => [
                v::OWL_AXIOM,
                v::OWL_ANNOTATION,
                v::OWL_ONTOLOGY,
                v::OWL_ALL_DISJOINT_CLASSES,
                v::OWL_ALL_DISJOINT_PROPERTIES,
                v::OWL_ALL_DIFFERENT,
                v::OWL_NEGATIVE_PROPERTY_ASSERTION,
            ]
            .iter()
            .any(|t| self.has_type(s, t).is_some()),
        }
    }

    fn infer_iri(&mut self, t: &Term, k: EntityKind) -> bool {
        match t {
            Term::NamedNode(n)
                if self.typing.kinds(n.as_str()).is_empty() || k == EntityKind::Class =>
            {
                self.typing.infer(n.as_str(), k)
            }
            _ => false,
        }
    }

    /// The items of a list, without checks, for inference.
    fn list_items_loose(&self, head: &Term) -> Vec<Term> {
        let mut out = Vec::new();
        let mut cur = head.clone();
        let mut seen = FxHashSet::default();
        while let Term::BlankNode(b) = &cur {
            if !seen.insert(b.clone()) {
                break;
            }
            let k: Key = b.clone().into();
            match (self.one(&k, v::RDF_FIRST), self.one(&k, v::RDF_REST)) {
                (Some((_, f)), Some((_, r))) => {
                    out.push(f.clone());
                    cur = r.clone();
                }
                _ => break,
            }
        }
        out
    }

    fn is_datatype_term(&self, t: &Term) -> bool {
        match t {
            Term::NamedNode(n) => self.typing.is(n.as_str(), EntityKind::Datatype),
            Term::BlankNode(b) => {
                let k: Key = b.clone().into();
                self.has_type(&k, v::RDFS_DATATYPE).is_some()
                    || self.has_type(&k, v::OWL_DATA_RANGE).is_some()
                    || !self.sp(&k, v::OWL_ON_DATATYPE).is_empty()
                    || !self.sp(&k, v::OWL_DATATYPE_COMPLEMENT_OF).is_empty()
            }
            _ => false,
        }
    }

    fn is_property_kind(&self, t: &Term, k: EntityKind) -> bool {
        match t {
            Term::NamedNode(n) => self.typing.is(n.as_str(), k),
            Term::BlankNode(b) => {
                k == EntityKind::ObjectProperty
                    && !self.sp(&b.clone().into(), v::OWL_INVERSE_OF).is_empty()
            }
            _ => false,
        }
    }

    fn infer_from(&mut self, i: usize) -> bool {
        let tr = &self.t[i];
        let s = term_of(&tr.subject);
        let o = tr.object.clone();
        let mut c = false;
        match tr.predicate.as_str() {
            v::RDFS_SUB_CLASS_OF | v::OWL_DISJOINT_WITH => {
                c |= self.infer_iri(&s, EntityKind::Class);
                c |= self.infer_iri(&o, EntityKind::Class);
            }
            v::OWL_EQUIVALENT_CLASS => {
                if self.is_datatype_term(&s) || self.is_datatype_term(&o) {
                    c |= self.infer_iri(&s, EntityKind::Datatype);
                    c |= self.infer_iri(&o, EntityKind::Datatype);
                } else {
                    c |= self.infer_iri(&s, EntityKind::Class);
                    c |= self.infer_iri(&o, EntityKind::Class);
                }
            }
            v::OWL_DISJOINT_UNION_OF => {
                c |= self.infer_iri(&s, EntityKind::Class);
                for item in self.list_items_loose(&o) {
                    c |= self.infer_iri(&item, EntityKind::Class);
                }
            }
            v::RDF_TYPE => {
                if let Term::NamedNode(n) = &o
                    && !v::is_reserved(n.as_str())
                {
                    c |= self.infer_iri(&o, EntityKind::Class);
                }
            }
            v::OWL_INTERSECTION_OF | v::OWL_UNION_OF => {
                let k: Key = tr.subject.clone();
                let data = self.has_type(&k, v::RDFS_DATATYPE).is_some()
                    || self.has_type(&k, v::OWL_DATA_RANGE).is_some();
                let kind = if data {
                    EntityKind::Datatype
                } else {
                    EntityKind::Class
                };
                if let NamedOrBlankNode::NamedNode(_) = tr.subject {
                    c |= self.infer_iri(&s, kind);
                }
                for item in self.list_items_loose(&o) {
                    c |= self.infer_iri(&item, kind);
                }
            }
            v::OWL_COMPLEMENT_OF => {
                c |= self.infer_iri(&o, EntityKind::Class);
                if let NamedOrBlankNode::NamedNode(_) = tr.subject {
                    c |= self.infer_iri(&s, EntityKind::Class);
                }
            }
            v::OWL_DATATYPE_COMPLEMENT_OF | v::OWL_ON_DATA_RANGE => {
                c |= self.infer_iri(&o, EntityKind::Datatype);
            }
            v::OWL_ON_CLASS => {
                c |= self.infer_iri(&o, EntityKind::Class);
            }
            v::OWL_ON_PROPERTY => {
                let r: Key = tr.subject.clone();
                if let Term::NamedNode(p) = &o
                    && self.typing.kinds(p.as_str()).property().is_none()
                {
                    let k = self.restriction_kind(&r);
                    c |= self.typing.infer(p.as_str(), k);
                }
                // the filler's kind follows the property's
                let data = self.is_property_kind(&o, EntityKind::DataProperty);
                for f in [v::OWL_SOME_VALUES_FROM, v::OWL_ALL_VALUES_FROM] {
                    if let Some((_, fv)) = self.one(&r, f) {
                        let fv = fv.clone();
                        c |= self.infer_iri(
                            &fv,
                            if data {
                                EntityKind::Datatype
                            } else {
                                EntityKind::Class
                            },
                        );
                    }
                }
            }
            v::OWL_ON_PROPERTIES => {
                for item in self.list_items_loose(&o) {
                    c |= self.infer_iri(&item, EntityKind::DataProperty);
                }
            }
            v::OWL_INVERSE_OF => {
                c |= self.infer_iri(&s, EntityKind::ObjectProperty);
                c |= self.infer_iri(&o, EntityKind::ObjectProperty);
            }
            v::OWL_PROPERTY_CHAIN_AXIOM => {
                c |= self.infer_iri(&s, EntityKind::ObjectProperty);
                for item in self.list_items_loose(&o) {
                    c |= self.infer_iri(&item, EntityKind::ObjectProperty);
                }
            }
            v::RDFS_SUB_PROPERTY_OF
            | v::OWL_EQUIVALENT_PROPERTY
            | v::OWL_PROPERTY_DISJOINT_WITH => {
                for (a, b) in [(&s, &o), (&o, &s)] {
                    if let (Term::NamedNode(an), Term::NamedNode(_)) = (a, b)
                        && let Some(k) = self.typing.kinds(an.as_str()).property()
                    {
                        c |= self.infer_iri(b, k);
                    }
                }
            }
            v::RDFS_DOMAIN => {
                if !self.is_property_kind(&s, EntityKind::AnnotationProperty) {
                    c |= self.infer_iri(&o, EntityKind::Class);
                }
            }
            v::RDFS_RANGE => {
                if self.is_property_kind(&s, EntityKind::AnnotationProperty) {
                } else if self.is_datatype_term(&o) {
                    c |= self.infer_iri(&s, EntityKind::DataProperty);
                } else if self.is_property_kind(&s, EntityKind::ObjectProperty) {
                    c |= self.infer_iri(&o, EntityKind::Class);
                } else if self.is_property_kind(&s, EntityKind::DataProperty) {
                    c |= self.infer_iri(&o, EntityKind::Datatype);
                } else if let Term::NamedNode(n) = &o
                    && self.typing.is(n.as_str(), EntityKind::Class)
                {
                    c |= self.infer_iri(&s, EntityKind::ObjectProperty);
                }
            }
            v::OWL_HAS_VALUE | v::OWL_SOME_VALUES_FROM | v::OWL_ALL_VALUES_FROM => {}
            v::OWL_SOURCE_INDIVIDUAL => {}
            v::OWL_ASSERTION_PROPERTY => {
                let x: Key = tr.subject.clone();
                if self.one(&x, v::OWL_TARGET_VALUE).is_some() {
                    c |= self.infer_iri(&o, EntityKind::DataProperty);
                } else if self.one(&x, v::OWL_TARGET_INDIVIDUAL).is_some() {
                    c |= self.infer_iri(&o, EntityKind::ObjectProperty);
                }
            }
            _ => {}
        }
        c
    }

    /// Whether a restriction node is on a data property, from its filler.
    fn restriction_kind(&self, r: &Key) -> EntityKind {
        if self.one(r, v::OWL_ON_DATA_RANGE).is_some() {
            return EntityKind::DataProperty;
        }
        if let Some((_, Term::Literal(_))) = self.one(r, v::OWL_HAS_VALUE) {
            return EntityKind::DataProperty;
        }
        for f in [v::OWL_SOME_VALUES_FROM, v::OWL_ALL_VALUES_FROM] {
            if let Some((_, fv)) = self.one(r, f)
                && self.is_datatype_term(fv)
            {
                return EntityKind::DataProperty;
            }
        }
        EntityKind::ObjectProperty
    }

    /// Table 8: the reification blank nodes.
    fn reification_nodes(&mut self) {
        for i in self.pred(v::RDF_TYPE) {
            let tr = &self.t[i];
            if let (NamedOrBlankNode::BlankNode(b), Term::NamedNode(o)) = (&tr.subject, &tr.object)
                && matches!(
                    o.as_str(),
                    v::OWL_AXIOM
                        | v::OWL_ANNOTATION
                        | v::OWL_ALL_DISJOINT_CLASSES
                        | v::OWL_ALL_DISJOINT_PROPERTIES
                        | v::OWL_ALL_DIFFERENT
                        | v::OWL_NEGATIVE_PROPERTY_ASSERTION
                )
            {
                self.rind.insert(b.clone());
            }
        }
    }

    fn is_ap(&self, p: &str) -> bool {
        self.typing.is(p, EntityKind::AnnotationProperty)
    }

    /// Table 10: index the annotation triples and the `owl:Annotation` nodes.
    fn annotations(&mut self) {
        for i in 0..self.t.len() {
            if self.used[i] {
                continue;
            }
            let tr = &self.t[i];
            if self.is_ap(tr.predicate.as_str()) {
                self.ann_triples
                    .entry(tr.subject.clone())
                    .or_default()
                    .push(i);
            }
        }
        for i in self.pred(v::RDF_TYPE) {
            let tr = &self.t[i];
            let (NamedOrBlankNode::BlankNode(w), Term::NamedNode(o)) = (&tr.subject, &tr.object)
            else {
                continue;
            };
            if o.as_str() != v::OWL_ANNOTATION {
                continue;
            }
            let wk: Key = w.clone().into();
            if let (Some((_, s)), Some((_, Term::NamedNode(p))), Some((_, t))) = (
                self.one(&wk, v::OWL_ANNOTATED_SOURCE),
                self.one(&wk, v::OWL_ANNOTATED_PROPERTY),
                self.one(&wk, v::OWL_ANNOTATED_TARGET),
            ) && let Some(sk) = key_of(s)
            {
                self.ann_nodes
                    .entry((sk, p.as_str().to_string(), t.clone()))
                    .or_default()
                    .push(w.clone());
            }
        }
    }

    /// ANN(x): the annotations of a node, with the triples they consume.
    fn annotations_of(&self, x: &Key, depth: usize) -> Vec<Parsed<Annotation>> {
        let mut out = Vec::new();
        if depth > 32 {
            return out;
        }
        for &i in self.ann_triples.get(x).map(Vec::as_slice).unwrap_or(&[]) {
            if self.used[i] {
                continue;
            }
            let tr = &self.t[i];
            let Some(value) = annotation_value(&tr.object) else {
                continue;
            };
            let property = AnnotationProperty(tr.predicate.clone());
            let key = (
                x.clone(),
                tr.predicate.as_str().to_string(),
                tr.object.clone(),
            );
            let ws = self.ann_nodes.get(&key).cloned().unwrap_or_default();
            if ws.is_empty() {
                out.push(Parsed {
                    value: Annotation::new(property.clone(), value.clone()),
                    triples: vec![i],
                });
            }
            for w in ws {
                let wk: Key = w.clone().into();
                let mut triples = vec![i];
                for p in [
                    v::OWL_ANNOTATED_SOURCE,
                    v::OWL_ANNOTATED_PROPERTY,
                    v::OWL_ANNOTATED_TARGET,
                ] {
                    triples.extend(self.sp(&wk, p));
                }
                if let Some(t) = self.has_type(&wk, v::OWL_ANNOTATION) {
                    triples.push(t);
                }
                let nested = self.annotations_of(&wk, depth + 1);
                let mut a = Annotation::new(property.clone(), value.clone());
                for n in nested {
                    a.annotations.insert(n.value);
                    triples.extend(n.triples);
                }
                out.push(Parsed { value: a, triples });
            }
        }
        out
    }

    fn take_annotations(&mut self, x: &Key) -> (BTreeSet<Annotation>, Vec<usize>) {
        let anns = self.annotations_of(x, 0);
        self.ann_taken.insert(x.clone());
        let mut set = BTreeSet::new();
        let mut triples = Vec::new();
        for a in anns {
            set.insert(a.value);
            triples.extend(a.triples);
        }
        (set, triples)
    }

    // ------------------------------------------------------------- expressions ------

    /// Whether blank node `b` may be parsed as a structure: used once, or shared in
    /// with a warning.
    fn check_shared(&mut self, b: &BlankNode) -> bool {
        if self.ref_count(b) <= 1 {
            return true;
        }
        // The reverse mapping assigns CE(_:x) once and lets any number of axioms refer
        // to it, so a shared expression node is valid OWL 2 DL (the W3C test case
        // Restriction-002 is in OWL 2 DL). Both modes read it once; the warning lets an
        // edit know that removing one of its users keeps the structure.
        if self.shared_warned.insert(b.clone()) {
            let triples = self.refs.get(b).cloned().unwrap_or_default();
            self.warn(
                "shared",
                format!(
                    "the structure _:{} is used by {} triples and was read once and shared",
                    b.as_str(),
                    triples.len()
                ),
                triples,
            );
        }
        true
    }

    /// T(SEQ y1 … yn): a well-formed list and the triples of its cells.
    fn list(&mut self, head: &Term) -> Option<Parsed<Vec<Term>>> {
        let mut items = Vec::new();
        let mut triples = Vec::new();
        let mut cur = head.clone();
        let mut seen = FxHashSet::default();
        loop {
            match &cur {
                Term::NamedNode(n) if n.as_str() == v::RDF_NIL => break,
                Term::BlankNode(b) => {
                    if !seen.insert(b.clone()) {
                        return None;
                    }
                    if self.ref_count(b) > 1 {
                        if self.strict {
                            return None;
                        }
                        if self.shared_warned.insert(b.clone()) {
                            let r = self.refs.get(b).cloned().unwrap_or_default();
                            self.warn(
                                "shared",
                                format!(
                                    "the list cell _:{} is shared and was read once",
                                    b.as_str()
                                ),
                                r,
                            );
                        }
                    }
                    let k: Key = b.clone().into();
                    let (fi, f) = self.one(&k, v::RDF_FIRST)?;
                    let (ri, r) = self.one(&k, v::RDF_REST)?;
                    triples.push(fi);
                    triples.push(ri);
                    items.push(f.clone());
                    cur = r.clone();
                }
                _ => return None,
            }
        }
        Some(Parsed {
            value: items,
            triples,
        })
    }

    fn ope(&mut self, t: &Term) -> Option<Parsed<ObjectPropertyExpression>> {
        match t {
            Term::NamedNode(n) if self.typing.is(n.as_str(), EntityKind::ObjectProperty) => {
                Some(Parsed {
                    value: ObjectPropertyExpression::Property(ObjectProperty(n.clone())),
                    triples: Vec::new(),
                })
            }
            Term::BlankNode(b) => {
                let k: Key = b.clone().into();
                let (i, y) = self.one(&k, v::OWL_INVERSE_OF)?;
                let Term::NamedNode(y) = y else {
                    return None;
                };
                if !self.typing.is(y.as_str(), EntityKind::ObjectProperty) || !self.check_shared(b)
                {
                    return None;
                }
                Some(Parsed {
                    value: ObjectPropertyExpression::InverseOf(ObjectProperty(y.clone())),
                    triples: vec![i],
                })
            }
            _ => None,
        }
    }

    fn dpe(&self, t: &Term) -> Option<DataProperty> {
        match t {
            Term::NamedNode(n) if self.typing.is(n.as_str(), EntityKind::DataProperty) => {
                Some(DataProperty(n.clone()))
            }
            _ => None,
        }
    }

    fn individual(&self, t: &Term) -> Option<Individual> {
        term_individual(t)
    }

    fn nn_int(&self, t: &Term) -> Option<u32> {
        let Term::Literal(l) = t else { return None };
        let dt = l.datatype().as_str();
        let integer_types = [
            v::XSD_NON_NEGATIVE_INTEGER,
            "http://www.w3.org/2001/XMLSchema#integer",
            "http://www.w3.org/2001/XMLSchema#positiveInteger",
            "http://www.w3.org/2001/XMLSchema#int",
            "http://www.w3.org/2001/XMLSchema#long",
            "http://www.w3.org/2001/XMLSchema#short",
            "http://www.w3.org/2001/XMLSchema#byte",
            "http://www.w3.org/2001/XMLSchema#unsignedInt",
            "http://www.w3.org/2001/XMLSchema#unsignedLong",
            "http://www.w3.org/2001/XMLSchema#unsignedShort",
            "http://www.w3.org/2001/XMLSchema#unsignedByte",
        ];
        // Table 13 asks for xsd:nonNegativeInteger; the other integer types and an
        // integral xsd:decimal denote the same values, so both modes accept them
        let ok_type = integer_types.contains(&dt)
            || dt == "http://www.w3.org/2001/XMLSchema#decimal"
            || !self.strict && l.language().is_none();
        if !ok_type {
            return None;
        }
        l.value().trim().trim_start_matches('+').parse::<u32>().ok()
    }

    fn data_range(&mut self, t: &Term) -> Option<Parsed<DataRange>> {
        match t {
            Term::NamedNode(n) if self.typing.is(n.as_str(), EntityKind::Datatype) => {
                Some(Parsed {
                    value: DataRange::Datatype(Datatype(n.clone())),
                    triples: Vec::new(),
                })
            }
            Term::BlankNode(b) => {
                if let Some(m) = self.dr_memo.get(b) {
                    return m.clone();
                }
                if self.active.len() >= MAX_NESTING {
                    self.too_deep(b);
                    return None;
                }
                if !self.active.insert(b.clone()) {
                    return None;
                }
                let r = if self.check_shared(b) {
                    self.data_range_node(b)
                } else {
                    None
                };
                self.active.remove(b);
                self.dr_memo.insert(b.clone(), r.clone());
                r
            }
            _ => None,
        }
    }

    fn data_range_node(&mut self, b: &BlankNode) -> Option<Parsed<DataRange>> {
        let k: Key = b.clone().into();
        let typed = self.has_type(&k, v::RDFS_DATATYPE);
        let owl1 = self.has_type(&k, v::OWL_DATA_RANGE);
        if typed.is_none() && owl1.is_none() && self.strict {
            return None;
        }
        let mut triples: Vec<usize> = typed.into_iter().chain(owl1).collect();
        let preds: Vec<&str> = [
            v::OWL_INTERSECTION_OF,
            v::OWL_UNION_OF,
            v::OWL_DATATYPE_COMPLEMENT_OF,
            v::OWL_ONE_OF,
            v::OWL_ON_DATATYPE,
        ]
        .into_iter()
        .filter(|p| !self.sp(&k, p).is_empty())
        .collect();
        let value = match preds.as_slice() {
            [p @ (v::OWL_INTERSECTION_OF | v::OWL_UNION_OF)] => {
                if owl1.is_some() && typed.is_none() {
                    return None;
                }
                let (i, l) = self.one(&k, p)?;
                let list = self.list(&l.clone())?;
                if list.value.len() < 2 {
                    return None;
                }
                triples.push(i);
                triples.extend(list.triples);
                let mut set = BTreeSet::new();
                for item in &list.value {
                    let r = self.data_range(item)?;
                    triples.extend(r.triples);
                    set.insert(r.value);
                }
                if *p == v::OWL_INTERSECTION_OF {
                    DataRange::IntersectionOf(set)
                } else {
                    DataRange::UnionOf(set)
                }
            }
            [v::OWL_DATATYPE_COMPLEMENT_OF] => {
                let (i, y) = self.one(&k, v::OWL_DATATYPE_COMPLEMENT_OF)?;
                let r = self.data_range(&y.clone())?;
                triples.push(i);
                triples.extend(r.triples);
                DataRange::ComplementOf(Box::new(r.value))
            }
            [v::OWL_ONE_OF] => {
                let (i, l) = self.one(&k, v::OWL_ONE_OF)?;
                let list = self.list(&l.clone())?;
                triples.push(i);
                triples.extend(list.triples);
                let mut set = BTreeSet::new();
                for item in &list.value {
                    let Term::Literal(l) = item else { return None };
                    set.insert(Literal(l.clone()));
                }
                if set.is_empty() {
                    if owl1.is_some() {
                        DataRange::ComplementOf(Box::new(DataRange::Datatype(Datatype::literal())))
                    } else {
                        return None;
                    }
                } else {
                    DataRange::OneOf(set)
                }
            }
            [v::OWL_ON_DATATYPE] => {
                let (i, d) = self.one(&k, v::OWL_ON_DATATYPE)?;
                let Term::NamedNode(d) = d else { return None };
                if !self.typing.is(d.as_str(), EntityKind::Datatype) {
                    return None;
                }
                let d = d.clone();
                let (wi, l) = self.one(&k, v::OWL_WITH_RESTRICTIONS)?;
                let list = self.list(&l.clone())?;
                triples.push(i);
                triples.push(wi);
                triples.extend(list.triples);
                let mut facets = BTreeSet::new();
                for item in &list.value {
                    let Term::BlankNode(z) = item else {
                        return None;
                    };
                    let zk: Key = z.clone().into();
                    let zs = self.about(&zk).to_vec();
                    let [zi] = zs.as_slice() else { return None };
                    let zt = &self.t[*zi];
                    let Term::Literal(lit) = &zt.object else {
                        return None;
                    };
                    triples.push(*zi);
                    facets.insert(FacetRestriction {
                        facet: zt.predicate.clone(),
                        value: Literal(lit.clone()),
                    });
                }
                if facets.is_empty() {
                    return None;
                }
                DataRange::DatatypeRestriction(Datatype(d), facets)
            }
            _ => return None,
        };
        if typed.is_none() && owl1.is_none() {
            self.warn(
                "untyped",
                format!(
                    "the data range _:{} has no rdf:type rdfs:Datatype",
                    b.as_str()
                ),
                triples.clone(),
            );
        }
        Some(Parsed { value, triples })
    }

    fn class_expression(&mut self, t: &Term) -> Option<Parsed<ClassExpression>> {
        match t {
            Term::NamedNode(n) if self.typing.is(n.as_str(), EntityKind::Class) => Some(Parsed {
                value: ClassExpression::Class(Class(n.clone())),
                triples: Vec::new(),
            }),
            Term::BlankNode(b) => {
                if let Some(m) = self.ce_memo.get(b) {
                    return m.clone();
                }
                if self.active.len() >= MAX_NESTING {
                    self.too_deep(b);
                    return None;
                }
                if !self.active.insert(b.clone()) {
                    return None;
                }
                let r = if self.check_shared(b) {
                    self.class_expression_node(b)
                } else {
                    None
                };
                self.active.remove(b);
                self.ce_memo.insert(b.clone(), r.clone());
                r
            }
            _ => None,
        }
    }

    fn class_expression_node(&mut self, b: &BlankNode) -> Option<Parsed<ClassExpression>> {
        let k: Key = b.clone().into();
        if !self.sp(&k, v::OWL_ON_PROPERTY).is_empty()
            || !self.sp(&k, v::OWL_ON_PROPERTIES).is_empty()
        {
            return self.restriction(b);
        }
        if self.has_type(&k, v::RDFS_DATATYPE).is_some()
            || self.has_type(&k, v::OWL_DATA_RANGE).is_some()
        {
            return None;
        }
        let typed = self.has_type(&k, v::OWL_CLASS);
        if typed.is_none() && self.strict {
            return None;
        }
        let mut triples: Vec<usize> = typed.into_iter().collect();
        let preds: Vec<&str> = [
            v::OWL_INTERSECTION_OF,
            v::OWL_UNION_OF,
            v::OWL_COMPLEMENT_OF,
            v::OWL_ONE_OF,
        ]
        .into_iter()
        .filter(|p| !self.sp(&k, p).is_empty())
        .collect();
        let value = match preds.as_slice() {
            [p @ (v::OWL_INTERSECTION_OF | v::OWL_UNION_OF)] => {
                let (i, l) = self.one(&k, p)?;
                let list = self.list(&l.clone())?;
                triples.push(i);
                triples.extend(list.triples);
                let mut set = BTreeSet::new();
                for item in &list.value {
                    let c = self.class_expression(item)?;
                    triples.extend(c.triples);
                    set.insert(c.value);
                }
                let intersection = *p == v::OWL_INTERSECTION_OF;
                match set.len() {
                    // Table 15: OWL 1 lists of zero or one member
                    0 if intersection => ClassExpression::Class(Class::thing()),
                    0 => ClassExpression::Class(Class::nothing()),
                    1 if list.value.len() == 1 => set.into_iter().next().unwrap(),
                    _ if intersection => ClassExpression::ObjectIntersectionOf(set),
                    _ => ClassExpression::ObjectUnionOf(set),
                }
            }
            [v::OWL_COMPLEMENT_OF] => {
                let (i, y) = self.one(&k, v::OWL_COMPLEMENT_OF)?;
                let c = self.class_expression(&y.clone())?;
                triples.push(i);
                triples.extend(c.triples);
                ClassExpression::ObjectComplementOf(Box::new(c.value))
            }
            [v::OWL_ONE_OF] => {
                let (i, l) = self.one(&k, v::OWL_ONE_OF)?;
                let list = self.list(&l.clone())?;
                triples.push(i);
                triples.extend(list.triples);
                let mut set = BTreeSet::new();
                for item in &list.value {
                    let ind = self.individual(item)?;
                    if matches!(ind, Individual::Anonymous(_)) && self.strict {
                        return None;
                    }
                    set.insert(ind);
                }
                if set.is_empty() {
                    ClassExpression::Class(Class::nothing())
                } else {
                    ClassExpression::ObjectOneOf(set)
                }
            }
            _ => return None,
        };
        if typed.is_none() {
            self.warn(
                "untyped",
                format!(
                    "the class expression _:{} has no rdf:type owl:Class",
                    b.as_str()
                ),
                triples.clone(),
            );
        }
        Some(Parsed { value, triples })
    }

    fn restriction(&mut self, b: &BlankNode) -> Option<Parsed<ClassExpression>> {
        let k: Key = b.clone().into();
        let typed = self.has_type(&k, v::OWL_RESTRICTION);
        if typed.is_none() && self.strict {
            return None;
        }
        let mut triples: Vec<usize> = typed.into_iter().collect();
        let fillers = [
            v::OWL_SOME_VALUES_FROM,
            v::OWL_ALL_VALUES_FROM,
            v::OWL_HAS_VALUE,
            v::OWL_HAS_SELF,
            v::OWL_MIN_CARDINALITY,
            v::OWL_MAX_CARDINALITY,
            v::OWL_CARDINALITY,
            v::OWL_MIN_QUALIFIED_CARDINALITY,
            v::OWL_MAX_QUALIFIED_CARDINALITY,
            v::OWL_QUALIFIED_CARDINALITY,
        ];
        let present: Vec<&str> = fillers
            .into_iter()
            .filter(|p| !self.sp(&k, p).is_empty())
            .collect();
        let [filler] = present.as_slice() else {
            return None;
        };
        let filler = *filler;
        let (fi, fv) = self.one(&k, filler)?;
        let fv = fv.clone();
        triples.push(fi);

        // n-ary data restrictions
        if let Some((pi, l)) = self.one(&k, v::OWL_ON_PROPERTIES) {
            if self.one(&k, v::OWL_ON_PROPERTY).is_some() {
                return None;
            }
            let list = self.list(&l.clone())?;
            triples.push(pi);
            triples.extend(list.triples);
            let mut props = Vec::new();
            for item in &list.value {
                props.push(self.dpe(item)?);
            }
            if props.is_empty() {
                return None;
            }
            let r = self.data_range(&fv)?;
            triples.extend(r.triples);
            let value = match filler {
                v::OWL_SOME_VALUES_FROM => ClassExpression::DataSomeValuesFrom(props, r.value),
                v::OWL_ALL_VALUES_FROM => ClassExpression::DataAllValuesFrom(props, r.value),
                _ => return None,
            };
            return Some(self.finish_restriction(b, typed, value, triples));
        }

        let (pi, p) = self.one(&k, v::OWL_ON_PROPERTY)?;
        let p = p.clone();
        triples.push(pi);
        let qualified = matches!(
            filler,
            v::OWL_MIN_QUALIFIED_CARDINALITY
                | v::OWL_MAX_QUALIFIED_CARDINALITY
                | v::OWL_QUALIFIED_CARDINALITY
        );
        let on_class = self.one(&k, v::OWL_ON_CLASS);
        let on_range = self.one(&k, v::OWL_ON_DATA_RANGE);
        if !qualified && (on_class.is_some() || on_range.is_some()) {
            return None;
        }

        if let Some(ope) = self.ope(&p) {
            triples.extend(ope.triples);
            let ope = ope.value;
            let value = match filler {
                v::OWL_SOME_VALUES_FROM | v::OWL_ALL_VALUES_FROM => {
                    let c = self.class_expression(&fv)?;
                    triples.extend(c.triples);
                    if filler == v::OWL_SOME_VALUES_FROM {
                        ClassExpression::ObjectSomeValuesFrom(ope, Box::new(c.value))
                    } else {
                        ClassExpression::ObjectAllValuesFrom(ope, Box::new(c.value))
                    }
                }
                v::OWL_HAS_VALUE => ClassExpression::ObjectHasValue(ope, self.individual(&fv)?),
                v::OWL_HAS_SELF => {
                    let Term::Literal(l) = &fv else { return None };
                    let ok = l.value() == "true"
                        && (l.datatype().as_str() == v::XSD_BOOLEAN || !self.strict)
                        || !self.strict && l.value() == "1";
                    if !ok {
                        return None;
                    }
                    ClassExpression::ObjectHasSelf(ope)
                }
                _ => {
                    let n = self.nn_int(&fv)?;
                    let q = if qualified {
                        let (ci, c) = on_class?;
                        let c = c.clone();
                        let ce = self.class_expression(&c)?;
                        triples.push(ci);
                        triples.extend(ce.triples);
                        Some(Box::new(ce.value))
                    } else {
                        None
                    };
                    match filler {
                        v::OWL_MIN_CARDINALITY | v::OWL_MIN_QUALIFIED_CARDINALITY => {
                            ClassExpression::ObjectMinCardinality(n, ope, q)
                        }
                        v::OWL_MAX_CARDINALITY | v::OWL_MAX_QUALIFIED_CARDINALITY => {
                            ClassExpression::ObjectMaxCardinality(n, ope, q)
                        }
                        _ => ClassExpression::ObjectExactCardinality(n, ope, q),
                    }
                }
            };
            return Some(self.finish_restriction(b, typed, value, triples));
        }

        let dp = self.dpe(&p)?;
        let value = match filler {
            v::OWL_SOME_VALUES_FROM | v::OWL_ALL_VALUES_FROM => {
                let r = self.data_range(&fv)?;
                triples.extend(r.triples);
                if filler == v::OWL_SOME_VALUES_FROM {
                    ClassExpression::DataSomeValuesFrom(vec![dp], r.value)
                } else {
                    ClassExpression::DataAllValuesFrom(vec![dp], r.value)
                }
            }
            v::OWL_HAS_VALUE => {
                let Term::Literal(l) = &fv else { return None };
                ClassExpression::DataHasValue(dp, Literal(l.clone()))
            }
            v::OWL_HAS_SELF => return None,
            _ => {
                let n = self.nn_int(&fv)?;
                let q = if qualified {
                    let (ri, r) = on_range?;
                    let r = r.clone();
                    let dr = self.data_range(&r)?;
                    triples.push(ri);
                    triples.extend(dr.triples);
                    Some(dr.value)
                } else {
                    None
                };
                match filler {
                    v::OWL_MIN_CARDINALITY | v::OWL_MIN_QUALIFIED_CARDINALITY => {
                        ClassExpression::DataMinCardinality(n, dp, q)
                    }
                    v::OWL_MAX_CARDINALITY | v::OWL_MAX_QUALIFIED_CARDINALITY => {
                        ClassExpression::DataMaxCardinality(n, dp, q)
                    }
                    _ => ClassExpression::DataExactCardinality(n, dp, q),
                }
            }
        };
        Some(self.finish_restriction(b, typed, value, triples))
    }

    fn finish_restriction(
        &mut self,
        b: &BlankNode,
        typed: Option<usize>,
        value: ClassExpression,
        triples: Vec<usize>,
    ) -> Parsed<ClassExpression> {
        if typed.is_none() {
            self.warn(
                "untyped",
                format!(
                    "the restriction _:{} has no rdf:type owl:Restriction",
                    b.as_str()
                ),
                triples.clone(),
            );
        }
        Parsed { value, triples }
    }

    // ------------------------------------------------------------------ axioms ------

    fn commit(
        &mut self,
        kind: AxiomKind,
        annotations: BTreeSet<Annotation>,
        triples: Vec<usize>,
        main: Option<usize>,
    ) {
        for i in &triples {
            self.used[*i] = true;
        }
        let graph = self.graph_of_triples(&triples);
        let mut triples = triples;
        triples.sort_unstable();
        triples.dedup();
        self.axioms
            .entry(Axiom { kind, annotations })
            .or_default()
            .push(Origin {
                graph,
                triples,
                main,
            });
    }

    /// The axiom whose main triple is `i` (Tables 16 and 18), with the triples of its
    /// expressions and side triples.
    fn axiom_of_main(&mut self, i: usize, annotated: bool) -> Option<(AxiomKind, Vec<usize>)> {
        let tr = &self.t[i];
        let s = term_of(&tr.subject);
        let o = tr.object.clone();
        let p = tr.predicate.as_str();
        let mut ts = vec![i];
        let ce2 =
            |this: &mut Self, ts: &mut Vec<usize>| -> Option<(ClassExpression, ClassExpression)> {
                let a = this.class_expression(&s)?;
                let b = this.class_expression(&o)?;
                ts.extend(a.triples);
                ts.extend(b.triples);
                Some((a.value, b.value))
            };
        let kind = match p {
            v::RDF_TYPE => return self.type_axiom(i),
            v::RDFS_SUB_CLASS_OF => {
                let (a, b) = ce2(self, &mut ts)?;
                AxiomKind::SubClassOf(a, b)
            }
            v::OWL_EQUIVALENT_CLASS => {
                if let Term::NamedNode(sn) = &s
                    && self.typing.is(sn.as_str(), EntityKind::Datatype)
                {
                    let r = self.data_range(&o)?;
                    ts.extend(r.triples);
                    AxiomKind::DatatypeDefinition(Datatype(sn.clone()), r.value)
                } else {
                    // `x owl:equivalentClass x` is EquivalentClasses(x x), whose
                    // operand set has one member
                    let (a, b) = ce2(self, &mut ts)?;
                    AxiomKind::EquivalentClasses([a, b].into_iter().collect())
                }
            }
            v::OWL_DISJOINT_WITH => {
                let (a, b) = ce2(self, &mut ts)?;
                if a == b {
                    return None;
                }
                AxiomKind::DisjointClasses([a, b].into_iter().collect())
            }
            v::OWL_DISJOINT_UNION_OF => {
                let Term::NamedNode(c) = &s else { return None };
                if !self.typing.is(c.as_str(), EntityKind::Class) {
                    return None;
                }
                let list = self.list(&o)?;
                ts.extend(list.triples);
                let mut set = BTreeSet::new();
                for item in &list.value {
                    let ce = self.class_expression(item)?;
                    ts.extend(ce.triples);
                    set.insert(ce.value);
                }
                if set.len() < 2 {
                    return None;
                }
                AxiomKind::DisjointUnion(Class(c.clone()), set)
            }
            v::RDFS_SUB_PROPERTY_OF => {
                if let (Some(a), Some(b)) = (self.ope(&s), self.ope(&o)) {
                    ts.extend(a.triples);
                    ts.extend(b.triples);
                    AxiomKind::SubObjectPropertyOf(
                        SubObjectPropertyExpression::Expression(a.value),
                        b.value,
                    )
                } else if let (Some(a), Some(b)) = (self.dpe(&s), self.dpe(&o)) {
                    AxiomKind::SubDataPropertyOf(a, b)
                } else if let (Term::NamedNode(a), Term::NamedNode(b)) = (&s, &o)
                    && self.is_ap(a.as_str())
                    && self.is_ap(b.as_str())
                {
                    AxiomKind::SubAnnotationPropertyOf(
                        AnnotationProperty(a.clone()),
                        AnnotationProperty(b.clone()),
                    )
                } else {
                    return None;
                }
            }
            v::OWL_PROPERTY_CHAIN_AXIOM => {
                let sup = self.ope(&s)?;
                ts.extend(sup.triples);
                let list = self.list(&o)?;
                ts.extend(list.triples);
                let mut chain = Vec::new();
                for item in &list.value {
                    let e = self.ope(item)?;
                    ts.extend(e.triples);
                    chain.push(e.value);
                }
                if chain.len() < 2 {
                    return None;
                }
                AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(chain), sup.value)
            }
            v::OWL_EQUIVALENT_PROPERTY | v::OWL_PROPERTY_DISJOINT_WITH => {
                let eq = p == v::OWL_EQUIVALENT_PROPERTY;
                if let (Some(a), Some(b)) = (self.ope(&s), self.ope(&o)) {
                    ts.extend(a.triples);
                    ts.extend(b.triples);
                    if !eq && a.value == b.value {
                        return None;
                    }
                    let set = [a.value, b.value].into_iter().collect();
                    if eq {
                        AxiomKind::EquivalentObjectProperties(set)
                    } else {
                        AxiomKind::DisjointObjectProperties(set)
                    }
                } else if let (Some(a), Some(b)) = (self.dpe(&s), self.dpe(&o)) {
                    if !eq && a == b {
                        return None;
                    }
                    let set = [a, b].into_iter().collect();
                    if eq {
                        AxiomKind::EquivalentDataProperties(set)
                    } else {
                        AxiomKind::DisjointDataProperties(set)
                    }
                } else {
                    return None;
                }
            }
            v::RDFS_DOMAIN | v::RDFS_RANGE => {
                let domain = p == v::RDFS_DOMAIN;
                if let Some(a) = self.ope(&s) {
                    let c = self.class_expression(&o)?;
                    ts.extend(a.triples);
                    ts.extend(c.triples);
                    if domain {
                        AxiomKind::ObjectPropertyDomain(a.value, c.value)
                    } else {
                        AxiomKind::ObjectPropertyRange(a.value, c.value)
                    }
                } else if let Some(a) = self.dpe(&s) {
                    if domain {
                        let c = self.class_expression(&o)?;
                        ts.extend(c.triples);
                        AxiomKind::DataPropertyDomain(a, c.value)
                    } else {
                        let r = self.data_range(&o)?;
                        ts.extend(r.triples);
                        AxiomKind::DataPropertyRange(a, r.value)
                    }
                } else if let (Term::NamedNode(a), Term::NamedNode(u)) = (&s, &o)
                    && self.is_ap(a.as_str())
                {
                    let ap = AnnotationProperty(a.clone());
                    if domain {
                        AxiomKind::AnnotationPropertyDomain(ap, u.clone())
                    } else {
                        AxiomKind::AnnotationPropertyRange(ap, u.clone())
                    }
                } else {
                    return None;
                }
            }
            v::OWL_INVERSE_OF => {
                // a blank subject is an inverse expression, read with its user
                if matches!(s, Term::BlankNode(_)) {
                    return None;
                }
                let a = self.ope(&s)?;
                let b = self.ope(&o)?;
                ts.extend(a.triples);
                ts.extend(b.triples);
                AxiomKind::InverseObjectProperties(a.value, b.value)
            }
            v::OWL_HAS_KEY => {
                let c = self.class_expression(&s)?;
                ts.extend(c.triples);
                let list = self.list(&o)?;
                ts.extend(list.triples);
                let (mut ops, mut dps) = (BTreeSet::new(), BTreeSet::new());
                for item in &list.value {
                    if let Some(e) = self.ope(item) {
                        ts.extend(e.triples);
                        ops.insert(e.value);
                    } else {
                        dps.insert(self.dpe(item)?);
                    }
                }
                if ops.is_empty() && dps.is_empty() {
                    return None;
                }
                AxiomKind::HasKey(c.value, ops, dps)
            }
            v::OWL_SAME_AS | v::OWL_DIFFERENT_FROM => {
                let a = self.individual(&s)?;
                let b = self.individual(&o)?;
                if p != v::OWL_SAME_AS && a == b {
                    return None;
                }
                let set = [a, b].into_iter().collect();
                if p == v::OWL_SAME_AS {
                    AxiomKind::SameIndividual(set)
                } else {
                    AxiomKind::DifferentIndividuals(set)
                }
            }
            v::OWL_COMPLEMENT_OF | v::OWL_UNION_OF | v::OWL_INTERSECTION_OF | v::OWL_ONE_OF => {
                // Table 18: OWL 1 class definitions on a named class
                let Term::NamedNode(c) = &s else { return None };
                if !self.typing.is(c.as_str(), EntityKind::Class) {
                    return None;
                }
                let e = match p {
                    v::OWL_COMPLEMENT_OF => {
                        let ce = self.class_expression(&o)?;
                        ts.extend(ce.triples);
                        ClassExpression::ObjectComplementOf(Box::new(ce.value))
                    }
                    v::OWL_ONE_OF => {
                        let list = self.list(&o)?;
                        ts.extend(list.triples);
                        let mut set = BTreeSet::new();
                        for item in &list.value {
                            set.insert(self.individual(item)?);
                        }
                        if set.is_empty() {
                            ClassExpression::Class(Class::nothing())
                        } else {
                            ClassExpression::ObjectOneOf(set)
                        }
                    }
                    _ => {
                        let list = self.list(&o)?;
                        ts.extend(list.triples);
                        let mut set = BTreeSet::new();
                        for item in &list.value {
                            let ce = self.class_expression(item)?;
                            ts.extend(ce.triples);
                            set.insert(ce.value);
                        }
                        let inter = p == v::OWL_INTERSECTION_OF;
                        match set.len() {
                            0 if inter => ClassExpression::Class(Class::thing()),
                            0 => ClassExpression::Class(Class::nothing()),
                            1 => set.into_iter().next().unwrap(),
                            _ if inter => ClassExpression::ObjectIntersectionOf(set),
                            _ => ClassExpression::ObjectUnionOf(set),
                        }
                    }
                };
                let cls = ClassExpression::Class(Class(c.clone()));
                if cls == e {
                    return None;
                }
                AxiomKind::EquivalentClasses([cls, e].into_iter().collect())
            }
            p if v::STRUCTURAL_PREDICATES.contains(&p) || v::FACETS.contains(&p) => return None,
            p => {
                if self.is_ap(p) {
                    // an annotated annotation assertion (plain ones come last)
                    if !annotated {
                        return None;
                    }
                    let subject = match &s {
                        Term::NamedNode(n) => AnnotationSubject::Iri(n.clone()),
                        Term::BlankNode(b) => {
                            AnnotationSubject::Anonymous(AnonymousIndividual::new(b.as_str()))
                        }
                        _ => return None,
                    };
                    return Some((
                        AxiomKind::AnnotationAssertion(
                            AnnotationProperty(iri(p)),
                            subject,
                            annotation_value(&o)?,
                        ),
                        ts,
                    ));
                }
                if self.typing.is(p, EntityKind::ObjectProperty) {
                    if matches!(o, Term::Literal(_)) {
                        return None;
                    }
                    AxiomKind::ObjectPropertyAssertion(
                        ObjectPropertyExpression::Property(ObjectProperty(iri(p))),
                        self.individual(&s)?,
                        self.individual(&o)?,
                    )
                } else if self.typing.is(p, EntityKind::DataProperty) {
                    let Term::Literal(l) = &o else { return None };
                    AxiomKind::DataPropertyAssertion(
                        DataProperty(iri(p)),
                        self.individual(&s)?,
                        Literal(l.clone()),
                    )
                } else {
                    return None;
                }
            }
        };
        Some((kind, ts))
    }

    /// The axiom of an `rdf:type` triple.
    fn type_axiom(&mut self, i: usize) -> Option<(AxiomKind, Vec<usize>)> {
        let tr = &self.t[i];
        let s = term_of(&tr.subject);
        let o = tr.object.clone();
        let mut ts = vec![i];
        if let Term::NamedNode(ty) = &o {
            if let (Some(k), Term::NamedNode(sn)) = (EntityKind::of_rdf_type(ty.as_str()), &s) {
                return Some((AxiomKind::Declaration(Entity::of_kind(k, sn.clone())), ts));
            }
            let ope_axiom = |e: ObjectPropertyExpression| -> Option<AxiomKind> {
                Some(match ty.as_str() {
                    v::OWL_FUNCTIONAL_PROPERTY => AxiomKind::FunctionalObjectProperty(e),
                    v::OWL_INVERSE_FUNCTIONAL_PROPERTY => {
                        AxiomKind::InverseFunctionalObjectProperty(e)
                    }
                    v::OWL_REFLEXIVE_PROPERTY => AxiomKind::ReflexiveObjectProperty(e),
                    v::OWL_IRREFLEXIVE_PROPERTY => AxiomKind::IrreflexiveObjectProperty(e),
                    v::OWL_SYMMETRIC_PROPERTY => AxiomKind::SymmetricObjectProperty(e),
                    v::OWL_ASYMMETRIC_PROPERTY => AxiomKind::AsymmetricObjectProperty(e),
                    v::OWL_TRANSITIVE_PROPERTY => AxiomKind::TransitiveObjectProperty(e),
                    _ => return None,
                })
            };
            match ty.as_str() {
                v::OWL_ONTOLOGY_PROPERTY => {
                    let Term::NamedNode(sn) = &s else { return None };
                    return Some((
                        AxiomKind::Declaration(Entity::AnnotationProperty(AnnotationProperty(
                            sn.clone(),
                        ))),
                        ts,
                    ));
                }
                v::OWL_FUNCTIONAL_PROPERTY
                | v::OWL_INVERSE_FUNCTIONAL_PROPERTY
                | v::OWL_REFLEXIVE_PROPERTY
                | v::OWL_IRREFLEXIVE_PROPERTY
                | v::OWL_SYMMETRIC_PROPERTY
                | v::OWL_ASYMMETRIC_PROPERTY
                | v::OWL_TRANSITIVE_PROPERTY => {
                    if let Some(e) = self.ope(&s) {
                        ts.extend(e.triples);
                        return ope_axiom(e.value).map(|k| (k, ts));
                    }
                    if ty.as_str() == v::OWL_FUNCTIONAL_PROPERTY
                        && let Some(d) = self.dpe(&s)
                    {
                        return Some((AxiomKind::FunctionalDataProperty(d), ts));
                    }
                    // an undeclared property: lenient mode reads it as an object property
                    if let Term::NamedNode(sn) = &s
                        && !self.strict
                        && self.typing.kinds(sn.as_str()).property().is_none()
                        && self.typing.infer(sn.as_str(), EntityKind::ObjectProperty)
                    {
                        self.warn(
                            "undeclared",
                            format!(
                                "<{}> is not declared and was read as ObjectProperty from its characteristic",
                                sn.as_str()
                            ),
                            vec![i],
                        );
                        let e = ObjectPropertyExpression::Property(ObjectProperty(sn.clone()));
                        return ope_axiom(e).map(|k| (k, ts));
                    }
                    return None;
                }
                v::OWL_DEPRECATED_CLASS | v::OWL_DEPRECATED_PROPERTY => {
                    let Term::NamedNode(sn) = &s else { return None };
                    return Some((
                        AxiomKind::AnnotationAssertion(
                            AnnotationProperty::new(v::OWL_DEPRECATED),
                            AnnotationSubject::Iri(sn.clone()),
                            AnnotationValue::Literal(Literal::typed("true", v::XSD_BOOLEAN)),
                        ),
                        ts,
                    ));
                }
                v::RDFS_CLASS => {
                    // OWL 1 RDFS-style class declarations, read as classes in lenient mode
                    let Term::NamedNode(sn) = &s else { return None };
                    if self.strict {
                        return None;
                    }
                    self.typing.infer(sn.as_str(), EntityKind::Class);
                    self.warn(
                        "undeclared",
                        format!(
                            "<{}> is typed rdfs:Class and was read as a class declaration",
                            sn.as_str()
                        ),
                        vec![i],
                    );
                    return Some((AxiomKind::Declaration(Entity::Class(Class(sn.clone()))), ts));
                }
                t if v::STRUCTURAL_TYPES.contains(&t) => return None,
                _ => {}
            }
        }
        // a class assertion; a structure node is not an individual
        if let Term::BlankNode(b) = &s
            && (self.rind.contains(b) || !self.sp(&b.clone().into(), v::RDF_FIRST).is_empty())
        {
            return None;
        }
        let c = self.class_expression(&o)?;
        ts.extend(c.triples);
        Some((AxiomKind::ClassAssertion(c.value, self.individual(&s)?), ts))
    }

    /// Table 17: axioms reified with `owl:Axiom`.
    fn annotated_axioms(&mut self) {
        let mut nodes: Vec<(usize, BlankNode)> = Vec::new();
        for i in self.pred(v::RDF_TYPE) {
            let tr = &self.t[i];
            if let (NamedOrBlankNode::BlankNode(b), Term::NamedNode(o)) = (&tr.subject, &tr.object)
                && o.as_str() == v::OWL_AXIOM
            {
                nodes.push((i, b.clone()));
            }
        }
        for (ti, x) in nodes {
            let xk: Key = x.clone().into();
            let (Some((si, s)), Some((pi, Term::NamedNode(p))), Some((oi, o))) = (
                self.one(&xk, v::OWL_ANNOTATED_SOURCE),
                self.one(&xk, v::OWL_ANNOTATED_PROPERTY),
                self.one(&xk, v::OWL_ANNOTATED_TARGET),
            ) else {
                continue;
            };
            let Some(sk) = key_of(s) else { continue };
            let main = self
                .about(&sk)
                .iter()
                .copied()
                .find(|j| self.t[*j].predicate == *p && self.t[*j].object == *o);
            let Some(m) = main else {
                self.warn(
                    "missing-main",
                    format!(
                        "the owl:Axiom node _:{} annotates a triple the graph does not hold",
                        x.as_str()
                    ),
                    vec![ti, si, pi, oi],
                );
                continue;
            };
            if self.used[m] && !self.annotated_main.contains(&m) {
                continue;
            }
            let Some((kind, mut ts)) = self.axiom_of_main(m, true) else {
                continue;
            };
            let (anns, ats) = self.take_annotations(&xk);
            ts.extend([ti, si, pi, oi]);
            ts.extend(ats);
            self.annotated_main.insert(m);
            ts.push(m);
            self.commit(kind, anns, ts, Some(m));
        }
    }

    /// The axioms represented by blank nodes: `owl:AllDisjointClasses`,
    /// `owl:AllDisjointProperties`, `owl:AllDifferent` and
    /// `owl:NegativePropertyAssertion`.
    fn blank_axioms(&mut self) {
        let mut nodes: Vec<(usize, BlankNode, String)> = Vec::new();
        for i in self.pred(v::RDF_TYPE) {
            let tr = &self.t[i];
            if let (NamedOrBlankNode::BlankNode(b), Term::NamedNode(o)) = (&tr.subject, &tr.object)
                && matches!(
                    o.as_str(),
                    v::OWL_ALL_DISJOINT_CLASSES
                        | v::OWL_ALL_DISJOINT_PROPERTIES
                        | v::OWL_ALL_DIFFERENT
                        | v::OWL_NEGATIVE_PROPERTY_ASSERTION
                )
            {
                nodes.push((i, b.clone(), o.as_str().to_string()));
            }
        }
        for (ti, x, ty) in nodes {
            if self.used[ti] {
                continue;
            }
            let xk: Key = x.clone().into();
            let parsed = if ty == v::OWL_NEGATIVE_PROPERTY_ASSERTION {
                self.negative_assertion(&xk)
            } else {
                self.members_axiom(&xk, &ty)
            };
            let Some((kind, mut ts)) = parsed else {
                continue;
            };
            ts.push(ti);
            let (anns, ats) = self.take_annotations(&xk);
            ts.extend(ats);
            self.commit(kind, anns, ts, None);
        }
    }

    fn members_axiom(&mut self, x: &Key, ty: &str) -> Option<(AxiomKind, Vec<usize>)> {
        let (mi, l) = match self.one(x, v::OWL_MEMBERS) {
            Some(m) => m,
            None if ty == v::OWL_ALL_DIFFERENT => self.one(x, v::OWL_DISTINCT_MEMBERS)?,
            None => return None,
        };
        let list = self.list(&l.clone())?;
        if list.value.len() < 2 {
            return None;
        }
        let mut ts = vec![mi];
        ts.extend(list.triples);
        let kind = match ty {
            v::OWL_ALL_DISJOINT_CLASSES => {
                let mut set = BTreeSet::new();
                for item in &list.value {
                    let c = self.class_expression(item)?;
                    ts.extend(c.triples);
                    set.insert(c.value);
                }
                AxiomKind::DisjointClasses(set)
            }
            v::OWL_ALL_DISJOINT_PROPERTIES => {
                let mut ops = BTreeSet::new();
                let mut opts = Vec::new();
                for item in &list.value {
                    match self.ope(item) {
                        Some(e) => {
                            opts.extend(e.triples);
                            ops.insert(e.value);
                        }
                        None => break,
                    }
                }
                if ops.len() == list.value.len() {
                    ts.extend(opts);
                    AxiomKind::DisjointObjectProperties(ops)
                } else {
                    let mut dps = BTreeSet::new();
                    for item in &list.value {
                        dps.insert(self.dpe(item)?);
                    }
                    AxiomKind::DisjointDataProperties(dps)
                }
            }
            _ => {
                let mut set = BTreeSet::new();
                for item in &list.value {
                    set.insert(self.individual(item)?);
                }
                AxiomKind::DifferentIndividuals(set)
            }
        };
        Some((kind, ts))
    }

    fn negative_assertion(&mut self, x: &Key) -> Option<(AxiomKind, Vec<usize>)> {
        let (si, s) = self.one(x, v::OWL_SOURCE_INDIVIDUAL)?;
        let (pi, p) = self.one(x, v::OWL_ASSERTION_PROPERTY)?;
        let (s, p) = (s.clone(), p.clone());
        let source = self.individual(&s)?;
        let mut ts = vec![si, pi];
        if let Some((oi, o)) = self.one(x, v::OWL_TARGET_INDIVIDUAL) {
            if self.one(x, v::OWL_TARGET_VALUE).is_some() {
                return None;
            }
            let o = o.clone();
            let e = self.ope(&p)?;
            ts.push(oi);
            ts.extend(e.triples);
            let target = self.individual(&o)?;
            return Some((
                AxiomKind::NegativeObjectPropertyAssertion(e.value, source, target),
                ts,
            ));
        }
        let (oi, o) = self.one(x, v::OWL_TARGET_VALUE)?;
        let Term::Literal(l) = o else { return None };
        let l = l.clone();
        let d = self.dpe(&p)?;
        ts.push(oi);
        Some((
            AxiomKind::NegativeDataPropertyAssertion(d, source, Literal(l)),
            ts,
        ))
    }

    /// Table 16 for the triples left.
    fn plain_axioms(&mut self) {
        for i in 0..self.t.len() {
            if self.used[i] {
                continue;
            }
            let tr = &self.t[i];
            if let NamedOrBlankNode::BlankNode(b) = &tr.subject
                && self.rind.contains(b)
            {
                continue;
            }
            // annotation triples become annotation assertions last
            if self.is_ap(tr.predicate.as_str()) {
                continue;
            }
            if let Some((kind, ts)) = self.axiom_of_main(i, false) {
                self.commit(kind, BTreeSet::new(), ts, Some(i));
            }
        }
    }

    /// The annotation assertions of the nodes outside RIND (Table 17's last step).
    fn annotation_assertions(&mut self) {
        let mut subjects: Vec<Key> = self.ann_triples.keys().cloned().collect();
        subjects.sort_by_key(|k| k.to_string());
        for x in subjects {
            if self.onodes.contains(&x) {
                continue;
            }
            if let NamedOrBlankNode::BlankNode(b) = &x
                && self.rind.contains(b)
            {
                continue;
            }
            let subject = match &x {
                NamedOrBlankNode::NamedNode(n) => AnnotationSubject::Iri(n.clone()),
                NamedOrBlankNode::BlankNode(b) => {
                    AnnotationSubject::Anonymous(AnonymousIndividual::new(b.as_str()))
                }
            };
            for a in self.annotations_of(&x, 0) {
                let Annotation {
                    property,
                    value,
                    annotations,
                } = a.value;
                let main = a.triples.first().copied();
                self.commit(
                    AxiomKind::AnnotationAssertion(property, subject.clone(), value),
                    annotations,
                    a.triples,
                    main,
                );
            }
        }
    }

    fn header_annotations(&mut self) {
        for n in 0..self.headers.len() {
            let node = self.headers[n].1.clone();
            let (anns, ts) = self.take_annotations(&node);
            for i in ts {
                self.used[i] = true;
            }
            self.headers[n].2.annotations = anns;
        }
    }
}

fn mentions(t: &Triple, iri: &str) -> bool {
    let n = |x: &NamedNode| x.as_str() == iri;
    matches!(&t.subject, NamedOrBlankNode::NamedNode(s) if n(s))
        || n(&t.predicate)
        || matches!(&t.object, Term::NamedNode(o) if n(o))
}
