//! A test-only reference evaluator for SPARQL 1.1 and 1.2 queries (roadmap item 512).
//!
//! It follows the algebra of SPARQL 1.1 section 18 and of the SPARQL 1.2 drafts
//! literally. Every operator is a nested loop over solution mappings, and nothing is
//! planned, reordered, cached or specialized, so an answer is as slow and as plain as
//! the specification's definitions. It reads a list of quads and a parsed
//! [`spargebra::Query`], and it uses nothing of the engine, so a defect of the engine
//! cannot reach it. The engine tests compare their answers with it where they once
//! compared with `Optimizations::NONE` or with the eager executor.
//!
//! What it does not evaluate is reported as [`Error::Unsupported`] and never answered
//! approximately, so a test can tell a query outside the evaluator from a disagreement.
//! That covers SERVICE, DESCRIBE, CONSTRUCT with graph templates, the Jena extensions of
//! the algebra, extension functions and aggregates, and `xsd` casts other than the
//! `xsd:string`, `xsd:boolean`, `xsd:integer`, `xsd:decimal`, `xsd:float`, `xsd:double`
//! and `xsd:dateTime` ones.
//!
//! Where the specification leaves an answer open, the evaluator picks one valid answer
//! and says so. [`value::spec_order`] returns `None` for the pairs of terms that ORDER BY
//! leaves unordered, and [`Reference::run_noting_choices`] reports in [`Notes`] whether
//! the answer depended on such a choice. That happens when a slice cuts between rows that the
//! specification does not order strictly, and when MIN, MAX, SAMPLE or GROUP_CONCAT pick
//! among values that it does not order. A comparison with an engine must allow every
//! answer that the specification allows.
//!
//! The file is included by `#[path]` into each test crate that uses it, so it carries
//! `allow(dead_code)`.
#![allow(dead_code, unused_imports)]

mod expr;
mod path;
pub mod value;

use oxiri::Iri;
use oxrdf::{BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use oxrdf::{Variable, vocab::xsd};
use oxsdatatypes::DateTime;
use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression, GraphPattern, OrderExpression, QueryDataset,
};
use spargebra::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};
use spargebra::{Query, SparqlParser};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use value::{ebv, int_term, total_order};

/// A solution mapping.
pub type Solution = BTreeMap<Variable, Term>;

/// Why a query got no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The query does not parse.
    Parse(String),
    /// The evaluator does not implement a feature of the query.
    Unsupported(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse(m) => write!(f, "parse error: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported by the reference evaluator: {m}"),
        }
    }
}

/// The answer to a query.
#[derive(Debug, Clone)]
pub enum Answer {
    Select {
        variables: Vec<String>,
        rows: Vec<Vec<Option<Term>>>,
    },
    Ask(bool),
    /// CONSTRUCT: the triples, each once.
    Graph(Vec<Triple>),
}

/// An evaluation failure that is not the SPARQL error value: an expression error is
/// [`Fail::Type`] and is handled where the algebra says (a FILTER drops the solution, an
/// Extend leaves the variable unbound), and [`Fail::Unsupported`] ends the query.
#[derive(Debug, Clone)]
pub(crate) enum Fail {
    Type,
    Unsupported(String),
}

pub(crate) type R<T> = Result<T, Fail>;

pub(crate) fn unsupported<T>(what: impl Into<String>) -> R<T> {
    Err(Fail::Unsupported(what.into()))
}

/// The triples of one graph, or of the merge of several, with an index on the
/// predicate.
pub(crate) struct GraphData {
    pub triples: Vec<Triple>,
    pub by_predicate: HashMap<NamedNode, Vec<usize>>,
}

impl GraphData {
    fn new(triples: Vec<Triple>) -> GraphData {
        let mut by_predicate: HashMap<NamedNode, Vec<usize>> = HashMap::new();
        for (i, t) in triples.iter().enumerate() {
            by_predicate.entry(t.predicate.clone()).or_default().push(i);
        }
        GraphData {
            triples,
            by_predicate,
        }
    }

    /// The nodes of the graph: the subjects and the objects of its triples.
    pub fn nodes(&self) -> Vec<Term> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for t in &self.triples {
            for n in [Term::from(t.subject.clone()), t.object.clone()] {
                if seen.insert(n.clone()) {
                    out.push(n);
                }
            }
        }
        out
    }
}

/// The reference evaluator over a dataset.
pub struct Reference {
    quads: Vec<Quad>,
    union_default: bool,
    pass_min_max_errors: bool,
    now: DateTime,
}

impl Reference {
    /// A dataset of the quads, each once.
    pub fn new(quads: impl IntoIterator<Item = Quad>) -> Reference {
        let mut seen = HashSet::new();
        let quads = quads
            .into_iter()
            .filter(|q| seen.insert(q.clone()))
            .collect();
        Reference {
            quads,
            union_default: false,
            pass_min_max_errors: false,
            now: DateTime::now(),
        }
    }

    /// Make the default graph the merge of every graph, as a store with a union default
    /// graph does.
    pub fn with_union_default(mut self, union: bool) -> Reference {
        self.union_default = union;
        self
    }

