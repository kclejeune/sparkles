//! Structural checks of a closed schema: references and inclusions resolve, no label
//! reaches itself without passing through a shape, `&include` names a triple
//! expression, no negated reference cycle (through NOT or EXTRA), unique labels, and the
//! facet rules; plus the strata of the dependency graph.
//!
//! The dependency graph has a node per declaration, one for START, and one per
//! triple-constraint value expression (each gets its own pair kind, see
//! [`crate::compile`]). An edge runs from a node to every declaration its expression
//! references and to the value expressions of the triple constraints in its shapes
//! (`&include`s expanded). An edge is negative when it is under an odd number of NOTs,
//! or when it leads to the value expression of a triple constraint whose predicate is
//! EXTRA in the enclosing shape: whether that value conforms decides whether the arc
//! may be left unmatched, so the shape is not monotone in it.
//!
//! The strongly connected components (Tarjan) of that graph must hold no negative edge.
//! Strata order the components bottom up: an edge to another component points to a
//! stratum no higher, and strictly lower when the edge is negative. Within a stratum
//! every dependency is positive, so its typing is a greatest fixed point; the strata
//! below it are final by then.

use crate::PrefixMap;
use crate::ast::{Label, NodeConstraint, NumericLiteral, Schema, Shape, ShapeExpr, TripleExpr};
use crate::error::SchemaError;
use rustc_hash::{FxHashMap, FxHashSet};

/// What the checks found that compilation needs. It borrows the checked schema.
#[derive(Clone, Debug, Default)]
pub struct Checked<'a> {
    /// the index in [`Schema::shapes`] of each declared label
    pub decls: FxHashMap<&'a Label, usize>,
    /// the labelled triple expressions (`$label`), which `&label` includes
    pub tes: FxHashMap<&'a Label, &'a TripleExpr>,
    /// the stratum of each declaration of [`Schema::shapes`], in order
    pub strata: Vec<u32>,
    /// the stratum of the START expression (0 without one)
    pub start_stratum: u32,
    /// the number of strata (strata are `0..num_strata`)
    pub num_strata: u32,
    /// the stratum of each triple-constraint value expression, by address
    value_strata: FxHashMap<usize, u32>,
}

impl Checked<'_> {
    /// The stratum of a triple constraint's value expression (an expression of the
    /// checked schema).
    pub fn value_stratum(&self, value: &ShapeExpr) -> u32 {
        self.value_strata.get(&addr(value)).copied().unwrap_or(0)
    }
}

fn addr<T>(x: &T) -> usize {
    x as *const T as usize
}

/// Check a closed schema (imports merged, see [`crate::resolve::close`]).
pub fn check(schema: &Schema) -> Result<Checked<'_>, SchemaError> {
    let mut cx = Cx {
        schema,
        decls: FxHashMap::default(),
        tes: FxHashMap::default(),
    };
    cx.collect_labels()?;
    cx.resolve()?;
    cx.check_include_cycles()?;
    cx.check_reference_cycles()?;
    let graph = Graph::build(&cx);
    let strata = graph.strata(&cx)?;
    let nd = schema.shapes.len();
    let num_strata = strata.iter().max().map_or(0, |m| m + 1);
    let value_strata = graph
        .values
        .iter()
        .enumerate()
        .map(|(i, v)| (addr(v.expr), strata[nd + 1 + i]))
        .collect();
    Ok(Checked {
        decls: cx.decls,
        tes: cx.tes,
        strata: strata[..nd].to_vec(),
        start_stratum: strata[nd],
        num_strata,
        value_strata,
    })
}

// ------------------------------------------------------------------- facets ------

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// Is `iri` one of the XSD numeric datatypes (`xsd:integer` and its derived types,
/// `xsd:decimal`, `xsd:float`, `xsd:double`)?
pub fn is_numeric_datatype(iri: &str) -> bool {
    iri.strip_prefix(XSD).is_some_and(|l| {
        matches!(
            l,
            "integer"
                | "decimal"
                | "float"
                | "double"
                | "long"
                | "int"
                | "short"
                | "byte"
                | "nonNegativeInteger"
                | "positiveInteger"
                | "nonPositiveInteger"
                | "negativeInteger"
                | "unsignedLong"
                | "unsignedInt"
                | "unsignedShort"
                | "unsignedByte"
        )
    })
}

