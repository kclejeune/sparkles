//! RDFS on read: queries answered over the RDFS closure of each graph with respect to a
//! fixed schema, computed at query time instead of materialized (Jena's
//! `DatasetGraphRDFS`, Fuseki's `--rdfs FILE` and the `ja:DatasetRDFS` assembler).
//!
//! The semantics are those of Jena's `MatchRDFS`. The schema alone defines the
//! vocabulary: `rdfs:subClassOf` and `rdfs:subPropertyOf` are closed transitively over
//! it, and `rdfs:domain` and `rdfs:range` apply to the property that declares them (a
//! subproperty does not inherit them). Data triples with these predicates are matched
//! as plain triples, and the schema's own triples are not added to the data. Jena
//! answers a triple pattern by its shape (which positions are constants), and so does
//! this module:
//!
//! | pattern | answers |
//! |---|---|
//! | `s P o`, `P` not one of the three below | stored `s P o`, and `s Q o` for each subproperty `Q` of `P` |
//! | `s rdfs:subClassOf o`, `s rdfs:subPropertyOf o` | stored triples |
//! | `s rdf:type o`, `s` or `o` a constant | stored types, domains of the properties of `s`, ranges of the properties pointing at `s` (literals included), and their superclasses |
//! | `?s rdf:type ?o` | the same, without range types for literals, plus `s Q o` for a `Q` below `rdf:type` (when the schema has a class hierarchy) |
//! | `s ?p o`, `s` a constant | the triples of `s` and what one rule step derives from them, superproperties only when the schema has a class hierarchy |
//! | `?s ?p o`, `o` a constant | stored triples of `o`, its instances as for `rdf:type`, and the superproperties of every predicate |
//! | `?s ?p ?o` | everything above |
//!
//! A schema with no `rdfs:subClassOf`, `rdfs:domain` or `rdfs:range` triple answers type
//! patterns from stored triples alone, as Jena's does.
//!
//! [`rewrite`] turns the query algebra into one over stored triples: each triple pattern
//! whose answers can change becomes a union of patterns with the schema's terms as
//! constants, under `DISTINCT` on the pattern's variables (an RDF graph is a set; Jena
//! may repeat a derived triple). Patterns that cannot change stay in their basic graph
//! pattern. The rewrite is graph-agnostic, so it applies in every graph the pattern is
//! matched in, and each graph is closed on its own, as in Jena, where every graph of the
//! dataset is wrapped with the schema.

use super::ctx::Ctx;
use crate::error::Result;
use crate::id::Id;
use crate::index::Perm;
use crate::store::Snapshot;
use oxrdf::vocab::{rdf, rdfs};
use oxrdf::{NamedNode, NamedNodeRef, Term, Triple, Variable};
use parking_lot::Mutex;
use spargebra::algebra::{
    AggregateExpression, Expression, Function, GraphPattern, OrderExpression,
    PropertyPathExpression,
};
use spargebra::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};
use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

type Set = BTreeSet<NamedNode>;
type Map = BTreeMap<NamedNode, Set>;

/// The vocabulary of an RDFS schema, as RDFS on read uses it (Jena's `SetupRDFS`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RdfsSchema {
    /// class → its strict superclasses (itself only through a cycle)
    sup_class: Map,
    /// class → its strict subclasses
    sub_class: Map,
    sup_prop: Map,
    sub_prop: Map,
    /// property → the classes of its `rdfs:domain` triples
    domain: Map,
    range: Map,
    /// schema triples with one of the four predicates that were read
    pub triples: usize,
    /// such triples left out because a term is not an IRI (a blank node class cannot be
    /// a constant of the rewritten query)
    pub skipped: usize,
}