    /// Make MIN and MAX pass over the errors among their values, a reading other than
    /// the specification's, which [`Notes::min_max_error`] describes. A test may compare
    /// with it where the difference is tracked as an open question.
    pub fn passing_over_min_max_errors(mut self) -> Reference {
        self.pass_min_max_errors = true;
        self
    }

    pub fn quads(&self) -> &[Quad] {
        &self.quads
    }

    /// Parse and evaluate `text`.
    pub fn run(&self, text: &str, base: Option<&str>) -> Result<Answer, Error> {
        self.run_noting_choices(text, base)
            .map(|(answer, _)| answer)
    }

    /// Parse and evaluate `text`, with the [`Notes`] on its answer.
    pub fn run_noting_choices(
        &self,
        text: &str,
        base: Option<&str>,
    ) -> Result<(Answer, Notes), Error> {
        let mut parser = SparqlParser::new();
        if let Some(b) = base {
            parser = parser
                .with_base_iri(b)
                .map_err(|e| Error::Parse(e.to_string()))?;
        }
        let query = parser
            .parse_query(text)
            .map_err(|e| Error::Parse(e.to_string()))?;
        self.evaluate_noting_choices(&query)
    }

    /// Evaluate a parsed query.
    pub fn evaluate(&self, query: &Query) -> Result<Answer, Error> {
        self.evaluate_noting_choices(query)
            .map(|(answer, _)| answer)
    }

    /// Evaluate a parsed query, with the [`Notes`] on its answer.
    pub fn evaluate_noting_choices(&self, query: &Query) -> Result<(Answer, Notes), Error> {
        let (dataset, pattern, base) = match query {
            Query::Select {
                dataset,
                pattern,
                base_iri,
            }
            | Query::Ask {
                dataset,
                pattern,
                base_iri,
            }
            | Query::Describe {
                dataset,
                pattern,
                base_iri,
            }
            | Query::Construct {
                dataset,
                pattern,
                base_iri,
                ..
            } => (dataset, pattern, base_iri),
        };
        let ev = Evaluator::new(self, dataset.as_ref(), base.clone());
        let run = || -> R<Answer> {
            let solutions = ev.eval_top(pattern)?;
            match query {
                Query::Select { .. } => {
                    let variables = projected(pattern);
                    let rows = solutions
                        .iter()
                        .map(|s| variables.iter().map(|v| s.get(v).cloned()).collect())
                        .collect();
                    Ok(Answer::Select {
                        variables: variables.iter().map(|v| v.as_str().to_string()).collect(),
                        rows,
                    })
                }
                Query::Ask { .. } => Ok(Answer::Ask(!solutions.is_empty())),
                Query::Construct {
                    template,
                    graph_templates,
                    ..
                } => {
                    if !graph_templates.is_empty() {
                        return unsupported("CONSTRUCT with graph templates");
                    }
                    Ok(Answer::Graph(ev.construct(template, &solutions)))
                }
                Query::Describe { .. } => unsupported("DESCRIBE"),
            }
        };
        let answer = run().map_err(|f| match f {
            Fail::Unsupported(m) => Error::Unsupported(m),
            Fail::Type => Error::Unsupported("an expression error outside any expression".into()),
        })?;
        let mut notes = ev.notes.get();
        notes.chose = ev.chose.get();
        Ok((answer, notes))
    }
}

/// The projected variables of a SELECT: those of the Project below its Slice, Distinct
/// and Reduced.
fn projected(p: &GraphPattern) -> Vec<Variable> {
    match p {
        GraphPattern::Slice { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner } => projected(inner),
        GraphPattern::Project { variables, .. } => variables.clone(),
        other => {
            let mut vars = Vec::new();
            other.on_in_scope_variable(|v| {
                if !vars.contains(v) {
                    vars.push(v.clone());
                }
            });
            vars
        }
    }
}

pub(crate) struct Evaluator<'a> {
    data: &'a Reference,
    pub base: Option<Iri<String>>,
    pub now: DateTime,
    default_names: Vec<GraphName>,
    named_names: Vec<GraphName>,
    graphs: RefCell<HashMap<Vec<GraphName>, Rc<GraphData>>>,
    /// The solution that BNODE(label) is evaluated for.
    pub scope: Cell<u64>,
    pub bnodes: RefCell<HashMap<(u64, String), BlankNode>>,
    pub fresh: Cell<u64>,
    pub rng: Cell<u64>,
    /// Whether the evaluation made a choice that the specification leaves open.
    pub chose: Cell<bool>,
    pub notes: Cell<Notes>,
}