/// Is the lexical form of a numeric bound a number of its kind?
fn valid_number(n: &NumericLiteral) -> bool {
    let (lex, dt) = match n {
        NumericLiteral::Integer(s) => (s, "integer"),
        NumericLiteral::Decimal(s) => (s, "decimal"),
        NumericLiteral::Double(s) => (s, "double"),
    };
    let lit = oxrdf::Literal::new_typed_literal(
        lex.as_str(),
        oxrdf::NamedNode::new_unchecked(format!("{XSD}{dt}")),
    );
    !lex.trim().is_empty() && lex.trim() == lex && sparkles::xsd::is_valid(&lit)
}

/// The facet rules of a node constraint (both syntaxes): no numeric facet on a
/// datatype that is not an XSD numeric one (an unknown datatype included), and numeric
/// bounds that are numbers. The error is the message.
///
/// Two string-length facets of the same kind are also an error, but a
/// [`NodeConstraint`] holds one of each: the ShExC parser reports a repeated facet
/// itself.
pub fn check_facets(nc: &NodeConstraint) -> Result<(), String> {
    let bounds = [
        ("MININCLUSIVE", &nc.min_inclusive),
        ("MINEXCLUSIVE", &nc.min_exclusive),
        ("MAXINCLUSIVE", &nc.max_inclusive),
        ("MAXEXCLUSIVE", &nc.max_exclusive),
    ];
    let numeric = bounds
        .iter()
        .filter(|(_, b)| b.is_some())
        .map(|(name, _)| *name)
        .chain(nc.total_digits.map(|_| "TOTALDIGITS"))
        .chain(nc.fraction_digits.map(|_| "FRACTIONDIGITS"))
        .next();
    if let (Some(dt), Some(facet)) = (&nc.datatype, numeric)
        && !is_numeric_datatype(dt)
    {
        return Err(format!(
            "numeric facet {facet} on <{dt}>, which is not an XSD numeric datatype"
        ));
    }
    for (name, bound) in bounds {
        if let Some(b) = bound
            && !valid_number(b)
        {
            let (NumericLiteral::Integer(s)
            | NumericLiteral::Decimal(s)
            | NumericLiteral::Double(s)) = b;
            return Err(format!("{name} needs a number, not \"{s}\""));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------- labels ------

/// A label for messages: a prefixed name when one of `prefixes` fits, else `<iri>` or
/// `_:label`.
pub fn show_label(label: &Label, prefixes: &PrefixMap) -> String {
    match label {
        Label::Iri(i) => show_iri(i, prefixes),
        Label::BNode(b) => format!("_:{b}"),
    }
}

/// An IRI for messages: a prefixed name when one of `prefixes` fits (the longest
/// namespace), else `<iri>`.
pub fn show_iri(iri: &str, prefixes: &PrefixMap) -> String {
    let local_ok = |l: &str| {
        !l.ends_with('.')
            && !l.starts_with(['-', '.'])
            && l.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
    };
    prefixes
        .iter()
        .filter(|(_, ns)| !ns.is_empty())
        .filter_map(|(p, ns)| Some((p, ns, iri.strip_prefix(ns.as_str())?)))
        .filter(|(_, _, local)| local_ok(local))
        .max_by_key(|(_, ns, _)| ns.len())
        .map_or_else(
            || format!("<{iri}>"),
            |(p, _, local)| format!("{p}:{local}"),
        )
}

// ------------------------------------------------------------------ the walk ------

struct Cx<'a> {
    schema: &'a Schema,
    decls: FxHashMap<&'a Label, usize>,
    tes: FxHashMap<&'a Label, &'a TripleExpr>,
}

impl<'a> Cx<'a> {
    fn show(&self, l: &Label) -> String {
        show_label(l, &self.schema.prefixes)
    }

    fn err(&self, msg: String) -> SchemaError {
        SchemaError::new(msg)
    }

    /// Declared labels and triple-expression labels: unique, and disjoint.
    fn collect_labels(&mut self) -> Result<(), SchemaError> {
        for (i, d) in self.schema.shapes.iter().enumerate() {
            if self.decls.insert(&d.label, i).is_some() {
                return Err(self.err(format!("duplicate shape label {}", self.show(&d.label))));
            }
        }
        let mut found = Vec::new();
        for e in self.top_exprs() {
            each_te_label(e, &mut found);
        }
        for (label, te) in found {
            if self.decls.contains_key(label) {
                return Err(self.err(format!(
                    "{} labels both a shape expression and a triple expression",
                    self.show(label)
                )));
            }
            if self.tes.insert(label, te).is_some() {
                return Err(self.err(format!(
                    "duplicate triple expression label {}",
                    self.show(label)
                )));
            }
        }
        Ok(())
    }

    /// The declarations' expressions, then START.
    fn top_exprs(&self) -> impl Iterator<Item = &'a ShapeExpr> + use<'a> {
        let s = self.schema;
        s.shapes.iter().map(|d| &d.expr).chain(s.start.as_ref())
    }

    /// References and inclusions resolve; no reference to an EXTERNAL shape without a
    /// definition; the facet rules.
    fn resolve(&self) -> Result<(), SchemaError> {
        for e in self.top_exprs() {
            self.resolve_se(e)?;
        }
        Ok(())
    }

    fn resolve_se(&self, e: &'a ShapeExpr) -> Result<(), SchemaError> {
        match e {
            ShapeExpr::Or(v) | ShapeExpr::And(v) => v.iter().try_for_each(|x| self.resolve_se(x)),
            ShapeExpr::Not(x) => self.resolve_se(x),
            ShapeExpr::Nc(nc) => check_facets(nc).map_err(SchemaError::new),
            ShapeExpr::Shape(s) => s.expression.iter().try_for_each(|t| self.resolve_te(t)),
            ShapeExpr::External => Ok(()),
            ShapeExpr::Ref(l) => match self.decls.get(l) {
                None => Err(self.err(format!("reference to undefined shape {}", self.show(l)))),
                Some(&i) if matches!(self.schema.shapes[i].expr, ShapeExpr::External) => {
                    Err(self.err(format!("external shape {} has no definition", self.show(l))))
                }
                Some(_) => Ok(()),
            },
        }
    }

    fn resolve_te(&self, t: &'a TripleExpr) -> Result<(), SchemaError> {
        match t {
            TripleExpr::EachOf(g) | TripleExpr::OneOf(g) => {
                g.exprs.iter().try_for_each(|x| self.resolve_te(x))
            }
            TripleExpr::Tc(tc) => tc.value_expr.iter().try_for_each(|v| self.resolve_se(v)),
            TripleExpr::Include(l) => {
                if self.tes.contains_key(l) {
                    Ok(())
                } else if self.decls.contains_key(l) {
                    Err(self.err(format!(
                        "&{} names a shape expression, not a triple expression",
                        self.show(l)
                    )))
                } else {
                    Err(self.err(format!(
                        "inclusion of undefined triple expression {}",
                        self.show(l)
                    )))
                }
            }
        }
    }

    /// No triple expression includes itself.
    fn check_include_cycles(&self) -> Result<(), SchemaError> {
        let mut labels: Vec<&Label> = self.tes.keys().copied().collect();
        labels.sort();
        let index: FxHashMap<&Label, usize> =
            labels.iter().enumerate().map(|(i, l)| (*l, i)).collect();
        let edges: Vec<Vec<usize>> = labels
            .iter()
            .map(|l| {
                let mut inc = Vec::new();
                includes(self.tes[l], &mut inc);
                inc.iter().map(|l| index[l]).collect()
            })
            .collect();
        if let Some(cycle) = find_cycle(&edges) {
            let path: Vec<String> = cycle.iter().map(|&i| self.show(labels[i])).collect();
            return Err(self.err(format!("inclusion cycle: {}", path.join(" -> "))));
        }
        Ok(())
    }

    /// No label reaches itself through AND, OR, NOT and references alone.
    fn check_reference_cycles(&self) -> Result<(), SchemaError> {
        let edges: Vec<Vec<usize>> = self
            .schema
            .shapes
            .iter()
            .map(|d| {
                let mut refs = Vec::new();
                direct_refs(&d.expr, &mut refs);
                refs.iter().map(|l| self.decls[l]).collect()
            })
            .collect();
        if let Some(cycle) = find_cycle(&edges) {
            let path: Vec<String> = cycle
                .iter()
                .map(|&i| self.show(&self.schema.shapes[i].label))
                .collect();
            return Err(self.err(format!(
                "reference cycle without a shape: {}",
                path.join(" -> ")
            )));
        }
        Ok(())
    }
}

/// The labelled triple expressions under `e` (in nested shapes too).
fn each_te_label<'a>(e: &'a ShapeExpr, out: &mut Vec<(&'a Label, &'a TripleExpr)>) {
    match e {
        ShapeExpr::Or(v) | ShapeExpr::And(v) => v.iter().for_each(|x| each_te_label(x, out)),
        ShapeExpr::Not(x) => each_te_label(x, out),
        ShapeExpr::Shape(s) => {
            if let Some(t) = &s.expression {
                te_labels(t, out);
            }
        }
        ShapeExpr::Nc(_) | ShapeExpr::External | ShapeExpr::Ref(_) => {}
    }
}

fn te_labels<'a>(t: &'a TripleExpr, out: &mut Vec<(&'a Label, &'a TripleExpr)>) {
    match t {
        TripleExpr::EachOf(g) | TripleExpr::OneOf(g) => {
            if let Some(id) = &g.id {
                out.push((id, t));
            }
            g.exprs.iter().for_each(|x| te_labels(x, out));
        }
        TripleExpr::Tc(tc) => {
            if let Some(id) = &tc.id {
                out.push((id, t));
            }
            if let Some(v) = &tc.value_expr {
                each_te_label(v, out);
            }
        }
        TripleExpr::Include(_) => {}
    }
}