impl RdfsSchema {
    /// The schema of these triples: their `rdfs:subClassOf`, `rdfs:subPropertyOf`,
    /// `rdfs:domain` and `rdfs:range` statements between IRIs.
    pub fn from_triples<'a>(triples: impl IntoIterator<Item = &'a Triple>) -> RdfsSchema {
        let mut s = RdfsSchema::default();
        let (mut sc, mut sp) = (Map::new(), Map::new());
        for t in triples {
            let p = t.predicate.as_ref();
            let edges = if p == rdfs::SUB_CLASS_OF {
                &mut sc
            } else if p == rdfs::SUB_PROPERTY_OF {
                &mut sp
            } else if p == rdfs::DOMAIN {
                &mut s.domain
            } else if p == rdfs::RANGE {
                &mut s.range
            } else {
                continue;
            };
            let (oxrdf::NamedOrBlankNode::NamedNode(a), Term::NamedNode(b)) =
                (&t.subject, &t.object)
            else {
                s.skipped += 1;
                continue;
            };
            if edges.entry(a.clone()).or_default().insert(b.clone()) {
                s.triples += 1;
            }
        }
        (s.sup_class, s.sub_class) = closure(&sc);
        (s.sup_prop, s.sub_prop) = closure(&sp);
        s
    }

    /// Whether the schema changes no answer.
    pub fn is_empty(&self) -> bool {
        self.sub_class.is_empty()
            && self.sub_prop.is_empty()
            && self.domain.is_empty()
            && self.range.is_empty()
    }

    /// Classes with a superclass, properties with a superproperty, properties with a
    /// domain, and properties with a range.
    pub fn counts(&self) -> [usize; 4] {
        [
            self.sup_class.len(),
            self.sup_prop.len(),
            self.domain.len(),
            self.range.len(),
        ]
    }

    /// The schema's triples, closed: a subclass and subproperty triple for each pair of
    /// the closure, and the domain and range triples.
    pub fn triples(&self) -> Vec<Triple> {
        let mut out = Vec::new();
        for (m, p) in [
            (&self.sup_class, rdfs::SUB_CLASS_OF),
            (&self.sup_prop, rdfs::SUB_PROPERTY_OF),
            (&self.domain, rdfs::DOMAIN),
            (&self.range, rdfs::RANGE),
        ] {
            for [a, b] in pairs(m) {
                out.push(Triple::new(a, p.into_owned(), b));
            }
        }
        out
    }

    /// Jena's `hasClassDeclarations`: some `rdfs:subClassOf`.
    fn has_classes(&self) -> bool {
        !self.sub_class.is_empty()
    }

    /// Jena's `hasOnlyPropertyDeclarations`: no class hierarchy, domain or range.
    fn only_properties(&self) -> bool {
        self.sub_class.is_empty() && self.domain.is_empty() && self.range.is_empty()
    }

    fn get<'m>(m: &'m Map, k: NamedNodeRef<'_>) -> Option<&'m Set> {
        m.get(&k.into_owned()).filter(|s| !s.is_empty())
    }

    /// The classes `c` and its superclasses.
    fn sup_inc<'s>(&'s self, c: &'s NamedNode) -> impl Iterator<Item = &'s NamedNode> {
        std::iter::once(c).chain(Self::get(&self.sup_class, c.as_ref()).into_iter().flatten())
    }

    /// The classes a domain or range set gives a resource: the classes and their
    /// superclasses.
    fn closed(&self, classes: &Set) -> Set {
        classes
            .iter()
            .flat_map(|c| self.sup_inc(c))
            .cloned()
            .collect()
    }

    /// (class, strict superclass) pairs.
    fn class_pairs(&self) -> Vec<[NamedNode; 2]> {
        pairs(&self.sup_class)
    }

    /// (property, strict superproperty) pairs.
    fn property_pairs(&self) -> Vec<[NamedNode; 2]> {
        pairs(&self.sup_prop)
    }
}

fn pairs(m: &Map) -> Vec<[NamedNode; 2]> {
    m.iter()
        .flat_map(|(a, bs)| bs.iter().map(move |b| [a.clone(), b.clone()]))
        .collect()
}

/// The transitive closure of direct edges `a → b` as (strict successors, strict
/// predecessors): the nodes reachable in one or more steps (Jena's `Transitive`).
fn closure(edges: &Map) -> (Map, Map) {
    let mut up = Map::new();
    let mut down = Map::new();
    for start in edges.keys() {
        let mut seen = Set::new();
        let mut stack: Vec<&NamedNode> = edges[start].iter().collect();
        while let Some(n) = stack.pop() {
            if seen.insert(n.clone())
                && let Some(next) = edges.get(n)
            {
                stack.extend(next);
            }
        }
        for n in &seen {
            down.entry(n.clone()).or_default().insert(start.clone());
        }
        up.insert(start.clone(), seen);
    }
    (up, down)
}

/// Where RDFS on read takes its schema from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaSource {
    /// a schema read once, from a file or a request body
    Fixed(Arc<RdfsSchema>),
    /// a graph of the dataset (`None`: the default graph), read in the state each query
    /// sees
    Graph(Option<String>),
}

/// A snapshot's state: its generation, delta version and commit.
type StateKey = (usize, u64, u64);

/// RDFS on read for a dataset: the schema's source, and the schema of a graph source as
/// last read.
#[derive(Debug)]
pub struct RdfsOnRead {
    pub source: SchemaSource,
    /// the snapshot last read, and its schema
    last: Mutex<Option<(StateKey, Arc<RdfsSchema>)>>,
}

