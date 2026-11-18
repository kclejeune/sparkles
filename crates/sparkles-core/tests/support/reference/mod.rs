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
//! answer that the specification allows. [`Reference::run_selected`] says which columns
//! and rows are open and reports the sort keys of an ordered answer, and
//! [`check::check_select`] compares an engine's answer with it.
//!
//! The file is included by `#[path]` into each test crate that uses it, so it carries
//! `allow(dead_code)`.
#![allow(dead_code, unused_imports)]

pub mod check;
mod expr;
mod path;
pub mod value;

use oxiri::Iri;
use oxrdf::{BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use oxrdf::{Variable, vocab::xsd};
use oxsdatatypes::DateTime;
use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression, Function, GraphPattern, OrderExpression,
    QueryDataset,
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
    by_subject: HashMap<Term, Vec<usize>>,
    by_object: HashMap<Term, Vec<usize>>,
}

impl GraphData {
    fn new(triples: Vec<Triple>) -> GraphData {
        let mut by_predicate: HashMap<NamedNode, Vec<usize>> = HashMap::new();
        let mut by_subject: HashMap<Term, Vec<usize>> = HashMap::new();
        let mut by_object: HashMap<Term, Vec<usize>> = HashMap::new();
        for (i, t) in triples.iter().enumerate() {
            by_predicate.entry(t.predicate.clone()).or_default().push(i);
            by_subject
                .entry(Term::from(t.subject.clone()))
                .or_default()
                .push(i);
            by_object.entry(t.object.clone()).or_default().push(i);
        }
        GraphData {
            triples,
            by_predicate,
            by_subject,
            by_object,
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

/// The graphs that evaluations have read, by the names merged into each and whether they
/// were merged apart.
type Graphs = HashMap<(Vec<GraphName>, bool), Rc<GraphData>>;

/// The reference evaluator over a dataset.
pub struct Reference {
    quads: Vec<Quad>,
    union_default: bool,
    aggregate_errors: Cell<AggregateErrors>,
    ignore_direction_in_compatibility: Cell<bool>,
    spec_readings: Cell<bool>,
    sparql11_ebv: Cell<bool>,
    /// The start of the labels of the blank nodes that the evaluation makes, which no
    /// blank node of the quads has, so a made blank node is fresh.
    fresh_prefix: String,
    work_limit: Option<u64>,
    now: DateTime,
    /// The graphs that evaluations have read, by the names merged into each, which every
    /// evaluation of these quads shares.
    graphs: RefCell<Graphs>,
}

impl Reference {
    /// A dataset of the quads, each once.
    pub fn new(quads: impl IntoIterator<Item = Quad>) -> Reference {
        let mut seen = HashSet::new();
        let quads: Vec<Quad> = quads
            .into_iter()
            .filter(|q| seen.insert(q.clone()))
            .collect();
        let mut labels = HashSet::new();
        for q in &quads {
            if let NamedOrBlankNode::BlankNode(b) = &q.subject {
                labels.insert(b.as_str().to_string());
            }
            blank_labels(&q.object, &mut labels);
            if let GraphName::BlankNode(b) = &q.graph_name {
                labels.insert(b.as_str().to_string());
            }
        }
        let mut fresh_prefix = String::from("ref");
        while labels.iter().any(|l| l.starts_with(&fresh_prefix)) {
            fresh_prefix.push('x');
        }
        Reference {
            quads,
            union_default: false,
            aggregate_errors: Cell::new(AggregateErrors::Spec),
            ignore_direction_in_compatibility: Cell::new(false),
            spec_readings: Cell::new(false),
            sparql11_ebv: Cell::new(false),
            fresh_prefix,
            work_limit: None,
            now: DateTime::now(),
            graphs: RefCell::new(HashMap::new()),
        }
    }

    /// Make the default graph the merge of the named graphs, as a store with a union
    /// default graph does in TDB2's scope. The stored default graph is then reachable only
    /// as `GRAPH <urn:x-arq:DefaultGraph>`.
    pub fn with_union_default(mut self, union: bool) -> Reference {
        self.union_default = union;
        self
    }

    /// Evaluate MIN and MAX over errors, AVG over errors only and GROUP_CONCAT over
    /// values that are not strings under `reading`, which [`AggregateErrors`] describes.
    pub fn with_aggregate_errors(self, reading: AggregateErrors) -> Reference {
        self.aggregate_errors.set(reading);
        self
    }

    /// Set the reading of [`Self::with_aggregate_errors`] for later evaluations, for an
    /// evaluator kept across queries.
    pub fn set_aggregate_errors(&self, reading: AggregateErrors) {
        self.aggregate_errors.set(reading);
    }

    /// Set whether later evaluations take two language-tagged string arguments of the same
    /// language and different base directions as compatible, a reading other than SPARQL
    /// 1.2's, which [`Notes::direction_in_compatibility`] describes.
    pub fn set_ignoring_direction_in_compatibility(&self, ignore: bool) {
        self.ignore_direction_in_compatibility.set(ignore);
    }

    /// Evaluate the questions where Jena's reading differs from the specification's text
    /// under the text, for later evaluations. By default the evaluator follows Jena, as
    /// the engine does. Under Jena's reading the default graph of several FROM clauses is
    /// the union of their graphs, which shares their blank nodes as a store does, a
    /// negated property set gives a pair once for each triple that joins it, and REGEX
    /// leaves `\b` and `\B` unsupported, since Java reads them as word boundaries. ORDER BY
    /// orders two different terms with equal values, such as `1` and `1.0`, by the terms
    /// themselves, an order the evaluator leaves open. Under the
    /// text the default graph is the RDF merge of section 13.2, which keeps the blank nodes
    /// of each graph apart from the others and from the named graphs, a negated property
    /// set gives each pair once, `\b` is an error, as XPath has no such escape, and two
    /// different terms with equal values tie, so the next ORDER BY condition orders them.
    pub fn with_spec_readings(self, spec: bool) -> Reference {
        self.spec_readings.set(spec);
        self
    }

    /// Read the effective boolean value of an ill-formed boolean or numeric literal as
    /// false, as SPARQL 1.1 section 17.2.2 does, where SPARQL 1.2 section 17.2.3, which
    /// the evaluator follows otherwise, makes it an error.
    pub fn with_sparql11_ebv(self, sparql11: bool) -> Reference {
        self.sparql11_ebv.set(sparql11);
        self
    }

    /// Refuse a query, as one the evaluator does not evaluate, once it has formed
    /// `limit` intermediate solutions, which keeps a nested loop over large inputs
    /// within a test's time and memory.
    pub fn with_work_limit(mut self, limit: u64) -> Reference {
        self.work_limit = Some(limit);
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

    /// Evaluate a parsed query, with the [`Notes`] on its answer. [`Notes::chose`] is set
    /// where the answer depends on a choice that the specification leaves open.
    pub fn evaluate_noting_choices(&self, query: &Query) -> Result<(Answer, Notes), Error> {
        let (answer, notes, selected) = self.evaluate_all(query)?;
        let mut notes = notes;
        if let Some(s) = &selected {
            notes.chose |= s.rows_open || s.open.iter().any(|o| *o);
        }
        Ok((answer, notes))
    }

    /// Parse and evaluate a SELECT, with what a comparison needs to allow every answer
    /// that the specification allows. Another query form gives `None`.
    pub fn run_selected(&self, text: &str, base: Option<&str>) -> Result<Option<Selected>, Error> {
        let query = parse(text, base)?;
        Ok(self.evaluate_all(&query)?.2)
    }

    /// Parse and evaluate an ASK, with the [`Notes`] on its answer, whose `chose` is set
    /// where an open value or choice decides it. Another query form gives `None`.
    pub fn run_ask(&self, text: &str, base: Option<&str>) -> Result<Option<(bool, Notes)>, Error> {
        let query = parse(text, base)?;
        let (answer, notes, _) = self.evaluate_all(&query)?;
        Ok(match answer {
            Answer::Ask(b) => Some((b, notes)),
            _ => None,
        })
    }

    fn evaluate_all(&self, query: &Query) -> Result<(Answer, Notes, Option<Selected>), Error> {
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
        value::take_beyond_minimum_precision();
        value::set_sparql11_ebv(self.sparql11_ebv.get());
        let ev = Evaluator::new(self, dataset.as_ref(), base.clone());
        let mut selected = None;
        let run = || -> R<(Answer, Option<Selected>)> {
            match query {
                Query::Select { .. } => {
                    let s = ev.eval_select(pattern)?;
                    let answer = Answer::Select {
                        variables: s.variables.clone(),
                        rows: s.rows.clone(),
                    };
                    Ok((answer, Some(s)))
                }
                Query::Ask { .. } => Ok((Answer::Ask(!ev.eval_top(pattern)?.is_empty()), None)),
                Query::Construct {
                    template,
                    graph_templates,
                    ..
                } => {
                    if !graph_templates.is_empty() {
                        return unsupported("CONSTRUCT with graph templates");
                    }
                    let solutions = ev.eval_top(pattern)?;
                    Ok((Answer::Graph(ev.construct(template, &solutions)), None))
                }
                Query::Describe { .. } => unsupported("DESCRIBE"),
            }
        };
        let (answer, sel) = run().map_err(|f| match f {
            Fail::Unsupported(m) => Error::Unsupported(m),
            Fail::Type => Error::Unsupported("an expression error outside any expression".into()),
        })?;
        let mut notes = ev.notes.get();
        notes.chose = ev.chose.get();
        notes.beyond_minimum_precision = value::take_beyond_minimum_precision();
        if let Some(mut s) = sel {
            s.notes = notes;
            selected = Some(s);
        }
        Ok((answer, notes, selected))
    }
}

fn parse(text: &str, base: Option<&str>) -> Result<Query, Error> {
    let mut parser = SparqlParser::new();
    if let Some(b) = base {
        parser = parser
            .with_base_iri(b)
            .map_err(|e| Error::Parse(e.to_string()))?;
    }
    parser
        .parse_query(text)
        .map_err(|e| Error::Parse(e.to_string()))
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
    /// Whether the default graph is the RDF merge of the graphs of FROM clauses.
    default_is_merge: bool,
    named_names: Vec<GraphName>,
    /// The solution that BNODE(label) is evaluated for.
    pub scope: Cell<u64>,
    pub bnodes: RefCell<HashMap<(u64, String), BlankNode>>,
    pub fresh: Cell<u64>,
    pub rng: Cell<u64>,
    /// Whether the evaluation made a choice that the specification leaves open.
    pub chose: Cell<bool>,
    pub notes: Cell<Notes>,
    work: Cell<u64>,
    /// The variables that hold open values somewhere in the evaluation.
    pub open_vars: RefCell<HashSet<Variable>>,
    /// Whether the expression being evaluated read an open value.
    pub read_open: Cell<bool>,
}

/// The readings of the aggregates where SPARQL, Jena and the engine differ, roadmap item
/// 1064: MIN and MAX over an error, AVG over values that are all errors and GROUP_CONCAT
/// over a value that is not a string, such as an IRI or a number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AggregateErrors {
    /// The specification's text. MIN and MAX sort the values in the order of ORDER BY,
    /// where an error comes first, so MIN is an error and MAX passes over it. AVG is zero
    /// where Count, which removes the errors, is zero. GROUP_CONCAT is CONCAT over the
    /// values, which makes a value that is not a string an error.
    #[default]
    Spec,
    /// Jena's reading. An error makes MIN, MAX and AVG errors, and GROUP_CONCAT reads
    /// the lexical form of any value.
    Jena,
    /// The engine's `sparkles` reading, its default before item 1064. MIN and MAX pass
    /// over errors, an error makes AVG an error, and GROUP_CONCAT reads the lexical form of
    /// any value.
    Sparkles,
}

/// What an evaluation noted about its answer beside the answer itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Notes {
    /// The specification allows other answers, because the evaluator made a choice
    /// that it leaves open.
    pub chose: bool,
    /// MIN or MAX read an error, where the readings of [`AggregateErrors`] differ.
    pub min_max_error: bool,
    /// `COUNT(DISTINCT *)` counted solutions of a pattern with blank nodes. Section
    /// 18.2.2.4 joins the two halves of a path sequence on a fresh variable, which is in
    /// the solutions, while a blank node of a basic graph pattern is not. The parser
    /// writes both as blank nodes, so the evaluator cannot tell them apart and leaves
    /// both out.
    pub distinct_over_blank_nodes: bool,
    /// The evaluation read an `xsd:integer` or `xsd:decimal` with more than 16 digits.
    /// XSD 1.1 section 5.4 lets an implementation limit such values, as long as it
    /// supports 16, so the evaluator's answer, which reads them exactly up to 38 digits,
    /// is one of those it allows.
    pub beyond_minimum_precision: bool,
    /// A string function had two language-tagged arguments of the same language whose
    /// base directions differ, which SPARQL 1.2 and Jena make incompatible arguments and
    /// an error. A test may compare with the reading that ignores the direction where the
    /// difference is tracked as an open defect.
    pub direction_in_compatibility: bool,
    /// AVG read only errors, where the readings of [`AggregateErrors`] differ.
    pub avg_of_errors: bool,
    /// GROUP_CONCAT read a value that is not a string, such as an IRI or a number, where
    /// the readings of [`AggregateErrors`] differ.
    pub group_concat_non_string: bool,
}

/// A SELECT answer with what a comparison needs to allow every answer that the
/// specification allows: which columns hold open values, which hold values that the
/// evaluation computed, and the order and slice of the rows.
#[derive(Debug, Clone)]
pub struct Selected {
    pub variables: Vec<String>,
    /// The answer.
    pub rows: Vec<Vec<Option<Term>>>,
    pub notes: Notes,
    /// An open value, or a choice that the specification leaves open, decides which rows
    /// there are, so the rows cannot be compared.
    pub rows_open: bool,
    /// For each column, whether its values are open, such as a SAMPLE, a RAND or a new
    /// blank node, so that only their place in the rows can be compared.
    pub open: Vec<bool>,
    /// For each column, whether the query computes its values, where an implementation
    /// chooses the lexical form of a number, or copies terms, which keep theirs.
    pub computed: Vec<Provenance>,
    /// The order and slice of the answer, when the query orders or slices it.
    pub order: Option<TopOrder>,
    /// The answer is ordered, and DISTINCT kept a row whose copies have different sort
    /// keys, so the order is not checked.
    pub order_unchecked: bool,
}

/// Where the values of a column come from, by the patterns and expressions that bind
/// its variable anywhere in the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Only terms of the data or the query, copied, which keep their lexical form.
    Copied,
    /// Only values that an operator or function computed, whose lexical form is the
    /// implementation's.
    Computed,
    /// Both, so a cell's lexical form may be fixed or free.
    Mixed,
}

/// The provenance of each variable that `p` binds. A variable is copied where a basic
/// graph pattern, a path, VALUES or a GRAPH name binds it, or an expression whose
/// outermost operator passes a term through or keeps the lexical form it is given: a
/// variable, a constant, COALESCE, IF, STRDT, STRLANG, STRLANGDIR and the MIN, MAX and
/// SAMPLE aggregates. It is computed where arithmetic,
/// a numeric function, a cast to a number or the COUNT, SUM and AVG aggregates bind it,
/// whose numbers take the implementation's lexical form. Any other operator binds it
/// both ways, so that its numbers compare exactly and a difference only in their
/// lexical form makes the comparison partial.
fn provenance(p: &GraphPattern) -> HashMap<Variable, Provenance> {
    #[derive(Default)]
    struct Site {
        copied: bool,
        computed: bool,
        from: Vec<Variable>,
    }
    fn expr_site(e: &Expression, site: &mut Site) {
        match e {
            Expression::Variable(v) => site.from.push(v.clone()),
            Expression::NamedNode(_) | Expression::Literal(_) => site.copied = true,
            Expression::Coalesce(list) => list.iter().for_each(|x| expr_site(x, site)),
            Expression::If(_, a, b) => {
                expr_site(a, site);
                expr_site(b, site);
            }
            Expression::Add(..)
            | Expression::Subtract(..)
            | Expression::Multiply(..)
            | Expression::Divide(..)
            | Expression::UnaryPlus(_)
            | Expression::UnaryMinus(_) => site.computed = true,
            Expression::FunctionCall(f, _) => match f {
                Function::Abs
                | Function::Ceil
                | Function::Floor
                | Function::Round
                | Function::Rand
                | Function::StrLen
                | Function::Year
                | Function::Month
                | Function::Day
                | Function::Hours
                | Function::Minutes
                | Function::Seconds => site.computed = true,
                Function::Custom(n)
                    if [xsd::INTEGER, xsd::DECIMAL, xsd::FLOAT, xsd::DOUBLE]
                        .iter()
                        .any(|d| d.as_str() == n.as_str()) =>
                {
                    site.computed = true
                }
                Function::StrDt | Function::StrLang | Function::StrLangDir => site.copied = true,
                _ => {
                    site.copied = true;
                    site.computed = true;
                }
            },
            _ => {
                site.copied = true;
                site.computed = true;
            }
        }
    }
    fn term_vars(t: &TermPattern, out: &mut Vec<Variable>) {
        match t {
            TermPattern::Variable(v) => out.push(v.clone()),
            TermPattern::Triple(tr) => {
                term_vars(&tr.subject, out);
                if let NamedNodePattern::Variable(v) = &tr.predicate {
                    out.push(v.clone());
                }
                term_vars(&tr.object, out);
            }
            _ => {}
        }
    }
    fn walk(p: &GraphPattern, sites: &mut Vec<(Variable, Site)>) {
        let copied = |v: Variable| {
            (
                v,
                Site {
                    copied: true,
                    ..Site::default()
                },
            )
        };
        match p {
            GraphPattern::Bgp { patterns } => {
                let mut vs = Vec::new();
                for tp in patterns {
                    term_vars(&tp.subject, &mut vs);
                    if let NamedNodePattern::Variable(v) = &tp.predicate {
                        vs.push(v.clone());
                    }
                    term_vars(&tp.object, &mut vs);
                }
                sites.extend(vs.into_iter().map(copied));
            }
            GraphPattern::Path {
                subject, object, ..
            } => {
                let mut vs = Vec::new();
                term_vars(subject, &mut vs);
                term_vars(object, &mut vs);
                sites.extend(vs.into_iter().map(copied));
            }
            GraphPattern::Values { variables, .. } => {
                sites.extend(variables.iter().cloned().map(copied));
            }
            GraphPattern::Graph { name, inner } => {
                if let NamedNodePattern::Variable(v) = name {
                    sites.push(copied(v.clone()));
                }
                walk(inner, sites);
            }
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } => {
                let mut site = Site::default();
                expr_site(expression, &mut site);
                sites.push((variable.clone(), site));
                walk(inner, sites);
            }
            GraphPattern::Group {
                inner, aggregates, ..
            } => {
                for (v, a) in aggregates {
                    let mut site = Site::default();
                    match a {
                        AggregateExpression::FunctionCall {
                            name:
                                AggregateFunction::Min
                                | AggregateFunction::Max
                                | AggregateFunction::Sample,
                            expr,
                            ..
                        } => expr_site(expr, &mut site),
                        AggregateExpression::CountSolutions { .. }
                        | AggregateExpression::FunctionCall {
                            name:
                                AggregateFunction::Count
                                | AggregateFunction::Sum
                                | AggregateFunction::Avg,
                            ..
                        } => site.computed = true,
                        _ => {
                            site.copied = true;
                            site.computed = true;
                        }
                    }
                    sites.push((v.clone(), site));
                }
                walk(inner, sites);
            }
            GraphPattern::Join { left, right }
            | GraphPattern::LeftJoin { left, right, .. }
            | GraphPattern::Lateral { left, right }
            | GraphPattern::Union { left, right }
            | GraphPattern::Minus { left, right } => {
                walk(left, sites);
                walk(right, sites);
            }
            GraphPattern::Filter { inner, .. }
            | GraphPattern::OrderBy { inner, .. }
            | GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner }
            | GraphPattern::Reduced { inner }
            | GraphPattern::Slice { inner, .. } => walk(inner, sites),
            // the patterns that the evaluator does not evaluate
            _ => {}
        }
    }
    let mut sites = Vec::new();
    walk(p, &mut sites);
    // a variable that copies another takes its provenance, to a fixed point
    let mut of: HashMap<Variable, (bool, bool)> = HashMap::new();
    loop {
        let mut changed = false;
        for (v, site) in &sites {
            let mut c = (site.copied, site.computed);
            for f in &site.from {
                // a variable that nothing binds is unbound, which copies nothing
                let (a, b) = of.get(f).copied().unwrap_or((false, false));
                c = (c.0 || a, c.1 || b);
            }
            let old = of.entry(v.clone()).or_default();
            let new = (old.0 || c.0, old.1 || c.1);
            if new != *old {
                *old = new;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    of.into_iter()
        .map(|(v, c)| {
            let p = match c {
                (_, false) => Provenance::Copied,
                (false, true) => Provenance::Computed,
                (true, true) => Provenance::Mixed,
            };
            (v, p)
        })
        .collect()
}

/// The rows of an ordered or sliced answer before its slice, in the order of ORDER BY,
/// each with its sort key, `None` where the key does not constrain the row's place.
#[derive(Debug, Clone)]
pub struct TopOrder {
    pub items: Vec<(Vec<Option<Term>>, Option<SortKey>)>,
    pub descending: Vec<bool>,
    /// Whether two different terms with equal values are unordered, as under Jena's
    /// reading, which [`strictly_before`] describes.
    pub term_ties: bool,
    /// Whether a row's key ends before a condition that read an open value. The rows
    /// that tie on the conditions before it may still be ordered by the ones after it,
    /// so the keys tell only which orders are forbidden.
    pub open_conditions: bool,
    pub offset: usize,
    pub limit: Option<usize>,
}

/// Solutions in ORDER BY order, with the sort key of each and the direction of each
/// key: what a slice needs to tell whether its cut is one the specification fixes.
struct Keyed {
    solutions: Vec<Solution>,
    /// The key of each solution, `None` where its place is open.
    keys: Option<(Vec<Option<SortKey>>, Vec<bool>)>,
    /// Whether DISTINCT kept a row whose copies have different keys, so that the order
    /// places it in one of several ranges, which the keys do not describe.
    unplaced: bool,
    /// Whether a key ends before a condition that read an open value.
    open_conditions: bool,
}

/// The values of the ORDER BY conditions for one solution, unbound where a condition is
/// an error or unbound. The key ends before the first condition that read an open
/// value, since that condition and the ones after it do not fix the row's place.
pub type SortKey = Vec<Option<Term>>;

/// Whether the specification puts the key `a` strictly before `b`. Two different terms
/// with equal values, such as `1` and `1.0`, are equal under the specification's text,
/// which then orders them by the next condition, and Jena orders them by the terms
/// themselves, so under Jena's reading, `term_ties`, they are unordered.
pub(crate) fn strictly_before(
    a: &[Option<Term>],
    b: &[Option<Term>],
    descending: &[bool],
    term_ties: bool,
) -> bool {
    for ((x, y), desc) in a.iter().zip(b).zip(descending) {
        let mut o = value::spec_order(x.as_ref(), y.as_ref());
        if term_ties && o == Some(std::cmp::Ordering::Equal) && x != y {
            o = None;
        }
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

/// The solutions of `right` that can be compatible with a solution of `left`, found by
/// the values of the variables that every solution of both binds. Compatible solutions
/// agree on those variables, so the candidates hold every compatible solution, in the
/// order of `right`, and the nested loop over them gives the same answer as over all of
/// `right` while trying fewer pairs.
struct Candidates {
    keys: Vec<Variable>,
    by_key: HashMap<Vec<Term>, Vec<usize>>,
    all: Vec<usize>,
}

impl Candidates {
    fn new(left: &[Solution], right: &[Solution]) -> Self {
        let all: Vec<usize> = (0..right.len()).collect();
        let keys: Vec<Variable> = match (left.first(), right.first()) {
            (Some(a), Some(_)) => a
                .keys()
                .filter(|k| **k != row_variable())
                .filter(|k| left.iter().all(|s| s.contains_key(*k)))
                .filter(|k| right.iter().all(|s| s.contains_key(*k)))
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        let mut by_key: HashMap<Vec<Term>, Vec<usize>> = HashMap::new();
        if !keys.is_empty() {
            for (i, b) in right.iter().enumerate() {
                by_key
                    .entry(keys.iter().map(|k| b[k].clone()).collect())
                    .or_default()
                    .push(i);
            }
        }
        Candidates { keys, by_key, all }
    }

    fn of(&self, a: &Solution) -> &[usize] {
        if self.keys.is_empty() {
            return &self.all;
        }
        let key: Vec<Term> = self.keys.iter().map(|k| a[k].clone()).collect();
        self.by_key.get(&key).map_or(&[], Vec::as_slice)
    }
}

pub(crate) fn merge(a: &Solution, b: &Solution) -> Solution {
    let mut out = a.clone();
    for (k, v) in b {
        out.insert(k.clone(), v.clone());
    }
    // The merge keeps the number that BNODE reads only where it is the numbered solution
    // itself, the other side binding nothing new. Otherwise it is a new solution, which a
    // later BNODE numbers again.
    let row = row_variable();
    let keeps = |numbered: &Solution, other: &Solution| {
        numbered.contains_key(&row)
            && !other.contains_key(&row)
            && other
                .keys()
                .all(|k| is_hidden(k) || numbered.contains_key(k))
    };
    if !(keeps(a, b) || keeps(b, a)) {
        out.remove(&row);
        if let (Some(x), Some(y)) = (a.get(&row), b.get(&row))
            && x == y
        {
            out.insert(row, x.clone());
        }
    }
    out
}

/// `t` with the label of each blank node prefixed by `prefix`.
fn rename_blank_nodes(t: &Triple, prefix: &str) -> Triple {
    let term = |x: &Term| -> Term {
        match x {
            Term::BlankNode(b) => {
                Term::BlankNode(BlankNode::new_unchecked(format!("{prefix}{}", b.as_str())))
            }
            Term::Triple(tr) => Term::Triple(Box::new(rename_blank_nodes(tr, prefix))),
            other => other.clone(),
        }
    };
    let subject = match term(&Term::from(t.subject.clone())) {
        Term::NamedNode(n) => NamedOrBlankNode::from(n),
        Term::BlankNode(b) => NamedOrBlankNode::from(b),
        _ => unreachable!("a subject is an IRI or a blank node"),
    };
    Triple::new(subject, t.predicate.clone(), term(&t.object))
}

/// The labels of the blank nodes in `t`, in triple terms too.
fn blank_labels(t: &Term, out: &mut HashSet<String>) {
    match t {
        Term::BlankNode(b) => {
            out.insert(b.as_str().to_string());
        }
        Term::Triple(tr) => {
            blank_labels(&Term::from(tr.subject.clone()), out);
            blank_labels(&tr.object, out);
        }
        _ => {}
    }
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
            None if data.union_default => (all_named.clone(), all_named),
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
        let default_is_merge = dataset.is_some() && default_names.len() > 1;
        Evaluator {
            data,
            base,
            now: data.now,
            default_names,
            default_is_merge,
            named_names,
            scope: Cell::new(0),
            bnodes: RefCell::new(HashMap::new()),
            fresh: Cell::new(0),
            rng: Cell::new(0x9e37_79b9_7f4a_7c15),
            chose: Cell::new(false),
            notes: Cell::new(Notes::default()),
            work: Cell::new(0),
            open_vars: RefCell::new(HashSet::new()),
            read_open: Cell::new(false),
        }
    }

    fn graph_of(&self, names: &[GraphName]) -> Rc<GraphData> {
        self.graph_of_merging(names, false)
    }

    /// The graph of the triples of the graphs `names`. Where `merge` asks for their RDF
    /// merge under the specification's reading, the blank nodes of each graph are renamed
    /// apart, as section 13.2 makes the default graph of several FROM clauses the RDF merge
    /// of their graphs, which shares no blank node with another graph or with a named
    /// graph. Jena's reading, the default, takes their union.
    fn graph_of_merging(&self, names: &[GraphName], merge: bool) -> Rc<GraphData> {
        let merge = merge && names.len() > 1 && self.data.spec_readings.get();
        let key = (names.to_vec(), merge);
        if let Some(g) = self.data.graphs.borrow().get(&key) {
            return g.clone();
        }
        let mut seen = HashSet::new();
        let mut triples = Vec::new();
        for q in &self.data.quads {
            let Some(g) = names.iter().position(|n| *n == q.graph_name) else {
                continue;
            };
            let mut t = Triple::new(q.subject.clone(), q.predicate.clone(), q.object.clone());
            if merge {
                let prefix = format!("{}m{g}x", self.data.fresh_prefix);
                t = rename_blank_nodes(&t, &prefix);
            }
            if seen.insert(t.clone()) {
                triples.push(t);
            }
        }
        let g = Rc::new(GraphData::new(triples));
        self.data.graphs.borrow_mut().insert(key, g.clone());
        g
    }

    pub(crate) fn fresh_id(&self) -> u64 {
        let n = self.fresh.get();
        self.fresh.set(n + 1);
        n
    }

    fn eval_top(&self, p: &GraphPattern) -> R<Vec<Solution>> {
        let default = self.graph_of_merging(&self.default_names.clone(), self.default_is_merge);
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
        let out = self.eval_inner(p, cx)?;
        self.charge(out.len())?;
        Ok(out)
    }

    /// Count `n` intermediate solutions against the work limit.
    fn charge(&self, n: usize) -> R<()> {
        let work = self.work.get() + n as u64;
        self.work.set(work);
        match self.data.work_limit {
            Some(limit) if work > limit => {
                unsupported(format!("more than {limit} intermediate solutions"))
            }
            _ => Ok(()),
        }
    }

    fn eval_inner(&self, p: &GraphPattern, cx: &Cx) -> R<Vec<Solution>> {
        match p {
            GraphPattern::Bgp { patterns } => {
                let mut sols = vec![Solution::new()];
                for tp in patterns {
                    let mut next = Vec::new();
                    for s in &sols {
                        self.match_triple_pattern(tp, s, cx, &mut next);
                        self.charge(1)?;
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
                if self.shares_open(left, right) {
                    self.chose.set(true);
                }
                let candidates = Candidates::new(&l, &r);
                let mut out = Vec::new();
                for a in &l {
                    // every pair the nested loop tries is work
                    let tried = candidates.of(a);
                    self.charge(tried.len())?;
                    for b in tried.iter().map(|&i| &r[i]) {
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
                if self.shares_open(left, right) {
                    self.chose.set(true);
                }
                let candidates = Candidates::new(&l, &r);
                let mut out = Vec::new();
                for a in &l {
                    let tried = candidates.of(a);
                    self.charge(tried.len())?;
                    let mut matched = false;
                    for b in tried.iter().map(|&i| &r[i]) {
                        if !compatible(a, b) {
                            continue;
                        }
                        let m = merge(a, b);
                        let keep = match expression {
                            None => true,
                            Some(e) => {
                                let (keep, open) = self.tracking(|| self.truthy(e, cx, &m));
                                if open {
                                    self.chose.set(true);
                                }
                                keep?
                            }
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
                let l = self.eval(left, cx)?;
                // the right side reads the left's values, open ones too
                if self.binds_open(left) {
                    self.chose.set(true);
                }
                for a in l {
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
                    let (keep, open) = self.tracking(|| self.truthy(expr, cx, &s));
                    if open {
                        self.chose.set(true);
                    }
                    if keep? {
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
                let numbers = reads_bnode(expression);
                let mut open = false;
                for mut s in self.eval(inner, cx)? {
                    // BNODE(label) is the same blank node within one solution, so a solution
                    // that an expression with BNODE reads gets a number to key it by.
                    if numbers && !s.contains_key(&row_variable()) {
                        let n = self.fresh_id();
                        s.insert(row_variable(), int_term(n as i128));
                    }
                    let (v, read_open) = self.tracking(|| self.soft(self.expr(expression, cx, &s)));
                    open |= read_open;
                    if let Some(v) = v? {
                        if s.get(variable).is_some_and(|old| *old != v) {
                            // a variable already in scope cannot be extended
                            return unsupported("BIND of a variable that is in scope");
                        }
                        s.insert(variable.clone(), v);
                    }
                    out.push(s);
                }
                if open {
                    self.mark_open(variable);
                }
                Ok(out)
            }
            GraphPattern::Minus { left, right } => {
                let (l, r) = (self.eval(left, cx)?, self.eval(right, cx)?);
                if self.shares_open(left, right) {
                    self.chose.set(true);
                }
                let candidates = Candidates::new(&l, &r);
                let mut out = Vec::new();
                for a in &l {
                    let tried = candidates.of(a);
                    self.charge(tried.len())?;
                    if !tried.iter().map(|&i| &r[i]).any(|b| {
                        compatible(a, b) && a.keys().any(|k| !is_hidden(k) && b.contains_key(k))
                    }) {
                        out.push(a.clone());
                    }
                }
                Ok(out)
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
                    // the solution substituted into the pattern may lack an open value that
                    // another answer binds
                    if s.keys().any(|k| self.open_vars.borrow().contains(k)) {
                        self.chose.set(true);
                    }
                    if compatible(&s, cx.env) {
                        out.push(s);
                    }
                }
                Ok(out)
            }
            GraphPattern::OrderBy { .. }
            | GraphPattern::Project { .. }
            | GraphPattern::Distinct { .. }
            | GraphPattern::Reduced { .. } => Ok(self.eval_keyed(p, cx)?.solutions),
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => {
                let Keyed {
                    solutions, keys, ..
                } = self.eval_keyed(inner, cx)?;
                let n = solutions.len();
                let from = (*start).min(n);
                let to = length.map_or(n, |l| from.saturating_add(l).min(n));
                self.note_open_cuts(&solutions, keys.as_ref(), from, to);
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

    /// Note a choice where a slice of `solutions` from `from` to `to` cuts between rows
    /// that the specification does not order. A cut is fixed when every row before it
    /// either equals every row after it or comes strictly before it in the order.
    fn note_open_cuts(
        &self,
        solutions: &[Solution],
        keys: Option<&(Vec<Option<SortKey>>, Vec<bool>)>,
        from: usize,
        to: usize,
    ) {
        let n = solutions.len();
        let visible_rows: Vec<Solution> = solutions.iter().map(visible).collect();
        for cut in [from, to] {
            if cut == 0 || cut == n {
                continue;
            }
            let fixed = (0..cut).all(|i| {
                (cut..n).all(|j| {
                    visible_rows[i] == visible_rows[j]
                        || keys.is_some_and(|(k, d)| match (&k[i], &k[j]) {
                            (Some(a), Some(b)) => {
                                strictly_before(a, b, d, !self.data.spec_readings.get())
                            }
                            _ => false,
                        })
                })
            });
            if !fixed {
                self.chose.set(true);
            }
        }
    }

    /// Evaluate the pattern of a SELECT, keeping the order and the slice of its answer
    /// and the columns that hold open or computed values.
    fn eval_select(&self, p: &GraphPattern) -> R<Selected> {
        let variables = projected(p);
        let default = self.graph_of_merging(&self.default_names.clone(), self.default_is_merge);
        let env = Solution::new();
        let cx = Cx {
            graph: &default,
            env: &env,
        };
        let (inner, slice) = match p {
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => (inner.as_ref(), Some((*start, *length))),
            other => (other, None),
        };
        let Keyed {
            solutions,
            keys,
            unplaced,
            open_conditions,
        } = self.eval_keyed(inner, &cx)?;
        // A top slice's cuts are the comparison's to check, so they do not make the rows
        // open. A row that DISTINCT kept from copies with different keys has a place that
        // the keys do not describe, so a slice of the rows is open, and their order is not
        // checked.
        let rows_open = self.chose.get() || unplaced && slice.is_some();
        let order_unchecked = unplaced && keys.is_some();
        let n = solutions.len();
        let (from, to) = match slice {
            Some((start, length)) => {
                let from = start.min(n);
                (from, length.map_or(n, |l| from.saturating_add(l).min(n)))
            }
            None => (0, n),
        };
        if slice.is_some() {
            self.note_open_cuts(&solutions, keys.as_ref(), from, to);
        }
        let row_of = |s: &Solution| -> Vec<Option<Term>> {
            variables.iter().map(|v| s.get(v).cloned()).collect()
        };
        let order = if (keys.is_some() || slice.is_some()) && !order_unchecked {
            let (ks, descending) = keys.unwrap_or_else(|| (vec![None; n], Vec::new()));
            Some(TopOrder {
                items: solutions
                    .iter()
                    .zip(ks)
                    .map(|(s, k)| (row_of(s), k))
                    .collect(),
                descending,
                term_ties: !self.data.spec_readings.get(),
                open_conditions,
                offset: slice.map_or(0, |s| s.0),
                limit: slice.and_then(|s| s.1),
            })
        } else {
            None
        };
        let rows = solutions[from..to].iter().map(row_of).collect();
        let open_vars = self.open_vars.borrow();
        let provenance = provenance(p);
        Ok(Selected {
            variables: variables.iter().map(|v| v.as_str().to_string()).collect(),
            rows,
            notes: Notes::default(),
            rows_open,
            open: variables.iter().map(|v| open_vars.contains(v)).collect(),
            computed: variables
                .iter()
                .map(|v| provenance.get(v).copied().unwrap_or(Provenance::Copied))
                .collect(),
            order,
            order_unchecked,
        })
    }

    /// Whether one of the open variables is in scope of both `left` and `right`, where
    /// its value, or whether it is bound at all, decides which solutions are compatible.
    fn shares_open(&self, left: &GraphPattern, right: &GraphPattern) -> bool {
        let open = self.open_vars.borrow();
        if open.is_empty() {
            return false;
        }
        let (l, r) = (in_scope(left), in_scope(right));
        open.iter().any(|v| l.contains(v) && r.contains(v))
    }

    /// Whether one of the open variables is in scope of `p`, so that its solutions may
    /// bind it or not, or bind it to another value, in another answer.
    fn binds_open(&self, p: &GraphPattern) -> bool {
        let open = self.open_vars.borrow();
        !open.is_empty() && in_scope(p).iter().any(|v| open.contains(v))
    }

    /// Whether `p` mentions one of the open variables anywhere, in a FILTER too, so that
    /// substituting a solution into it reads an open value or an open absence of one.
    pub(crate) fn mentions_open(&self, p: &GraphPattern) -> bool {
        let open = self.open_vars.borrow();
        if open.is_empty() {
            return false;
        }
        let text = p.to_string();
        open.iter().any(|v| {
            let name = format!("?{}", v.as_str());
            text.match_indices(&name).any(|(i, _)| {
                !text[i + name.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
            })
        })
    }

    /// Run `f` and tell whether the expressions it evaluated read an open value.
    pub(crate) fn tracking<T>(&self, f: impl FnOnce() -> T) -> (T, bool) {
        let outer = self.read_open.replace(false);
        let r = f();
        let open = self.read_open.get();
        self.read_open.set(outer || open);
        (r, open)
    }

    /// Note that `v` holds open values.
    fn mark_open(&self, v: &Variable) {
        self.open_vars.borrow_mut().insert(v.clone());
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
                    // the conditions from the first that reads an open value on do not fix
                    // the row's place
                    let mut fixed = expression.len();
                    for (i, o) in expression.iter().enumerate() {
                        let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = o;
                        let (k, open) = self.tracking(|| self.soft(self.expr(e, cx, &s)));
                        keys.push(k?);
                        if open && fixed == expression.len() {
                            fixed = i;
                        }
                    }
                    keyed.push((keys, fixed, s));
                }
                let descending: Vec<bool> = expression
                    .iter()
                    .map(|o| matches!(o, OrderExpression::Desc(_)))
                    .collect();
                keyed.sort_by(|(a, _, _), (b, _, _)| {
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
                let open_conditions = keyed.iter().any(|(_, fixed, _)| *fixed < expression.len());
                let (keys, solutions) = keyed
                    .into_iter()
                    .map(|(mut k, fixed, s)| {
                        k.truncate(fixed);
                        (Some(k), s)
                    })
                    .unzip();
                Ok(Keyed {
                    solutions,
                    keys: Some((keys, descending)),
                    unplaced: false,
                    open_conditions,
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
                let Keyed {
                    solutions,
                    keys,
                    unplaced: unplaced_any,
                    open_conditions,
                } = self.eval_keyed(inner, cx)?;
                // open values may be equal or not in another answer
                if self.binds_open(inner) {
                    self.chose.set(true);
                }
                let mut first: HashMap<Solution, usize> = HashMap::new();
                let mut kept = Vec::new();
                let mut unplaced = HashSet::new();
                for (i, s) in solutions.iter().enumerate() {
                    match first.get(&visible(s)) {
                        None => {
                            first.insert(visible(s), i);
                            kept.push(i);
                        }
                        // Which copy DISTINCT keeps is open, and it places the row
                        // differently when the copies have different keys, so the kept
                        // row's key does not fix its place.
                        Some(&f) => {
                            if let Some((k, _)) = &keys
                                && k[f] != k[i]
                            {
                                unplaced.insert(f);
                            }
                        }
                    }
                }
                Ok(Keyed {
                    unplaced: unplaced_any || !unplaced.is_empty(),
                    open_conditions,
                    solutions: kept.iter().map(|&i| solutions[i].clone()).collect(),
                    keys: keys.map(|(k, d)| {
                        let k = kept
                            .iter()
                            .map(|&i| {
                                if unplaced.contains(&i) {
                                    None
                                } else {
                                    k[i].clone()
                                }
                            })
                            .collect();
                        (k, d)
                    }),
                })
            }
            // REDUCED may drop duplicates and need not, and this one keeps them, so an
            // answer with duplicates is one of several
            GraphPattern::Reduced { inner } => {
                let k = self.eval_keyed(inner, cx)?;
                let mut seen = HashSet::new();
                if self.binds_open(inner) || !k.solutions.iter().all(|s| seen.insert(visible(s))) {
                    self.chose.set(true);
                }
                Ok(k)
            }
            _ => Ok(Keyed {
                solutions: self.eval(p, cx)?,
                keys: None,
                unplaced: false,
                open_conditions: false,
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
            // Jena's names for the union of the named graphs and for the default graph
            NamedNodePattern::NamedNode(n)
                if matches!(
                    n.as_str(),
                    "urn:x-arq:UnionGraph" | "urn:x-arq:DefaultGraph"
                ) =>
            {
                let names = if n.as_str() == "urn:x-arq:UnionGraph" {
                    self.named_names.clone()
                } else {
                    vec![GraphName::DefaultGraph]
                };
                let data = self.graph_of(&names);
                out = self.eval(
                    inner,
                    &Cx {
                        graph: &data,
                        env: cx.env,
                    },
                )?;
            }
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
                // The graph's name joins the inner pattern's binding of the variable,
                // so an open value there decides which solutions the join keeps.
                if self.open_vars.borrow().contains(v) && in_scope(inner).contains(v) {
                    self.chose.set(true);
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
        // An index only narrows the scan to the triples that could match, and every
        // candidate is matched in full, so the answer is the scan's.
        let predicate = match &tp.predicate {
            NamedNodePattern::NamedNode(n) => Some(n.clone()),
            NamedNodePattern::Variable(v) => match s.get(v).or_else(|| cx.env.get(v)) {
                Some(Term::NamedNode(n)) => Some(n.clone()),
                Some(_) => return,
                None => None,
            },
        };
        let known = |p: &TermPattern| -> Option<Term> {
            match p {
                TermPattern::NamedNode(n) => Some(Term::NamedNode(n.clone())),
                TermPattern::Literal(l) => Some(Term::Literal(l.clone())),
                TermPattern::Variable(v) => s.get(v).or_else(|| cx.env.get(v)).cloned(),
                TermPattern::BlankNode(b) => s.get(&hidden_bnode_variable(b)).cloned(),
                TermPattern::Triple(_) => None,
            }
        };
        let mut candidates: Vec<&[usize]> = Vec::new();
        if let Some(p) = &predicate {
            candidates.push(cx.graph.by_predicate.get(p).map_or(&[], Vec::as_slice));
        }
        if let Some(t) = known(&tp.subject) {
            candidates.push(cx.graph.by_subject.get(&t).map_or(&[], Vec::as_slice));
        }
        if let Some(t) = known(&tp.object) {
            candidates.push(cx.graph.by_object.get(&t).map_or(&[], Vec::as_slice));
        }
        let mut try_one = |t: &Triple| {
            let mut m = s.clone();
            if self.match_triple(tp, t, &mut m, cx) {
                out.push(m);
            }
        };
        match candidates.into_iter().min_by_key(|c| c.len()) {
            Some(c) => {
                for &i in c {
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
        let (fixed_subject, fixed_object) = (fixed(subject, cx), fixed(object, cx));
        let pairs = match self.anchored_closure(
            path,
            fixed_subject.as_ref(),
            fixed_object.as_ref(),
            cx.graph,
            &extra,
        )? {
            Some(pairs) => pairs,
            None => self.path_pairs(path, cx.graph, &extra)?,
        };
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
        let mut sols = self.eval(inner, cx)?;
        let inner_open = self.binds_open(inner);
        // BNODE(label) is the same blank node for one solution, whichever aggregate reads
        // it, so each solution that an aggregate with BNODE reads is numbered once
        let numbers = aggregates.iter().any(|(_, a)| match a {
            AggregateExpression::FunctionCall { expr, .. } => reads_bnode(expr),
            _ => false,
        });
        if numbers {
            for s in &mut sols {
                s.entry(row_variable())
                    .or_insert_with(|| int_term(self.fresh_id() as i128));
            }
        }
        // a group key with open values may group the solutions otherwise in another answer
        if variables
            .iter()
            .any(|v| self.open_vars.borrow().contains(v))
        {
            self.chose.set(true);
        }
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
                if let Some(t) = self.aggregate(v, a, members, inner_open, cx)? {
                    row.insert(v.clone(), t);
                }
            }
            out.push(row);
        }
        Ok(out)
    }

    /// An aggregate over the solutions of one group, into `out`: `None` where the
    /// aggregate is an error. An aggregate that reads an open value, or picks a value that
    /// the specification leaves open, makes `out` open.
    fn aggregate(
        &self,
        out: &Variable,
        a: &AggregateExpression,
        members: &[Solution],
        inner_open: bool,
        cx: &Cx,
    ) -> R<Option<Term>> {
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
                        self.mark_open(out);
                    }
                    if inner_open {
                        self.mark_open(out);
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
        // BNODE(label) gives each solution its own blank node.
        let numbers = reads_bnode(expr);
        let mut values: Vec<Option<Term>> = Vec::new();
        let (r, open) = self.tracking(|| -> R<()> {
            for s in members {
                let v = if numbers && !s.contains_key(&row_variable()) {
                    let mut s = s.clone();
                    s.insert(row_variable(), int_term(self.fresh_id() as i128));
                    self.soft(self.expr(expr, cx, &s))?
                } else {
                    self.soft(self.expr(expr, cx, s))?
                };
                values.push(v);
            }
            Ok(())
        });
        r?;
        if open {
            self.mark_open(out);
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
            // Count removes the errors
            AggregateFunction::Count => Some(int_term(terms.len() as i128)),
            AggregateFunction::Sum | AggregateFunction::Avg => {
                // Avg is zero where Count, which removes the errors, is zero, and Sum
                // adds every value, so an error makes it an error
                if matches!(name, AggregateFunction::Avg) && terms.is_empty() {
                    if has_error {
                        let mut notes = self.notes.get();
                        notes.avg_of_errors = true;
                        self.notes.set(notes);
                        if self.data.aggregate_errors.get() != AggregateErrors::Spec {
                            return Ok(None);
                        }
                    }
                    return Ok(Some(int_term(0)));
                }
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
                } else {
                    sum.arith(Arith::Div, value::Num::Int(terms.len() as i128))
                        .map(Num::to_term)
                }
            }
            AggregateFunction::Min | AggregateFunction::Max => {
                // Min and Max sort the values in the order of ORDER BY, where an error
                // sorts with the unbound values, first. So an error is the minimum, and
                // the maximum passes over it.
                if has_error {
                    let mut notes = self.notes.get();
                    notes.min_max_error = true;
                    self.notes.set(notes);
                    let error = match self.data.aggregate_errors.get() {
                        AggregateErrors::Spec => matches!(name, AggregateFunction::Min),
                        AggregateErrors::Jena => true,
                        AggregateErrors::Sparkles => false,
                    };
                    if error {
                        return Ok(None);
                    }
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
                        self.mark_open(out);
                    }
                }
                best.cloned()
            }
            AggregateFunction::Sample => {
                // any element of the values, an error among them
                if terms.iter().any(|t| Some(t) != terms.first()) || has_error && !terms.is_empty()
                {
                    self.mark_open(out);
                }
                terms.first().cloned()
            }
            AggregateFunction::GroupConcat { separator } => {
                if has_error {
                    return Ok(None);
                }
                // GroupConcat is CONCAT over the values, which takes only strings. Jena
                // reads the lexical form of any value.
                let sep = separator.clone().unwrap_or_else(|| " ".to_string());
                let mut parts = Vec::new();
                for t in &terms {
                    let string = matches!(t, Term::Literal(l) if is_string_literal(l));
                    if !string {
                        let mut notes = self.notes.get();
                        notes.group_concat_non_string = true;
                        self.notes.set(notes);
                        if self.data.aggregate_errors.get() == AggregateErrors::Spec {
                            return Ok(None);
                        }
                    }
                    parts.push(match t {
                        Term::Literal(l) => l.value().to_string(),
                        Term::NamedNode(n) => n.as_str().to_string(),
                        _ => return Ok(None),
                    });
                }
                // the order of the values is open
                if terms.iter().any(|t| Some(t) != terms.first()) {
                    self.mark_open(out);
                }
                // CONCAT with the separator, or with "" for one value, makes a simple
                // string whatever the languages of the values
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
                    .or_insert_with(|| {
                        BlankNode::new_unchecked(format!(
                            "{}c{n}x{}",
                            self.data.fresh_prefix,
                            b.as_str()
                        ))
                    })
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

/// Whether an expression calls BNODE, which needs each solution numbered.
/// The variables in scope of `p`, the only ones that its solutions can bind.
fn in_scope(p: &GraphPattern) -> HashSet<Variable> {
    let mut out = HashSet::new();
    p.on_in_scope_variable(|v| {
        out.insert(v.clone());
    });
    out
}

fn reads_bnode(e: &Expression) -> bool {
    format!("{e:?}").contains("BNode")
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