/// The `&label`s of a triple expression (not those of nested shapes).
fn includes<'a>(t: &'a TripleExpr, out: &mut Vec<&'a Label>) {
    match t {
        TripleExpr::EachOf(g) | TripleExpr::OneOf(g) => {
            g.exprs.iter().for_each(|x| includes(x, out))
        }
        TripleExpr::Tc(_) => {}
        TripleExpr::Include(l) => out.push(l),
    }
}

/// The labels `e` references outside any shape.
fn direct_refs<'a>(e: &'a ShapeExpr, out: &mut Vec<&'a Label>) {
    match e {
        ShapeExpr::Or(v) | ShapeExpr::And(v) => v.iter().for_each(|x| direct_refs(x, out)),
        ShapeExpr::Not(x) => direct_refs(x, out),
        ShapeExpr::Ref(l) => out.push(l),
        ShapeExpr::Nc(_) | ShapeExpr::Shape(_) | ShapeExpr::External => {}
    }
}

/// A cycle of a directed graph, as its nodes with the first repeated at the end.
fn find_cycle(edges: &[Vec<usize>]) -> Option<Vec<usize>> {
    let sccs = tarjan(edges);
    for scc in &sccs.members {
        let u = scc[0];
        let cyclic = scc.len() > 1 || edges[u].contains(&u);
        if cyclic {
            let v = *edges[u]
                .iter()
                .find(|&&v| sccs.of[v] == sccs.of[u])
                .expect("a cyclic component has an inner edge");
            let mut path = vec![u];
            path.extend(shortest_path(edges, &sccs.of, v, u));
            return Some(path);
        }
    }
    None
}