impl RdfsOnRead {
    pub fn new(source: SchemaSource) -> RdfsOnRead {
        RdfsOnRead {
            source,
            last: Mutex::new(None),
        }
    }

    /// A fixed schema.
    pub fn fixed(schema: RdfsSchema) -> RdfsOnRead {
        RdfsOnRead::new(SchemaSource::Fixed(Arc::new(schema)))
    }

    /// The schema a query of `snap` uses. A graph source is read again when the snapshot
    /// is another state than the one read last.
    pub fn schema(&self, snap: &Snapshot) -> Result<Arc<RdfsSchema>> {
        let graph = match &self.source {
            SchemaSource::Fixed(s) => return Ok(s.clone()),
            SchemaSource::Graph(g) => g,
        };
        let key = (
            Arc::as_ptr(&snap.generation) as usize,
            snap.version,
            snap.commit,
        );
        if let Some((k, s)) = &*self.last.lock()
            && *k == key
        {
            return Ok(s.clone());
        }
        let s = Arc::new(schema_of_graph(snap, graph.as_deref())?);
        *self.last.lock() = Some((key, s.clone()));
        Ok(s)
    }
}

/// The schema in graph `graph` (`None`: the default graph) of a snapshot.
pub fn schema_of_graph(snap: &Snapshot, graph: Option<&str>) -> Result<RdfsSchema> {
    let g = match graph {
        None => Id::DEFAULT_GRAPH,
        Some(iri) => match snap.lookup_iri(iri) {
            Some(g) => g,
            None => return Ok(RdfsSchema::default()),
        },
    };
    let mut triples = Vec::new();
    for p in [
        rdfs::SUB_CLASS_OF,
        rdfs::SUB_PROPERTY_OF,
        rdfs::DOMAIN,
        rdfs::RANGE,
    ] {
        let Some(pid) = snap.lookup_iri(p.as_str()) else {
            continue;
        };
        for k in snap.scan_keys(Perm::Pos, &[pid.0])? {
            let q = Perm::Pos.to_quad(&k);
            if q[3] != g {
                continue;
            }
            let (Some(s), Some(o)) = (snap.term(q[0]), snap.term(q[2])) else {
                continue;
            };
            let s = match s {
                Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
                Term::BlankNode(b) => oxrdf::NamedOrBlankNode::BlankNode(b),
                _ => continue,
            };
            triples.push(Triple::new(s, p.into_owned(), o));
        }
    }
    Ok(RdfsSchema::from_triples(&triples))
}

/// The query pattern with RDFS on read applied, when the context has a schema.
pub(crate) fn apply<'a>(ctx: &Ctx, gp: &'a GraphPattern) -> Cow<'a, GraphPattern> {
    match ctx.rdfs.as_deref() {
        Some(s) if !s.is_empty() => Cow::Owned(rewrite(gp, s)),
        _ => Cow::Borrowed(gp),
    }
}

/// `gp` over stored triples, answering as RDFS on read with `schema` answers.
pub fn rewrite(gp: &GraphPattern, schema: &RdfsSchema) -> GraphPattern {
    let mut g = gp.clone();
    if schema.is_empty() {
        return g;
    }
    let rw = Rewriter {
        s: schema,
        n: Cell::new(0),
    };
    // Blank nodes of the patterns become variables, so that the parts a pattern is split
    // into still share them. Labels are scoped to one basic graph pattern, and the
    // projection of the query was fixed when it was parsed, so none becomes visible.
    blank_to_var(&mut g);
    rw.walk(&mut g);
    g
}

const TYPE: NamedNodeRef<'static> = rdf::TYPE;

struct Rewriter<'a> {
    s: &'a RdfsSchema,
    n: Cell<u32>,
}

/// A table of schema terms joined with a pattern.
enum Table {
    /// no row matches
    Empty,
    /// every row matches and binds nothing
    Unit,
    Values(GraphPattern),
}