/// What an evaluation noted about its answer beside the answer itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Notes {
    /// The specification allows other answers, because the evaluator made a choice
    /// that it leaves open.
    pub chose: bool,
    /// MIN or MAX read an error. The evaluator follows the specification's definition,
    /// which sorts the values in the order of ORDER BY, where an error sorts first, so
    /// MIN is an error and MAX passes over it. Jena makes both an error, and other
    /// readings pass over it in both.
    pub min_max_error: bool,
    /// `COUNT(DISTINCT *)` counted solutions of a pattern with blank nodes. Section
    /// 18.2.2.4 joins the two halves of a path sequence on a fresh variable, which is in
    /// the solutions, while a blank node of a basic graph pattern is not. The parser
    /// writes both as blank nodes, so the evaluator cannot tell them apart and leaves
    /// both out.
    pub distinct_over_blank_nodes: bool,
}

/// Solutions in ORDER BY order, with the sort key of each and the direction of each
/// key: what a slice needs to tell whether its cut is one the specification fixes.
struct Keyed {
    solutions: Vec<Solution>,
    keys: Option<(Vec<Vec<Option<Term>>>, Vec<bool>)>,
}

/// Whether the specification puts the key `a` strictly before `b`.
fn strictly_before(a: &[Option<Term>], b: &[Option<Term>], descending: &[bool]) -> bool {
    for ((x, y), desc) in a.iter().zip(b).zip(descending) {
        let mut o = value::spec_order(x.as_ref(), y.as_ref());
        if *desc {
            o = o.map(std::cmp::Ordering::reverse);
        }
        match o {
            Some(std::cmp::Ordering::Less) => return true,
            Some(std::cmp::Ordering::Equal) => {}
            _ => return false,
        }
    }
    false
}

/// Where a pattern is evaluated: the active graph and the solution that a LATERAL join
/// or an EXISTS substitutes into the pattern.
pub(crate) struct Cx<'c> {
    pub graph: &'c Rc<GraphData>,
    pub env: &'c Solution,
}

/// Whether a variable is one of the evaluator's own: the blank nodes of a basic graph
/// pattern, which an RDF instance mapping binds and a solution mapping does not, and the
/// solution number that BNODE reads.
pub(crate) fn is_hidden(v: &Variable) -> bool {
    v.as_str().starts_with(':')
}

/// Compatibility of two solution mappings. The solution numbers of BNODE belong to each
/// side alone, so they never make two solutions incompatible.
pub(crate) fn compatible(a: &Solution, b: &Solution) -> bool {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    small
        .iter()
        .all(|(k, v)| *k == row_variable() || large.get(k).is_none_or(|w| w == v))
}