/// The shortest path from `from` to `to` within one component, both ends included.
fn shortest_path(edges: &[Vec<usize>], scc: &[usize], from: usize, to: usize) -> Vec<usize> {
    let mut prev: FxHashMap<usize, usize> = FxHashMap::default();
    let mut queue = std::collections::VecDeque::from([from]);
    let mut seen = FxHashSet::from_iter([from]);
    while let Some(u) = queue.pop_front() {
        if u == to {
            break;
        }
        for &v in &edges[u] {
            if scc[v] == scc[from] && seen.insert(v) {
                prev.insert(v, u);
                queue.push_back(v);
            }
        }
    }
    let mut path = vec![to];
    let mut at = to;
    while at != from {
        at = prev[&at];
        path.push(at);
    }
    path.reverse();
    path
}

/// The strongly connected components of a graph, in reverse topological order (a
/// component comes after every component it reaches).
struct Sccs {
    /// the component of each node
    of: Vec<usize>,
    /// the nodes of each component
    members: Vec<Vec<usize>>,
}

/// Tarjan's algorithm, iterative.
fn tarjan(edges: &[Vec<usize>]) -> Sccs {
    const NONE: usize = usize::MAX;
    let n = edges.len();
    let mut index = vec![NONE; n];
    let mut low = vec![0; n];
    let mut on_stack = vec![false; n];
    let mut stack = Vec::new();
    let mut of = vec![NONE; n];
    let mut members = Vec::new();
    let mut next = 0;
    // (node, next edge to look at)
    let mut call: Vec<(usize, usize)> = Vec::new();
    for root in 0..n {
        if index[root] != NONE {
            continue;
        }
        call.push((root, 0));
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (u, ref mut e)) = call.last_mut() {
            if let Some(&v) = edges[u].get(*e) {
                *e += 1;
                if index[v] == NONE {
                    index[v] = next;
                    low[v] = next;
                    next += 1;
                    stack.push(v);
                    on_stack[v] = true;
                    call.push((v, 0));
                } else if on_stack[v] {
                    low[u] = low[u].min(index[v]);
                }
                continue;
            }
            call.pop();
            if let Some(&(parent, _)) = call.last() {
                low[parent] = low[parent].min(low[u]);
            }
            if low[u] == index[u] {
                let c = members.len();
                let mut scc = Vec::new();
                loop {
                    let w = stack.pop().expect("Tarjan stack");
                    on_stack[w] = false;
                    of[w] = c;
                    scc.push(w);
                    if w == u {
                        break;
                    }
                }
                scc.reverse();
                members.push(scc);
            }
        }
    }
    Sccs { of, members }
}