impl Rewriter<'_> {
    fn fresh(&self) -> Variable {
        let n = self.n.get();
        self.n.set(n + 1);
        Variable::new_unchecked(format!("__rdfs{n}"))
    }

    fn walk(&self, gp: &mut GraphPattern) {
        use GraphPattern as G;
        match gp {
            G::Bgp { patterns } => {
                if let Some(new) = self.bgp(patterns) {
                    *gp = new;
                }
            }
            G::Path {
                subject,
                path,
                object,
            } => {
                if self.path_changes(path, true) {
                    *gp = self.path(subject, path, object);
                }
            }
            G::Join { left, right } | G::Union { left, right } | G::Minus { left, right } => {
                self.walk(left);
                self.walk(right);
            }
            G::LeftJoin {
                left,
                right,
                expression,
            } => {
                self.walk(left);
                self.walk(right);
                if let Some(e) = expression {
                    self.expr(e);
                }
            }
            G::Filter { expr, inner } => {
                self.expr(expr);
                self.walk(inner);
            }
            G::Extend {
                inner, expression, ..
            } => {
                self.expr(expression);
                self.walk(inner);
            }
            G::Graph { inner, .. }
            | G::Project { inner, .. }
            | G::Distinct { inner }
            | G::Reduced { inner }
            | G::Slice { inner, .. } => self.walk(inner),
            G::OrderBy { inner, expression } => {
                for o in expression {
                    match o {
                        OrderExpression::Asc(e) | OrderExpression::Desc(e) => self.expr(e),
                    }
                }
                self.walk(inner);
            }
            G::Group {
                inner, aggregates, ..
            } => {
                for (_, a) in aggregates {
                    if let AggregateExpression::FunctionCall { expr, .. } = a {
                        self.expr(expr);
                    }
                }
                self.walk(inner);
            }
            // the remote endpoint answers SERVICE; tables hold no patterns
            G::Service { .. } | G::Values { .. } => {}
        }
    }

    fn expr(&self, e: &mut Expression) {
        use Expression as E;
        match e {
            E::Exists(p) => self.walk(p),
            E::Or(a, b)
            | E::And(a, b)
            | E::Equal(a, b)
            | E::SameTerm(a, b)
            | E::Greater(a, b)
            | E::GreaterOrEqual(a, b)
            | E::Less(a, b)
            | E::LessOrEqual(a, b)
            | E::Add(a, b)
            | E::Subtract(a, b)
            | E::Multiply(a, b)
            | E::Divide(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            E::In(a, l) => {
                self.expr(a);
                l.iter_mut().for_each(|x| self.expr(x));
            }
            E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => self.expr(a),
            E::If(a, b, c) => {
                self.expr(a);
                self.expr(b);
                self.expr(c);
            }
            E::Coalesce(l) | E::FunctionCall(_, l) => l.iter_mut().for_each(|x| self.expr(x)),
            E::NamedNode(_) | E::Literal(_) | E::Variable(_) | E::Bound(_) => {}
        }
    }

    /// A basic graph pattern with its changed patterns replaced, or `None` when none
    /// changes.
    fn bgp(&self, patterns: &[TriplePattern]) -> Option<GraphPattern> {
        let mut kept = Vec::new();
        let mut parts = Vec::new();
        for t in patterns {
            match self.triple(&t.subject, &t.predicate, &t.object) {
                Some(p) => parts.push(p),
                None => kept.push(t.clone()),
            }
        }
        if parts.is_empty() {
            return None;
        }
        let mut out = (!kept.is_empty()).then_some(GraphPattern::Bgp { patterns: kept });
        for p in parts {
            out = Some(match out {
                None => p,
                Some(l) => join(l, p),
            });
        }
        out
    }

    /// The answers to one triple pattern, or `None` when they are the stored triples.
    fn triple(
        &self,
        s: &TermPattern,
        p: &NamedNodePattern,
        o: &TermPattern,
    ) -> Option<GraphPattern> {
        let changes = match p {
            NamedNodePattern::NamedNode(p) if p.as_ref() == TYPE => !self.s.only_properties(),
            NamedNodePattern::NamedNode(p)
                if p.as_ref() == rdfs::SUB_CLASS_OF || p.as_ref() == rdfs::SUB_PROPERTY_OF =>
            {
                false
            }
            NamedNodePattern::NamedNode(p) => {
                RdfsSchema::get(&self.s.sub_prop, p.as_ref()).is_some()
            }
            NamedNodePattern::Variable(_) => true,
        };
        if !changes {
            return None;
        }
        // a variable repeated within the pattern: the later occurrences become fresh
        // variables, equal to the first
        let mut vars: Vec<Variable> = Vec::new();
        let mut eqs: Vec<(Variable, Variable)> = Vec::new();
        let mut distinct = |t: &TermPattern| -> TermPattern {
            match t {
                TermPattern::Variable(v) if vars.contains(v) => {
                    let f = self.fresh();
                    eqs.push((v.clone(), f.clone()));
                    TermPattern::Variable(f)
                }
                TermPattern::Variable(v) => {
                    vars.push(v.clone());
                    t.clone()
                }
                _ => {
                    collect_vars(t, &mut vars);
                    t.clone()
                }
            }
        };
        let s = distinct(s);
        let p = match p {
            NamedNodePattern::Variable(v) => match distinct(&TermPattern::Variable(v.clone())) {
                TermPattern::Variable(v) => NamedNodePattern::Variable(v),
                _ => unreachable!(),
            },
            p => p.clone(),
        };
        let o = distinct(o);
        let branches = match &p {
            NamedNodePattern::NamedNode(pn) if pn.as_ref() == TYPE => {
                if is_var(&s) && is_var(&o) {
                    self.types(&s, &o, true, false, true)
                } else {
                    self.types(&s, &o, true, true, false)
                }
            }
            NamedNodePattern::NamedNode(pn) => {
                let mut b = vec![tp(&s, pn, &o)];
                for q in RdfsSchema::get(&self.s.sub_prop, pn.as_ref())
                    .into_iter()
                    .flatten()
                {
                    b.push(tp(&s, q, &o));
                }
                b
            }
            NamedNodePattern::Variable(pv) => self.any_predicate(&s, pv, &o),
        };
        let mut all = vars.clone();
        all.extend(eqs.iter().map(|(_, f)| f.clone()));
        let mut g = GraphPattern::Distinct {
            inner: Box::new(GraphPattern::Project {
                inner: Box::new(union(branches)),
                variables: all,
            }),
        };
        if !eqs.is_empty() {
            let mut cond = None;
            for (a, b) in eqs {
                let e = Expression::SameTerm(
                    Box::new(Expression::Variable(a)),
                    Box::new(Expression::Variable(b)),
                );
                cond = Some(match cond {
                    None => e,
                    Some(c) => Expression::And(Box::new(c), Box::new(e)),
                });
            }
            g = GraphPattern::Project {
                inner: Box::new(GraphPattern::Filter {
                    expr: cond.expect("a repeated variable"),
                    inner: Box::new(g),
                }),
                variables: vars,
            };
        }
        Some(g)
    }

    /// Answers of `s rdf:type o`: the stored types (with `stored`), the superclasses of
    /// stored types, the domains and ranges of the properties of `s` and their
    /// superclasses (a literal gets range types only with `literal_range`), and with
    /// `subproperties`, `s Q o` for the properties `Q` below `rdf:type`.
    fn types(
        &self,
        s: &TermPattern,
        o: &TermPattern,
        stored: bool,
        literal_range: bool,
        subproperties: bool,
    ) -> Vec<GraphPattern> {
        let ty = TYPE.into_owned();
        let mut b = Vec::new();
        if stored {
            b.push(tp(s, &ty, o));
        }
        if self.s.only_properties() {
            return b;
        }
        // superclasses of stored types
        match o {
            TermPattern::NamedNode(t) => {
                for c in RdfsSchema::get(&self.s.sub_class, t.as_ref())
                    .into_iter()
                    .flatten()
                {
                    b.push(tp(s, &ty, &TermPattern::NamedNode(c.clone())));
                }
            }
            TermPattern::Variable(_) => {
                let c = TermPattern::Variable(self.fresh());
                let rows = self.s.class_pairs();
                if let Some(g) =
                    with_table(tp(s, &ty, &c), &[&c, o], rows.into_iter().map(Vec::from))
                {
                    b.push(g);
                }
            }
            _ => {}
        }
        // domains: s p ?y
        for (p, classes) in &self.s.domain {
            let y = TermPattern::Variable(self.fresh());
            let k = self.s.closed(classes);
            if let Some(g) = with_table(tp(s, p, &y), &[o], k.into_iter().map(|c| vec![c])) {
                b.push(g);
            }
        }
        // ranges: ?y p s
        let literal_s = matches!(s, TermPattern::Literal(_));
        if literal_range || !literal_s {
            for (p, classes) in &self.s.range {
                let y = TermPattern::Variable(self.fresh());
                let k = self.s.closed(classes);
                let mut pat = tp(&y, p, s);
                if let (false, TermPattern::Variable(v)) = (literal_range, s) {
                    pat = GraphPattern::Filter {
                        expr: Expression::Not(Box::new(Expression::FunctionCall(
                            Function::IsLiteral,
                            vec![Expression::Variable(v.clone())],
                        ))),
                        inner: Box::new(pat),
                    };
                }
                if let Some(g) = with_table(pat, &[o], k.into_iter().map(|c| vec![c])) {
                    b.push(g);
                }
            }
        }
        if subproperties && self.s.has_classes() {
            for (q, sups) in &self.s.sup_prop {
                if sups.contains(&ty) {
                    b.push(tp(s, q, o));
                }
            }
        }
        b
    }

    /// Answers of `s ?p o`, by Jena's shapes: a constant subject, a constant object, or
    /// neither.
    fn any_predicate(&self, s: &TermPattern, pv: &Variable, o: &TermPattern) -> Vec<GraphPattern> {
        let p = TermPattern::Variable(pv.clone());
        let mut b = vec![GraphPattern::Bgp {
            patterns: vec![TriplePattern {
                subject: s.clone(),
                predicate: NamedNodePattern::Variable(pv.clone()),
                object: o.clone(),
            }],
        }];
        let subject_bound = !is_var(s);
        // superproperties of stored predicates (from a constant subject, Jena applies
        // them only when the schema has a class hierarchy)
        if !subject_bound || self.s.has_classes() {
            let q = self.fresh();
            let pat = GraphPattern::Bgp {
                patterns: vec![TriplePattern {
                    subject: s.clone(),
                    predicate: NamedNodePattern::Variable(q.clone()),
                    object: o.clone(),
                }],
            };
            let rows = self.s.property_pairs();
            if let Some(g) = with_table(
                pat,
                &[&TermPattern::Variable(q), &p],
                rows.into_iter().map(Vec::from),
            ) {
                b.push(g);
            }
        }
        // derived types, under rdf:type (and, unless the subject is a constant, its
        // superproperties)
        let (literal_range, subproperties) = if subject_bound {
            (false, false)
        } else if is_var(o) {
            (false, true)
        } else {
            (true, false)
        };
        let types = self.types(s, o, false, literal_range, subproperties);
        let mut preds = vec![TYPE.into_owned()];
        if !subject_bound {
            preds.extend(
                RdfsSchema::get(&self.s.sup_prop, TYPE)
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
        }
        for t in types {
            let g = if preds.len() == 1 {
                GraphPattern::Extend {
                    inner: Box::new(t),
                    variable: pv.clone(),
                    expression: Expression::NamedNode(preds[0].clone()),
                }
            } else {
                match with_table(t, &[&p], preds.iter().map(|x| vec![x.clone()])) {
                    Some(g) => g,
                    None => continue,
                }
            };
            b.push(g);
        }
        b
    }

    /// Whether a path's answers can change: at the top (`top`), through its links and
    /// negated property sets; inside `*`, `+` and `?`, through links with subproperties.
    fn path_changes(&self, path: &PropertyPathExpression, top: bool) -> bool {
        use PropertyPathExpression as P;
        match path {
            P::NamedNode(p) => {
                if p.as_ref() == rdfs::SUB_CLASS_OF || p.as_ref() == rdfs::SUB_PROPERTY_OF {
                    false
                } else if p.as_ref() == TYPE {
                    top && !self.s.only_properties()
                } else {
                    RdfsSchema::get(&self.s.sub_prop, p.as_ref()).is_some()
                }
            }
            P::NegatedPropertySet(_) => top,
            P::Reverse(x) => self.path_changes(x, top),
            P::Sequence(a, b) | P::Alternative(a, b) => {
                self.path_changes(a, top) || self.path_changes(b, top)
            }
            P::ZeroOrMore(x) | P::OneOrMore(x) | P::ZeroOrOne(x) => self.path_changes(x, false),
        }
    }

    /// A path pattern over stored triples. Sequences, alternatives and inverses are split
    /// into triple patterns; inside `*`, `+` and `?`, a link becomes the alternative of
    /// it and its subproperties.
    fn path(
        &self,
        s: &TermPattern,
        path: &PropertyPathExpression,
        o: &TermPattern,
    ) -> GraphPattern {
        use PropertyPathExpression as P;
        match path {
            P::NamedNode(p) => {
                let pred = NamedNodePattern::NamedNode(p.clone());
                self.triple(s, &pred, o).unwrap_or_else(|| tp(s, p, o))
            }
            P::Reverse(x) => self.path(o, x, s),
            P::Sequence(a, b) => {
                let v = TermPattern::Variable(self.fresh());
                let mut vars = Vec::new();
                collect_vars(s, &mut vars);
                collect_vars(o, &mut vars);
                vars.dedup();
                GraphPattern::Project {
                    inner: Box::new(join(self.path(s, a, &v), self.path(&v, b, o))),
                    variables: dedup(vars),
                }
            }
            P::Alternative(a, b) => GraphPattern::Union {
                left: Box::new(self.path(s, a, o)),
                right: Box::new(self.path(s, b, o)),
            },
            P::NegatedPropertySet(list) => {
                let f = self.fresh();
                let pred = NamedNodePattern::Variable(f.clone());
                let inner = self
                    .triple(s, &pred, o)
                    .unwrap_or_else(|| GraphPattern::Bgp {
                        patterns: vec![TriplePattern {
                            subject: s.clone(),
                            predicate: pred,
                            object: o.clone(),
                        }],
                    });
                let mut vars = Vec::new();
                collect_vars(s, &mut vars);
                collect_vars(o, &mut vars);
                GraphPattern::Project {
                    inner: Box::new(GraphPattern::Filter {
                        expr: Expression::Not(Box::new(Expression::In(
                            Box::new(Expression::Variable(f)),
                            list.iter().cloned().map(Expression::NamedNode).collect(),
                        ))),
                        inner: Box::new(inner),
                    }),
                    variables: dedup(vars),
                }
            }
            P::ZeroOrMore(_) | P::OneOrMore(_) | P::ZeroOrOne(_) => GraphPattern::Path {
                subject: s.clone(),
                path: self.links(path),
                object: o.clone(),
            },
        }
    }

    /// A path with each link that has subproperties replaced by the alternative of the
    /// link and its subproperties.
    fn links(&self, path: &PropertyPathExpression) -> PropertyPathExpression {
        use PropertyPathExpression as P;
        let b = |x: &P| Box::new(self.links(x));
        match path {
            P::NamedNode(p) => {
                let mut out = P::NamedNode(p.clone());
                if p.as_ref() != TYPE
                    && p.as_ref() != rdfs::SUB_CLASS_OF
                    && p.as_ref() != rdfs::SUB_PROPERTY_OF
                {
                    for q in RdfsSchema::get(&self.s.sub_prop, p.as_ref())
                        .into_iter()
                        .flatten()
                    {
                        out = P::Alternative(Box::new(out), Box::new(P::NamedNode(q.clone())));
                    }
                }
                out
            }
            P::NegatedPropertySet(_) => path.clone(),
            P::Reverse(x) => P::Reverse(b(x)),
            P::Sequence(x, y) => P::Sequence(b(x), b(y)),
            P::Alternative(x, y) => P::Alternative(b(x), b(y)),
            P::ZeroOrMore(x) => P::ZeroOrMore(b(x)),
            P::OneOrMore(x) => P::OneOrMore(b(x)),
            P::ZeroOrOne(x) => P::ZeroOrOne(b(x)),
        }
    }
}

fn is_var(t: &TermPattern) -> bool {
    matches!(t, TermPattern::Variable(_) | TermPattern::BlankNode(_))
}

fn tp(s: &TermPattern, p: &NamedNode, o: &TermPattern) -> GraphPattern {
    GraphPattern::Bgp {
        patterns: vec![TriplePattern {
            subject: s.clone(),
            predicate: NamedNodePattern::NamedNode(p.clone()),
            object: o.clone(),
        }],
    }
}

fn join(l: GraphPattern, r: GraphPattern) -> GraphPattern {
    GraphPattern::Join {
        left: Box::new(l),
        right: Box::new(r),
    }
}

fn union(branches: Vec<GraphPattern>) -> GraphPattern {
    let mut it = branches.into_iter();
    let first = it.next().unwrap_or(GraphPattern::Values {
        variables: Vec::new(),
        bindings: Vec::new(),
    });
    it.fold(first, |l, r| GraphPattern::Union {
        left: Box::new(l),
        right: Box::new(r),
    })
}

/// `pattern` joined with a table whose columns are `cols`: a constant column keeps the
/// rows that hold it there, a variable column binds it. `None` when no row is left.
fn with_table(
    pattern: GraphPattern,
    cols: &[&TermPattern],
    rows: impl Iterator<Item = Vec<NamedNode>>,
) -> Option<GraphPattern> {
    match table(cols, rows) {
        Table::Empty => None,
        Table::Unit => Some(pattern),
        Table::Values(v) => Some(join(pattern, v)),
    }
}

fn table(cols: &[&TermPattern], rows: impl Iterator<Item = Vec<NamedNode>>) -> Table {
    let mut variables: Vec<Variable> = Vec::new();
    let mut slot: Vec<Option<usize>> = Vec::new();
    for c in cols {
        match c {
            TermPattern::Variable(v) => match variables.iter().position(|x| x == v) {
                Some(i) => slot.push(Some(i)),
                None => {
                    variables.push(v.clone());
                    slot.push(Some(variables.len() - 1));
                }
            },
            TermPattern::NamedNode(_) => slot.push(None),
            // a class or property is an IRI: no row holds a literal, blank node or
            // triple term
            _ => return Table::Empty,
        }
    }
    let mut out: BTreeSet<Vec<NamedNode>> = BTreeSet::new();
    'rows: for row in rows {
        let mut bound: Vec<Option<NamedNode>> = vec![None; variables.len()];
        for (i, x) in row.into_iter().enumerate() {
            match (cols[i], slot[i]) {
                (TermPattern::NamedNode(n), None) if *n != x => continue 'rows,
                (_, None) => {}
                (_, Some(j)) => match &bound[j] {
                    Some(y) if *y != x => continue 'rows,
                    _ => bound[j] = Some(x),
                },
            }
        }
        out.insert(bound.into_iter().map(|b| b.expect("bound")).collect());
    }
    if out.is_empty() {
        return Table::Empty;
    }
    if variables.is_empty() {
        return Table::Unit;
    }
    Table::Values(GraphPattern::Values {
        variables,
        bindings: out
            .into_iter()
            .map(|r| {
                r.into_iter()
                    .map(|n| Some(GroundTerm::NamedNode(n)))
                    .collect()
            })
            .collect(),
    })
}