/// A solution without the evaluator's own variables.
fn visible(s: &Solution) -> Solution {
    s.iter()
        .filter(|(k, _)| !is_hidden(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

pub(crate) fn merge(a: &Solution, b: &Solution) -> Solution {
    let mut out = a.clone();
    for (k, v) in b {
        if out
            .insert(k.clone(), v.clone())
            .is_some_and(|old| old != *v)
        {
            // the merge of two numbered solutions is a new solution, which a later BNODE
            // numbers again
            out.remove(k);
        }
    }
    out
}

/// The hidden variable that numbers a solution for BNODE.
pub(crate) fn row_variable() -> Variable {
    Variable::new_unchecked(":row")
}

fn hidden_bnode_variable(b: &BlankNode) -> Variable {
    Variable::new_unchecked(format!(":bnode:{}", b.as_str()))
}

impl<'a> Evaluator<'a> {
    fn new(data: &'a Reference, dataset: Option<&QueryDataset>, base: Option<Iri<String>>) -> Self {
        let all_named: Vec<GraphName> = {
            let mut seen = HashSet::new();
            data.quads
                .iter()
                .filter(|q| !q.graph_name.is_default_graph())
                .filter(|q| seen.insert(q.graph_name.clone()))
                .map(|q| q.graph_name.clone())
                .collect()
        };
        let (default_names, named_names) = match dataset {
            None if data.union_default => {
                let mut d = vec![GraphName::DefaultGraph];
                d.extend(all_named.clone());
                (d, all_named)
            }
            None => (vec![GraphName::DefaultGraph], all_named),
            Some(ds) => {
                let default = ds
                    .default
                    .iter()
                    .map(|n| GraphName::NamedNode(n.clone()))
                    .collect();
                let named = ds
                    .named
                    .as_ref()
                    .map(|n| n.iter().map(|n| GraphName::NamedNode(n.clone())).collect())
                    .unwrap_or_default();
                (default, named)
            }
        };
        Evaluator {
            data,
            base,
            now: data.now,
            default_names,
            named_names,
            graphs: RefCell::new(HashMap::new()),
            scope: Cell::new(0),
            bnodes: RefCell::new(HashMap::new()),
            fresh: Cell::new(0),
            rng: Cell::new(0x9e37_79b9_7f4a_7c15),
            chose: Cell::new(false),
            notes: Cell::new(Notes::default()),
        }
    }

    fn graph_of(&self, names: &[GraphName]) -> Rc<GraphData> {
        if let Some(g) = self.graphs.borrow().get(names) {
            return g.clone();
        }
        let mut seen = HashSet::new();
        let mut triples = Vec::new();
        for q in &self.data.quads {
            if names.contains(&q.graph_name) {
                let t = Triple::new(q.subject.clone(), q.predicate.clone(), q.object.clone());
                if seen.insert(t.clone()) {
                    triples.push(t);
                }
            }
        }
        let g = Rc::new(GraphData::new(triples));
        self.graphs.borrow_mut().insert(names.to_vec(), g.clone());
        g
    }

    pub(crate) fn fresh_id(&self) -> u64 {
        let n = self.fresh.get();
        self.fresh.set(n + 1);
        n
    }

    fn eval_top(&self, p: &GraphPattern) -> R<Vec<Solution>> {
        let default = self.graph_of(&self.default_names.clone());
        let env = Solution::new();
        self.eval(
            p,
            &Cx {
                graph: &default,
                env: &env,
            },
        )
    }

    // ---------------------------------------------------------------- patterns ------

    pub(crate) fn eval(&self, p: &GraphPattern, cx: &Cx) -> R<Vec<Solution>> {
        match p {
            GraphPattern::Bgp { patterns } => {
                let mut sols = vec![Solution::new()];
                for tp in patterns {
                    let mut next = Vec::new();
                    for s in &sols {
                        self.match_triple_pattern(tp, s, cx, &mut next);
                    }
                    sols = next;
                }
                Ok(sols)
            }
            GraphPattern::Path {
                subject,
                path,
                object,
            } => self.eval_path_pattern(subject, path, object, cx),
            GraphPattern::Join { left, right } => {
                let (l, r) = (self.eval(left, cx)?, self.eval(right, cx)?);
                let mut out = Vec::new();
                for a in &l {
                    for b in &r {
                        if compatible(a, b) {
                            out.push(merge(a, b));
                        }
                    }
                }
                Ok(out)
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => {
                let (l, r) = (self.eval(left, cx)?, self.eval(right, cx)?);
                let mut out = Vec::new();
                for a in &l {
                    let mut matched = false;
                    for b in &r {
                        if !compatible(a, b) {
                            continue;
                        }
                        let m = merge(a, b);
                        let keep = match expression {
                            None => true,
                            Some(e) => self.truthy(e, cx, &m)?,
                        };
                        if keep {
                            matched = true;
                            out.push(m);
                        }
                    }
                    if !matched {
                        out.push(a.clone());
                    }
                }
                Ok(out)
            }
            GraphPattern::Lateral { left, right } => {
                let mut out = Vec::new();
                for a in self.eval(left, cx)? {
                    let env = merge(cx.env, &a);
                    let inner = Cx {
                        graph: cx.graph,
                        env: &env,
                    };
                    for b in self.eval(right, &inner)? {
                        if compatible(&a, &b) {
                            out.push(merge(&a, &b));
                        }
                    }
                }
                Ok(out)
            }
            GraphPattern::Filter { expr, inner } => {
                let mut out = Vec::new();
                for s in self.eval(inner, cx)? {
                    if self.truthy(expr, cx, &s)? {
                        out.push(s);
                    }
                }
                Ok(out)
            }
            GraphPattern::Union { left, right } => {
                let mut out = self.eval(left, cx)?;
                out.extend(self.eval(right, cx)?);
                Ok(out)
            }
            GraphPattern::Graph { name, inner } => self.eval_graph(name, inner, cx),
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } => {
                let mut out = Vec::new();
                for mut s in self.eval(inner, cx)? {
                    // BNODE(label) is the same blank node within one solution, so a solution
                    // that an expression with BNODE reads gets a number to key it by.
                    if format!("{expression:?}").contains("BNode")
                        && !s.contains_key(&row_variable())
                    {
                        let n = self.fresh_id();
                        s.insert(row_variable(), int_term(n as i128));
                    }
                    if let Some(v) = self.soft(self.expr(expression, cx, &s))? {
                        if s.get(variable).is_some_and(|old| *old != v) {
                            // a variable already in scope cannot be extended
                            return unsupported("BIND of a variable that is in scope");
                        }
                        s.insert(variable.clone(), v);
                    }
                    out.push(s);
                }
                Ok(out)
            }
            GraphPattern::Minus { left, right } => {
                let (l, r) = (self.eval(left, cx)?, self.eval(right, cx)?);
                Ok(l.into_iter()
                    .filter(|a| {
                        !r.iter().any(|b| {
                            compatible(a, b) && a.keys().any(|k| !is_hidden(k) && b.contains_key(k))
                        })
                    })
                    .collect())
            }
            GraphPattern::Values {
                variables,
                bindings,
            } => {
                let mut out = Vec::new();
                for row in bindings {
                    let mut s = Solution::new();
                    for (v, t) in variables.iter().zip(row) {
                        if let Some(t) = t {
                            s.insert(v.clone(), ground_term(t));
                        }
                    }
                    if compatible(&s, cx.env) {
                        out.push(s);
                    }
                }
                Ok(out)
            }
            GraphPattern::OrderBy { .. }
            | GraphPattern::Project { .. }
            | GraphPattern::Distinct { .. } => Ok(self.eval_keyed(p, cx)?.solutions),
            // REDUCED may drop duplicates and need not, and this one keeps them
            GraphPattern::Reduced { inner } => self.eval(inner, cx),
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => {
                let Keyed { solutions, keys } = self.eval_keyed(inner, cx)?;
                let n = solutions.len();
                let from = (*start).min(n);
                let to = length.map_or(n, |l| from.saturating_add(l).min(n));
                // A cut is fixed when every row before it either equals every row after
                // it or comes strictly before it in the order.
                let visible_rows: Vec<Solution> = solutions.iter().map(visible).collect();
                for cut in [from, to] {
                    if cut == 0 || cut == n {
                        continue;
                    }
                    let fixed = (0..cut).all(|i| {
                        (cut..n).all(|j| {
                            visible_rows[i] == visible_rows[j]
                                || keys
                                    .as_ref()
                                    .is_some_and(|(k, d)| strictly_before(&k[i], &k[j], d))
                        })
                    });
                    if !fixed {
                        self.chose.set(true);
                    }
                }
                Ok(solutions.into_iter().skip(from).take(to - from).collect())
            }
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => self.eval_group(inner, variables, aggregates, cx),
            GraphPattern::Service { .. } => unsupported("SERVICE"),
            GraphPattern::Assign { .. } => unsupported("LET (Jena)"),
            GraphPattern::SemiJoin { .. } => unsupported("SEMIJOIN (Jena)"),
            GraphPattern::AntiJoin { .. } => unsupported("ANTIJOIN (Jena)"),
            GraphPattern::Unfold { .. } => unsupported("UNFOLD (Jena)"),
        }
    }

    /// Evaluate ORDER BY, and the projection and DISTINCT above it, keeping the sort
    /// keys for a slice. Any other pattern has no keys.
    fn eval_keyed(&self, p: &GraphPattern, cx: &Cx) -> R<Keyed> {
        match p {
            GraphPattern::OrderBy { inner, expression } => {
                let sols = self.eval(inner, cx)?;
                let mut keyed = Vec::new();
                for s in sols {
                    let mut keys = Vec::new();
                    for o in expression {
                        let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = o;
                        keys.push(self.soft(self.expr(e, cx, &s))?);
                    }
                    keyed.push((keys, s));
                }
                let descending: Vec<bool> = expression
                    .iter()
                    .map(|o| matches!(o, OrderExpression::Desc(_)))
                    .collect();
                keyed.sort_by(|(a, _), (b, _)| {
                    for (i, desc) in descending.iter().enumerate() {
                        let mut c = total_order(a[i].as_ref(), b[i].as_ref());
                        if *desc {
                            c = c.reverse();
                        }
                        if c.is_ne() {
                            return c;
                        }
                    }
                    std::cmp::Ordering::Equal
                });
                let (keys, solutions) = keyed.into_iter().unzip();
                Ok(Keyed {
                    solutions,
                    keys: Some((keys, descending)),
                })
            }
            GraphPattern::Project { inner, variables } => {
                // A sub-SELECT's variables that it does not project are its own, so the
                // solution that a LATERAL join or EXISTS substitutes stops at them.
                let env: Solution = cx
                    .env
                    .iter()
                    .filter(|(v, _)| variables.contains(v))
                    .map(|(v, t)| (v.clone(), t.clone()))
                    .collect();
                let inner_cx = Cx {
                    graph: cx.graph,
                    env: &env,
                };
                let mut k = self.eval_keyed(inner, &inner_cx)?;
                k.solutions = k
                    .solutions
                    .into_iter()
                    .map(|s| {
                        s.into_iter()
                            .filter(|(k, _)| variables.contains(k))
                            .collect()
                    })
                    .collect();
                Ok(k)
            }
            GraphPattern::Distinct { inner } => {
                let Keyed { solutions, keys } = self.eval_keyed(inner, cx)?;
                let mut first: HashMap<Solution, usize> = HashMap::new();
                let mut kept = Vec::new();
                for (i, s) in solutions.iter().enumerate() {
                    match first.get(&visible(s)) {
                        None => {
                            first.insert(visible(s), i);
                            kept.push(i);
                        }
                        // Which copy DISTINCT keeps is open, and it places the row
                        // differently when the copies have different keys.
                        Some(&f) => {
                            if let Some((k, _)) = &keys
                                && k[f] != k[i]
                            {
                                self.chose.set(true);
                            }
                        }
                    }
                }
                Ok(Keyed {
                    solutions: kept.iter().map(|&i| solutions[i].clone()).collect(),
                    keys: keys.map(|(k, d)| (kept.iter().map(|&i| k[i].clone()).collect(), d)),
                })
            }
            _ => Ok(Keyed {
                solutions: self.eval(p, cx)?,
                keys: None,
            }),
        }
    }

    /// Evaluate an expression and read an effective boolean value, which is false for an
    /// error.
    pub(crate) fn truthy(&self, e: &Expression, cx: &Cx, s: &Solution) -> R<bool> {
        Ok(self
            .soft(self.expr(e, cx, s))?
            .and_then(|t| ebv(&t))
            .unwrap_or(false))
    }

    /// Turn an expression error into `None`, and keep the failures that end the query.
    pub(crate) fn soft<T>(&self, r: R<T>) -> R<Option<T>> {
        match r {
            Ok(v) => Ok(Some(v)),
            Err(Fail::Type) => Ok(None),
            Err(f) => Err(f),
        }
    }

    fn eval_graph(
        &self,
        name: &NamedNodePattern,
        inner: &GraphPattern,
        cx: &Cx,
    ) -> R<Vec<Solution>> {
        let mut out = Vec::new();
        match name {
            NamedNodePattern::NamedNode(n) => {
                let g = GraphName::NamedNode(n.clone());
                if self.named_names.contains(&g) {
                    let data = self.graph_of(&[g]);
                    out = self.eval(
                        inner,
                        &Cx {
                            graph: &data,
                            env: cx.env,
                        },
                    )?;
                }
            }
            NamedNodePattern::Variable(v) => {
                for g in &self.named_names {
                    let term = match g {
                        GraphName::NamedNode(n) => Term::NamedNode(n.clone()),
                        GraphName::BlankNode(b) => Term::BlankNode(b.clone()),
                        GraphName::DefaultGraph => continue,
                    };
                    if cx.env.get(v).is_some_and(|e| *e != term) {
                        continue;
                    }
                    let data = self.graph_of(std::slice::from_ref(g));
                    for mut s in self.eval(
                        inner,
                        &Cx {
                            graph: &data,
                            env: cx.env,
                        },
                    )? {
                        if s.get(v).is_some_and(|b| *b != term) {
                            continue;
                        }
                        if !cx.env.contains_key(v) {
                            s.insert(v.clone(), term.clone());
                        }
                        out.push(s);
                    }
                }
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------ basic patterns ------

    fn bind(&self, v: &Variable, t: &Term, s: &mut Solution, cx: &Cx) -> bool {
        if let Some(e) = cx.env.get(v) {
            return e == t;
        }
        match s.get(v) {
            Some(old) => old == t,
            None => {
                s.insert(v.clone(), t.clone());
                true
            }
        }
    }

    fn match_term(&self, p: &TermPattern, t: &Term, s: &mut Solution, cx: &Cx) -> bool {
        match p {
            TermPattern::NamedNode(n) => matches!(t, Term::NamedNode(m) if m == n),
            TermPattern::Literal(l) => matches!(t, Term::Literal(m) if m == l),
            TermPattern::BlankNode(b) => self.bind(&hidden_bnode_variable(b), t, s, cx),
            TermPattern::Variable(v) => self.bind(v, t, s, cx),
            TermPattern::Triple(tp) => match t {
                Term::Triple(tr) => self.match_triple(tp, tr, s, cx),
                _ => false,
            },
        }
    }

    fn match_triple(&self, tp: &TriplePattern, t: &Triple, s: &mut Solution, cx: &Cx) -> bool {
        self.match_term(&tp.subject, &Term::from(t.subject.clone()), s, cx)
            && match &tp.predicate {
                NamedNodePattern::NamedNode(n) => *n == t.predicate,
                NamedNodePattern::Variable(v) => {
                    self.bind(v, &Term::NamedNode(t.predicate.clone()), s, cx)
                }
            }
            && self.match_term(&tp.object, &t.object, s, cx)
    }

    fn match_triple_pattern(
        &self,
        tp: &TriplePattern,
        s: &Solution,
        cx: &Cx,
        out: &mut Vec<Solution>,
    ) {
        // an index only narrows the scan to the triples that could match
        let predicate = match &tp.predicate {
            NamedNodePattern::NamedNode(n) => Some(n.clone()),
            NamedNodePattern::Variable(v) => match s.get(v).or_else(|| cx.env.get(v)) {
                Some(Term::NamedNode(n)) => Some(n.clone()),
                Some(_) => return,
                None => None,
            },
        };
        let mut try_one = |t: &Triple| {
            let mut m = s.clone();
            if self.match_triple(tp, t, &mut m, cx) {
                out.push(m);
            }
        };
        match predicate {
            Some(p) => {
                for &i in cx.graph.by_predicate.get(&p).into_iter().flatten() {
                    try_one(&cx.graph.triples[i]);
                }
            }
            None => cx.graph.triples.iter().for_each(&mut try_one),
        }
    }

    fn eval_path_pattern(
        &self,
        subject: &TermPattern,
        path: &spargebra::algebra::PropertyPathExpression,
        object: &TermPattern,
        cx: &Cx,
    ) -> R<Vec<Solution>> {
        // The parser has already rewritten a sequence at the top of the pattern into two
        // triple patterns joined on a fresh blank node, as section 18.2.2.4 does with a
        // fresh variable, and the inverse of an IRI into a swapped triple pattern.
        // The endpoints that the pattern fixes, which the zero-length paths need as
        // nodes even when the graph does not hold them.
        let fixed = |p: &TermPattern, cx: &Cx| -> Option<Term> {
            match p {
                TermPattern::NamedNode(n) => Some(Term::NamedNode(n.clone())),
                TermPattern::Literal(l) => Some(Term::Literal(l.clone())),
                TermPattern::Variable(v) => cx.env.get(v).cloned(),
                _ => None,
            }
        };
        let extra: Vec<Term> = [fixed(subject, cx), fixed(object, cx)]
            .into_iter()
            .flatten()
            .collect();
        let pairs = self.path_pairs(path, cx.graph, &extra)?;
        let mut out = Vec::new();
        for (a, b) in pairs {
            let mut s = Solution::new();
            if self.match_term(subject, &a, &mut s, cx) && self.match_term(object, &b, &mut s, cx) {
                out.push(s);
            }
        }
        Ok(out)
    }

    // ----------------------------------------------------------------- aggregates ------

    fn eval_group(
        &self,
        inner: &GraphPattern,
        variables: &[Variable],
        aggregates: &[(Variable, AggregateExpression)],
        cx: &Cx,
    ) -> R<Vec<Solution>> {
        let sols = self.eval(inner, cx)?;
        let mut keys: Vec<Vec<Option<Term>>> = Vec::new();
        let mut groups: HashMap<Vec<Option<Term>>, Vec<Solution>> = HashMap::new();
        for s in sols {
            let key: Vec<Option<Term>> = variables.iter().map(|v| s.get(v).cloned()).collect();
            if !groups.contains_key(&key) {
                keys.push(key.clone());
            }
            groups.entry(key).or_default().push(s);
        }
        // Without GROUP BY there is one group, even of no solutions.
        if variables.is_empty() && keys.is_empty() {
            keys.push(Vec::new());
            groups.insert(Vec::new(), Vec::new());
        }
        let mut out = Vec::new();
        for key in keys {
            let members = &groups[&key];
            let mut row = Solution::new();
            for (v, t) in variables.iter().zip(&key) {
                if let Some(t) = t {
                    row.insert(v.clone(), t.clone());
                }
            }
            for (v, a) in aggregates {
                if let Some(t) = self.aggregate(a, members, cx)? {
                    row.insert(v.clone(), t);
                }
            }
            out.push(row);
        }
        Ok(out)
    }

    /// An aggregate over the solutions of one group: `None` where the aggregate is an
    /// error.
    fn aggregate(&self, a: &AggregateExpression, members: &[Solution], cx: &Cx) -> R<Option<Term>> {
        use value::*;
        let (name, expr, distinct) = match a {
            AggregateExpression::CountSolutions { distinct } => {
                let n = if *distinct {
                    if members
                        .iter()
                        .any(|m| m.keys().any(|k| k.as_str().starts_with(":bnode:")))
                    {
                        let mut notes = self.notes.get();
                        notes.distinct_over_blank_nodes = true;
                        self.notes.set(notes);
                    }
                    members.iter().map(visible).collect::<HashSet<_>>().len()
                } else {
                    members.len()
                };
                return Ok(Some(int_term(n as i128)));
            }
            AggregateExpression::FunctionCall {
                name,
                expr,
                distinct,
            } => (name, expr, *distinct),
            AggregateExpression::Fold { .. } => return unsupported("FOLD (Jena)"),
        };
        if let AggregateFunction::Custom(n) = name {
            return unsupported(format!("aggregate {n}"));
        }
        // ListEval: the value of the expression for each solution, an error as `None`.
        let mut values: Vec<Option<Term>> = Vec::new();
        for s in members {
            values.push(self.soft(self.expr(expr, cx, s))?);
        }
        if distinct {
            let mut seen = HashSet::new();
            values.retain(|v| match v {
                Some(t) => seen.insert(t.clone()),
                None => true,
            });
        }
        let has_error = values.iter().any(Option::is_none);
        let terms: Vec<Term> = values.iter().flatten().cloned().collect();
        Ok(match name {
            AggregateFunction::Count => Some(int_term(terms.len() as i128)),
            AggregateFunction::Sum | AggregateFunction::Avg => {
                if has_error {
                    return Ok(None);
                }
                let mut sum = value::Num::Int(0);
                for t in &terms {
                    let Some(n) = numeric_term(t) else {
                        return Ok(None);
                    };
                    match sum.arith(Arith::Add, n) {
                        Some(s) => sum = s,
                        None => return Ok(None),
                    }
                }
                if matches!(name, AggregateFunction::Sum) {
                    Some(sum.to_term())
                } else if terms.is_empty() {
                    Some(int_term(0))
                } else {
                    sum.arith(Arith::Div, value::Num::Int(terms.len() as i128))
                        .map(Num::to_term)
                }
            }
            AggregateFunction::Min | AggregateFunction::Max => {
                // Min and Max sort the values in the order of ORDER BY, where an error
                // sorts with the unbound values, first. So an error is the minimum, and
                // the maximum passes over it.
                if has_error && !self.data.pass_min_max_errors {
                    let mut notes = self.notes.get();
                    notes.min_max_error = true;
                    self.notes.set(notes);
                }
                if has_error
                    && !self.data.pass_min_max_errors
                    && matches!(name, AggregateFunction::Min)
                {
                    return Ok(None);
                }
                let mut best: Option<&Term> = None;
                for t in &terms {
                    best = Some(match best {
                        None => t,
                        Some(b) => {
                            let o = total_order(Some(t), Some(b));
                            let better = if matches!(name, AggregateFunction::Min) {
                                o.is_lt()
                            } else {
                                o.is_gt()
                            };
                            if better { t } else { b }
                        }
                    });
                }
                // the choice is open when another value is not strictly worse
                if let Some(b) = best {
                    let worse = if matches!(name, AggregateFunction::Min) {
                        std::cmp::Ordering::Greater
                    } else {
                        std::cmp::Ordering::Less
                    };
                    if terms
                        .iter()
                        .any(|t| t != b && value::spec_order(Some(t), Some(b)) != Some(worse))
                    {
                        self.chose.set(true);
                    }
                }
                best.cloned()
            }
            AggregateFunction::Sample => {
                // any element of the values, an error among them
                if terms.iter().any(|t| Some(t) != terms.first()) || has_error && !terms.is_empty()
                {
                    self.chose.set(true);
                }
                terms.first().cloned()
            }
            AggregateFunction::GroupConcat { separator } => {
                if has_error {
                    return Ok(None);
                }
                let sep = separator.clone().unwrap_or_else(|| " ".to_string());
                let mut parts = Vec::new();
                for t in &terms {
                    parts.push(match t {
                        Term::Literal(l) => l.value().to_string(),
                        Term::NamedNode(n) => n.as_str().to_string(),
                        _ => return Ok(None),
                    });
                }
                // the order of the values is open
                if terms.iter().any(|t| Some(t) != terms.first()) {
                    self.chose.set(true);
                }
                // the result is a plain string, whatever the languages of the values
                Some(string_term(parts.join(&sep)))
            }
            AggregateFunction::Custom(_) => unreachable!(),
        })
    }

    // ------------------------------------------------------------------ CONSTRUCT ------

    fn construct(&self, template: &[TriplePattern], solutions: &[Solution]) -> Vec<Triple> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for s in solutions {
            // blank nodes of the template are new for each solution
            let mut fresh: HashMap<String, BlankNode> = HashMap::new();
            let n = self.fresh_id();
            let mut term =
                |p: &TermPattern| -> Option<Term> { self.template_term(p, s, n, &mut fresh) };
            for tp in template {
                let subject = term(&tp.subject);
                let object = term(&tp.object);
                let predicate = match &tp.predicate {
                    NamedNodePattern::NamedNode(n) => Some(n.clone()),
                    NamedNodePattern::Variable(v) => match s.get(v) {
                        Some(Term::NamedNode(n)) => Some(n.clone()),
                        _ => None,
                    },
                };
                let (Some(subject), Some(predicate), Some(object)) = (subject, predicate, object)
                else {
                    continue;
                };
                let subject: NamedOrBlankNode = match subject {
                    Term::NamedNode(n) => n.into(),
                    Term::BlankNode(b) => b.into(),
                    _ => continue,
                };
                let t = Triple::new(subject, predicate, object);
                if seen.insert(t.clone()) {
                    out.push(t);
                }
            }
        }
        out
    }

    fn template_term(
        &self,
        p: &TermPattern,
        s: &Solution,
        n: u64,
        fresh: &mut HashMap<String, BlankNode>,
    ) -> Option<Term> {
        match p {
            TermPattern::NamedNode(x) => Some(Term::NamedNode(x.clone())),
            TermPattern::Literal(l) => Some(Term::Literal(l.clone())),
            TermPattern::Variable(v) => s.get(v).cloned(),
            TermPattern::BlankNode(b) => Some(Term::BlankNode(
                fresh
                    .entry(b.as_str().to_string())
                    .or_insert_with(|| BlankNode::new_unchecked(format!("c{n}x{}", b.as_str())))
                    .clone(),
            )),
            TermPattern::Triple(tp) => {
                let subject = match self.template_term(&tp.subject, s, n, fresh)? {
                    Term::NamedNode(x) => NamedOrBlankNode::from(x),
                    Term::BlankNode(b) => NamedOrBlankNode::from(b),
                    _ => return None,
                };
                let predicate = match &tp.predicate {
                    NamedNodePattern::NamedNode(x) => x.clone(),
                    NamedNodePattern::Variable(v) => match s.get(v) {
                        Some(Term::NamedNode(x)) => x.clone(),
                        _ => return None,
                    },
                };
                let object = self.template_term(&tp.object, s, n, fresh)?;
                Some(Term::Triple(Box::new(Triple::new(
                    subject, predicate, object,
                ))))
            }
        }
    }
}

pub(crate) fn ground_term(t: &GroundTerm) -> Term {
    match t {
        GroundTerm::NamedNode(n) => Term::NamedNode(n.clone()),
        GroundTerm::Literal(l) => Term::Literal(l.clone()),
        GroundTerm::Triple(tr) => Term::Triple(Box::new(Triple::new(
            tr.subject.clone(),
            tr.predicate.clone(),
            ground_term(&tr.object),
        ))),
    }
}