// ------------------------------------------------------- the dependency graph ------

/// Why an edge is negative.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Neg {
    Not,
    /// the EXTRA predicate
    Extra(String),
}

/// A triple-constraint value expression in the graph.
struct Value<'a> {
    expr: &'a ShapeExpr,
    /// the predicate of the (first) triple constraint it belongs to, for messages
    pred: &'a str,
}

/// Nodes: declarations `0..n`, START at `n`, then value expressions.
struct Graph<'a> {
    edges: Vec<Vec<(usize, Option<Neg>)>>,
    values: Vec<Value<'a>>,
    value_index: FxHashMap<usize, usize>,
    /// (node, shape, triple expression, under an odd number of NOTs) already walked
    walked: FxHashSet<(usize, usize, usize, bool)>,
}

impl<'a> Graph<'a> {
    fn build(cx: &Cx<'a>) -> Graph<'a> {
        let n = cx.schema.shapes.len();
        let mut g = Graph {
            edges: vec![Vec::new(); n + 1],
            values: Vec::new(),
            value_index: FxHashMap::default(),
            walked: FxHashSet::default(),
        };
        for (i, d) in cx.schema.shapes.iter().enumerate() {
            g.walk_se(cx, i, &d.expr, false);
        }
        if let Some(s) = &cx.schema.start {
            g.walk_se(cx, n, s, false);
        }
        // value expressions found on the way, each walked once
        let mut i = 0;
        while let Some(v) = g.values.get(i) {
            let e = v.expr;
            g.walk_se(cx, n + 1 + i, e, false);
            i += 1;
        }
        g
    }

    fn walk_se(&mut self, cx: &Cx<'a>, from: usize, e: &'a ShapeExpr, odd: bool) {
        match e {
            ShapeExpr::Or(v) | ShapeExpr::And(v) => {
                v.iter().for_each(|x| self.walk_se(cx, from, x, odd))
            }
            ShapeExpr::Not(x) => self.walk_se(cx, from, x, !odd),
            ShapeExpr::Ref(l) => {
                let to = cx.decls[l];
                self.edges[from].push((to, odd.then_some(Neg::Not)));
            }
            ShapeExpr::Shape(s) => {
                if let Some(t) = &s.expression {
                    self.walk_te(cx, from, s, t, odd);
                }
            }
            ShapeExpr::Nc(_) | ShapeExpr::External => {}
        }
    }

    fn walk_te(&mut self, cx: &Cx<'a>, from: usize, s: &'a Shape, t: &'a TripleExpr, odd: bool) {
        if !self.walked.insert((from, addr(s), addr(t), odd)) {
            return;
        }
        match t {
            TripleExpr::EachOf(g) | TripleExpr::OneOf(g) => g
                .exprs
                .iter()
                .for_each(|x| self.walk_te(cx, from, s, x, odd)),
            TripleExpr::Include(l) => self.walk_te(cx, from, s, cx.tes[l], odd),
            TripleExpr::Tc(tc) => {
                let Some(v) = &tc.value_expr else { return };
                let to = self.value_node(cx, v, &tc.predicate);
                let neg = if s.extra.contains(&tc.predicate) {
                    Some(Neg::Extra(tc.predicate.clone()))
                } else {
                    odd.then_some(Neg::Not)
                };
                self.edges[from].push((to, neg));
            }
        }
    }

    fn value_node(&mut self, cx: &Cx<'a>, e: &'a ShapeExpr, pred: &'a str) -> usize {
        let base = cx.schema.shapes.len() + 1;
        let next = self.values.len();
        let i = *self.value_index.entry(addr(e)).or_insert(next);
        if i == next {
            self.values.push(Value { expr: e, pred });
            self.edges.push(Vec::new());
        }
        base + i
    }

    /// The stratum of every node, or the error for a negated reference cycle.
    fn strata(&self, cx: &Cx<'a>) -> Result<Vec<u32>, SchemaError> {
        let plain: Vec<Vec<usize>> = self
            .edges
            .iter()
            .map(|es| es.iter().map(|(v, _)| *v).collect())
            .collect();
        let sccs = tarjan(&plain);
        for (u, es) in self.edges.iter().enumerate() {
            for (v, neg) in es {
                if neg.is_some() && sccs.of[u] == sccs.of[*v] {
                    let mut cycle = vec![u];
                    cycle.extend(shortest_path(&plain, &sccs.of, *v, u));
                    return Err(SchemaError::new(format!(
                        "negated reference cycle: {}",
                        self.show_cycle(cx, &cycle)
                    )));
                }
            }
        }
        // components come sinks first, so the targets of their edges are done
        let mut scc_stratum = vec![0u32; sccs.members.len()];
        for (c, members) in sccs.members.iter().enumerate() {
            let mut s = 0;
            for &u in members {
                for (v, neg) in &self.edges[u] {
                    let d = sccs.of[*v];
                    if d != c {
                        s = s.max(scc_stratum[d] + u32::from(neg.is_some()));
                    }
                }
            }
            scc_stratum[c] = s;
        }
        Ok(sccs.of.iter().map(|&c| scc_stratum[c]).collect())
    }

    /// `ex:S -[NOT]-> ex:T -> ex:S`: the declarations of a cycle (its first node
    /// repeated at the end), with the first negative edge between two of them.
    fn show_cycle(&self, cx: &Cx<'a>, cycle: &[usize]) -> String {
        let n = cx.schema.shapes.len();
        let prefixes = &cx.schema.prefixes;
        let name = |u: usize| {
            if u < n {
                show_label(&cx.schema.shapes[u].label, prefixes)
            } else if u == n {
                "START".to_string()
            } else {
                format!(
                    "the value of {}",
                    show_iri(self.values[u - n - 1].pred, prefixes)
                )
            }
        };
        // start at a declaration when the cycle has one
        let len = cycle.len() - 1;
        let first = (0..len).find(|&i| cycle[i] < n).unwrap_or(0);
        let nodes: Vec<usize> = (0..=len).map(|k| cycle[(first + k) % len]).collect();
        let mut out = name(nodes[0]);
        let mut neg: Option<&Neg> = None;
        for w in nodes.windows(2) {
            let (u, v) = (w[0], w[1]);
            let e = self.edges[u]
                .iter()
                .filter(|(t, _)| *t == v)
                .find_map(|(_, g)| g.as_ref());
            neg = neg.or(e);
            if v < n || v == nodes[0] {
                match neg.take() {
                    None => out.push_str(" -> "),
                    Some(Neg::Not) => out.push_str(" -[NOT]-> "),
                    Some(Neg::Extra(p)) => {
                        out.push_str(&format!(" -[EXTRA {}]-> ", show_iri(p, prefixes)))
                    }
                }
                out.push_str(&name(v));
            }
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests;