fn collect_vars(t: &TermPattern, out: &mut Vec<Variable>) {
    match t {
        TermPattern::Variable(v) => {
            if !out.contains(v) {
                out.push(v.clone());
            }
        }
        TermPattern::Triple(t) => {
            collect_vars(&t.subject, out);
            if let NamedNodePattern::Variable(v) = &t.predicate
                && !out.contains(v)
            {
                out.push(v.clone());
            }
            collect_vars(&t.object, out);
        }
        _ => {}
    }
}

fn dedup(mut v: Vec<Variable>) -> Vec<Variable> {
    let mut seen = Vec::new();
    v.retain(|x| {
        if seen.contains(x) {
            false
        } else {
            seen.push(x.clone());
            true
        }
    });
    v
}

/// Turn the blank nodes of every pattern into variables (`__rdfsb_<label>`).
fn blank_to_var(gp: &mut GraphPattern) {
    use GraphPattern as G;
    fn term(t: &mut TermPattern) {
        match t {
            TermPattern::BlankNode(b) => {
                *t = TermPattern::Variable(Variable::new_unchecked(format!(
                    "__rdfsb_{}",
                    b.as_str()
                )));
            }
            TermPattern::Triple(tr) => {
                term(&mut tr.subject);
                term(&mut tr.object);
            }
            _ => {}
        }
    }
    fn expr(e: &mut Expression) {
        use Expression as E;
        match e {
            E::Exists(p) => blank_to_var(p),
            E::Or(a, b)
            | E::And(a, b)
            | E::Equal(a, b)
            | E::SameTerm(a, b)
            | E::Greater(a, b)
            | E::GreaterOrEqual(a, b)
            | E::Less(a, b)
            | E::LessOrEqual(a, b)
            | E::Add(a, b)
            | E::Subtract(a, b)
            | E::Multiply(a, b)
            | E::Divide(a, b) => {
                expr(a);
                expr(b);
            }
            E::In(a, l) => {
                expr(a);
                l.iter_mut().for_each(expr);
            }
            E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => expr(a),
            E::If(a, b, c) => {
                expr(a);
                expr(b);
                expr(c);
            }
            E::Coalesce(l) | E::FunctionCall(_, l) => l.iter_mut().for_each(expr),
            E::NamedNode(_) | E::Literal(_) | E::Variable(_) | E::Bound(_) => {}
        }
    }
    match gp {
        G::Bgp { patterns } => {
            for t in patterns {
                term(&mut t.subject);
                term(&mut t.object);
            }
        }
        G::Path {
            subject, object, ..
        } => {
            term(subject);
            term(object);
        }
        G::Join { left, right } | G::Union { left, right } | G::Minus { left, right } => {
            blank_to_var(left);
            blank_to_var(right);
        }
        G::LeftJoin {
            left,
            right,
            expression,
        } => {
            blank_to_var(left);
            blank_to_var(right);
            if let Some(e) = expression {
                expr(e);
            }
        }
        G::Filter { expr: e, inner } => {
            expr(e);
            blank_to_var(inner);
        }
        G::Extend {
            inner, expression, ..
        } => {
            expr(expression);
            blank_to_var(inner);
        }
        G::Graph { inner, .. }
        | G::Project { inner, .. }
        | G::Distinct { inner }
        | G::Reduced { inner }
        | G::Slice { inner, .. }
        | G::OrderBy { inner, .. }
        | G::Group { inner, .. } => blank_to_var(inner),
        G::Service { .. } | G::Values { .. } => {}
    }
}
