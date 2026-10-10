//! `why_empty` and `POST /{ds}/sparql/diagnose` (C18 §9.2): why a query has no
//! solutions in the caller's view.
//!
//! The query's required part is cut into steps in the order it is written: each
//! triple pattern or path, each FILTER, each BIND and VALUES block, and each UNION as
//! a whole. Every pattern is first asked alone, then the patterns are joined one by
//! one with the filters, BINDs and VALUES blocks in place, each check an `ASK` under a
//! tenth of the call's timeout. The first pattern, join or filter without solutions is
//! the answer, with whether its constants occur in the view at all and the
//! `check_query` issues about them. Nothing here writes, and every check reads as the
//! caller over the same snapshot.

use super::{Reader, exists_term};
use crate::mcp::Outcome;
use crate::mcp::errors::ToolError;
use crate::mcp::render::{Prefixes, Terms};
use crate::mcp::tools::{Tools, dataset_prefixes, parse};
use oxrdf::Term;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use spargebra::Query;
use spargebra::algebra::{Expression, GraphPattern, PropertyPathExpression};
use spargebra::term::{NamedNodePattern, TermPattern};
use sparkles::error::Error;
use sparkles::sparql::{self, QueryOptions};
use sparkles::store::Snapshot;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WhyArgs {
    dataset: Option<String>,
    query: String,
    reasoning: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
    timeout_seconds: Option<f64>,
}

/// The most steps a diagnosis checks.
const MAX_STEPS: usize = 64;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// a triple pattern, a path or a UNION: asked alone, then joined
    Pattern,
    /// a FILTER over the steps before it
    Filter,
    /// a BIND or a VALUES block: applied in the joins, never asked alone
    Apply,
}

struct Step {
    kind: Kind,
    /// the pattern alone (a pattern step), with its GRAPH
    alone: Option<GraphPattern>,
    /// how the step joins the steps before it
    apply: Box<dyn Fn(GraphPattern) -> GraphPattern>,
    text: String,
    constants: Vec<Term>,
}

/// The steps of a pattern and whether they cover every way it can lose solutions.
struct Cut<'t, 'p> {
    steps: Vec<Step>,
    /// parts that are not checked (OPTIONAL right sides cannot empty a result, MINUS,
    /// subqueries with LIMIT, HAVING and SERVICE can)
    unchecked: Vec<&'static str>,
    terms: &'t mut Terms<'p>,
}

fn graph_of(inner: GraphPattern, g: &Option<NamedNodePattern>) -> GraphPattern {
    match g {
        Some(name) => GraphPattern::Graph {
            name: name.clone(),
            inner: Box::new(inner),
        },
        None => inner,
    }
}

fn join(left: GraphPattern, right: GraphPattern) -> GraphPattern {
    match left {
        GraphPattern::Bgp { patterns } if patterns.is_empty() => right,
        left => GraphPattern::Join {
            left: Box::new(left),
            right: Box::new(right),
        },
    }
}

impl Cut<'_, '_> {
    fn term(&mut self, t: &TermPattern, constants: &mut Vec<Term>) -> String {
        match t {
            TermPattern::Variable(v) => format!("?{}", v.as_str()),
            TermPattern::NamedNode(n) => {
                constants.push(Term::NamedNode(n.clone()));
                self.terms.iri(n.as_str())
            }
            TermPattern::Literal(l) => {
                constants.push(Term::Literal(l.clone()));
                self.terms.term(&Term::Literal(l.clone()))
            }
            t => t.to_string(),
        }
    }

    fn path(&mut self, p: &PropertyPathExpression, constants: &mut Vec<Term>) -> String {
        match p {
            PropertyPathExpression::NamedNode(n) => {
                constants.push(Term::NamedNode(n.clone()));
                let t = self.terms.iri(n.as_str());
                if n.as_str() == super::RDF_TYPE {
                    "a".into()
                } else {
                    t
                }
            }
            p => p.to_string(),
        }
    }

    fn in_graph(&mut self, text: String, g: &Option<NamedNodePattern>) -> String {
        match g {
            Some(NamedNodePattern::NamedNode(n)) => {
                format!("GRAPH {} {{ {text} }}", self.terms.iri(n.as_str()))
            }
            Some(NamedNodePattern::Variable(v)) => format!("GRAPH ?{} {{ {text} }}", v.as_str()),
            None => text,
        }
    }

    fn pattern(
        &mut self,
        alone: GraphPattern,
        text: String,
        constants: Vec<Term>,
        g: &Option<NamedNodePattern>,
    ) {
        let alone = graph_of(alone, g);
        let text = self.in_graph(text, g);
        let a = alone.clone();
        self.steps.push(Step {
            kind: Kind::Pattern,
            alone: Some(alone),
            apply: Box::new(move |cum| join(cum, a.clone())),
            text,
            constants,
        });
    }

    fn walk(&mut self, p: &GraphPattern, g: &Option<NamedNodePattern>) {
        use GraphPattern as G;
        match p {
            G::Bgp { patterns } => {
                for tp in patterns {
                    let mut c = Vec::new();
                    let s = self.term(&tp.subject, &mut c);
                    let pr = match &tp.predicate {
                        NamedNodePattern::NamedNode(n) => {
                            c.push(Term::NamedNode(n.clone()));
                            if n.as_str() == super::RDF_TYPE {
                                "a".into()
                            } else {
                                self.terms.iri(n.as_str())
                            }
                        }
                        NamedNodePattern::Variable(v) => format!("?{}", v.as_str()),
                    };
                    let o = self.term(&tp.object, &mut c);
                    let alone = G::Bgp {
                        patterns: vec![tp.clone()],
                    };
                    self.pattern(alone, format!("{s} {pr} {o}"), c, g);
                }
            }
            G::Path {
                subject,
                path,
                object,
            } => {
                let mut c = Vec::new();
                let s = self.term(subject, &mut c);
                let pr = self.path(path, &mut c);
                let o = self.term(object, &mut c);
                self.pattern(p.clone(), format!("{s} {pr} {o}"), c, g);
            }
            G::Join { left, right } => {
                self.walk(left, g);
                self.walk(right, g);
            }
            // the optional part never removes a solution of the required one
            G::LeftJoin { left, .. } => self.walk(left, g),
            G::Graph { name, inner } => self.walk(inner, &Some(name.clone())),
            G::Filter { expr, inner } => {
                self.walk(inner, g);
                let e = expr.clone();
                let mut c = Vec::new();
                expression_constants(expr, &mut c);
                self.steps.push(Step {
                    kind: Kind::Filter,
                    alone: None,
                    apply: Box::new(move |cum| G::Filter {
                        expr: e.clone(),
                        inner: Box::new(cum),
                    }),
                    text: format!("FILTER({expr})"),
                    constants: c,
                });
            }
            G::Extend {
                inner,
                variable,
                expression,
            } => {
                self.walk(inner, g);
                let (v, e) = (variable.clone(), expression.clone());
                self.steps.push(Step {
                    kind: Kind::Apply,
                    alone: None,
                    apply: Box::new(move |cum| G::Extend {
                        inner: Box::new(cum),
                        variable: v.clone(),
                        expression: e.clone(),
                    }),
                    text: format!("BIND({expression} AS ?{})", variable.as_str()),
                    constants: Vec::new(),
                });
            }
            G::Values { .. } => {
                let v = p.clone();
                self.steps.push(Step {
                    kind: Kind::Apply,
                    alone: None,
                    apply: Box::new(move |cum| join(cum, v.clone())),
                    text: "VALUES".into(),
                    constants: Vec::new(),
                });
            }
            G::Union { .. } => {
                let mut c = Vec::new();
                crate::mcp::tools::constant_terms(p, &mut c);
                self.pattern(p.clone(), "UNION { … }".into(), c, g);
            }
            G::Project { inner, .. }
            | G::Distinct { inner }
            | G::Reduced { inner }
            | G::OrderBy { inner, .. } => self.walk(inner, g),
            G::Slice { inner, start, .. } => {
                if *start > 0 {
                    self.unchecked.push("OFFSET");
                }
                self.walk(inner, g);
            }
            G::Group { inner, .. } => {
                self.unchecked.push("GROUP BY");
                self.walk(inner, g);
            }
            G::Minus { left, .. } => {
                self.unchecked.push("MINUS");
                self.walk(left, g);
            }
            G::Service { .. } => self.unchecked.push("SERVICE"),
            _ => self.unchecked.push("a pattern the diagnosis does not cut"),
        }
    }
}

fn expression_constants(e: &Expression, out: &mut Vec<Term>) {
    use Expression as E;
    match e {
        E::NamedNode(n) => out.push(Term::NamedNode(n.clone())),
        E::Literal(l) => out.push(Term::Literal(l.clone())),
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
            expression_constants(a, out);
            expression_constants(b, out);
        }
        E::In(a, list) => {
            expression_constants(a, out);
            for x in list {
                expression_constants(x, out);
            }
        }
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => expression_constants(a, out),
        E::FunctionCall(_, args) | E::Coalesce(args) => {
            for x in args {
                expression_constants(x, out);
            }
        }
        _ => {}
    }
}

/// One `ASK` over the call's snapshot.
struct Asker<'a> {
    snap: &'a Arc<Snapshot>,
    opts: QueryOptions,
    query: &'a Query,
    deadline: Instant,
    each: Duration,
}

impl Asker<'_> {
    /// `Some(found)`, or `None` when the check ran out of its time.
    fn ask(&self, pattern: GraphPattern) -> Result<Option<bool>, Error> {
        let (dataset, base_iri) = match self.query {
            Query::Select {
                dataset, base_iri, ..
            }
            | Query::Ask {
                dataset, base_iri, ..
            }
            | Query::Construct {
                dataset, base_iri, ..
            }
            | Query::Describe {
                dataset, base_iri, ..
            } => (dataset.clone(), base_iri.clone()),
        };
        let q = Query::Ask {
            dataset,
            pattern,
            base_iri,
        };
        let left = crate::mcp::tools::remaining(self.deadline)?;
        let mut opts = self.opts.clone();
        opts.timeout = Some(self.each.min(left));
        match sparql::execute_query(self.snap.clone(), &q, &opts, 0.0) {
            Ok(r) => Ok(Some(r.boolean)),
            Err(Error::Timeout) if Instant::now() < self.deadline => Ok(None),
            Err(e) => Err(e),
        }
    }
}

fn whole(q: &Query) -> &GraphPattern {
    match q {
        Query::Select { pattern, .. }
        | Query::Ask { pattern, .. }
        | Query::Construct { pattern, .. }
        | Query::Describe { pattern, .. } => pattern,
    }
}

impl Tools<'_> {
    pub(crate) fn why_empty(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let mut check_args = args.clone();
        let a: WhyArgs = parse(args)?;
        if a.query.trim().is_empty() {
            return Err(ToolError::bad_argument("query must not be empty"));
        }
        if a.query.chars().count() > 65536 {
            return Err(ToolError::bad_argument(
                "query must be at most 65536 characters",
            ));
        }
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let parsed = self.parse_query(&a.query, &prefix_map, &ctx)?;
        let reader: Reader =
            self.reader(&ds, a.at_commit, a.at.as_ref(), a.reasoning, deadline, &ctx)?;
        let reasoning = Self::reasoning(&ds, a.reasoning);
        let opts = self
            .query_options(
                &ds.name,
                crate::auth::Endpoint::Query,
                reasoning,
                deadline,
                &prefix_map,
            )
            .map_err(|e| ctx.engine(e))?;
        let asker = Asker {
            snap: &reader.snap,
            opts,
            query: &parsed,
            deadline,
            each: timeout / 10,
        };
        let mut terms = Terms::new(&prefixes, 200);
        let mut out = json!({ "dataset": ds.name, "commit": reader.snap.commit });
        let has_rows = asker
            .ask(whole(&parsed).clone())
            .map_err(|e| ctx.engine(e))?;
        out["empty"] = has_rows.map(|f| !f).into();
        let mut cut = Cut {
            steps: Vec::new(),
            unchecked: Vec::new(),
            terms: &mut terms,
        };
        cut.walk(whole(&parsed), &None);
        let Cut {
            mut steps,
            unchecked,
            ..
        } = cut;
        let mut complete = unchecked.is_empty();
        if steps.len() > MAX_STEPS {
            steps.truncate(MAX_STEPS);
            complete = false;
        }
        if has_rows == Some(true) {
            out["steps"] = json!([]);
            out["complete"] = true.into();
            out["message"] = "The query has solutions in your view.".into();
            out["prefixes"] = json!(terms.used());
            return Ok(Outcome::Structured(out));
        }
        let mut report: Vec<Value> = Vec::new();
        let mut first: Option<(usize, &'static str)> = None;
        // each pattern alone
        for (i, s) in steps.iter().enumerate() {
            let Some(alone) = &s.alone else { continue };
            let found = asker.ask(alone.clone()).map_err(|e| ctx.engine(e))?;
            report.push(json!({ "kind": "pattern", "text": s.text, "solutions": found }));
            match found {
                Some(false) => {
                    first = Some((i, "pattern"));
                    break;
                }
                None => complete = false,
                Some(true) => {}
            }
        }
        // the patterns joined in order, with the filters, BINDs and VALUES in place
        if first.is_none() {
            let mut cum = GraphPattern::Bgp {
                patterns: Vec::new(),
            };
            let mut patterns = 0;
            for (i, s) in steps.iter().enumerate() {
                cum = (s.apply)(cum);
                let kind = match s.kind {
                    Kind::Pattern => {
                        patterns += 1;
                        if patterns == 1 {
                            continue;
                        }
                        "join"
                    }
                    Kind::Filter => "filter",
                    Kind::Apply => continue,
                };
                let found = asker.ask(cum.clone()).map_err(|e| ctx.engine(e))?;
                report.push(json!({ "kind": kind, "text": s.text, "solutions": found }));
                match found {
                    Some(false) => {
                        first = Some((i, kind));
                        break;
                    }
                    None => complete = false,
                    Some(true) => {}
                }
            }
        }
        out["steps"] = report.into();
        let message;
        if let Some((i, kind)) = first {
            let s = &steps[i];
            let mut constants = Vec::new();
            let mut absent = Vec::new();
            let mut seen = BTreeSet::new();
            for c in &s.constants {
                let t = terms.term(c);
                if !seen.insert(t.clone()) {
                    continue;
                }
                let occurs = match c {
                    Term::NamedNode(_) | Term::Literal(_) => match reader.snap.lookup_term(c) {
                        None => false,
                        Some(_) => reader
                            .ask(&exists_term(&reader), vec![("t".into(), c.clone())])
                            .map_err(|e| ctx.engine(e))?,
                    },
                    _ => true,
                };
                if !occurs {
                    absent.push(t.clone());
                }
                constants.push(json!({ "term": t, "occurs": occurs }));
            }
            // the check_query issues about this step's constants
            check_args.remove("timeoutSeconds");
            let issues = match self.check_query(check_args) {
                Ok(Outcome::Structured(c)) => c["issues"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|i| i["term"].as_str().is_some_and(|t| seen.contains(t)))
                    .collect(),
                _ => Vec::new(),
            };
            let mut f =
                json!({ "kind": kind, "text": s.text, "constants": constants, "issues": issues });
            let at = s.constants.iter().find_map(|c| match c {
                Term::NamedNode(n) => super::check::locate(&a.query, n.as_str(), &prefix_map),
                _ => None,
            });
            if let Some((l, c)) = at {
                f["line"] = l.into();
                f["column"] = c.into();
            }
            message = match kind {
                "pattern" if !absent.is_empty() => format!(
                    "The pattern {} has no solutions: {} does not occur in your view of dataset {}.",
                    s.text,
                    absent.join(" and "),
                    ds.name
                ),
                "pattern" => format!(
                    "The pattern {} has no solutions in your view of dataset {}, although its terms occur.",
                    s.text, ds.name
                ),
                "join" => format!(
                    "The pattern {} has solutions alone, but none that join the patterns before it.",
                    s.text
                ),
                _ => format!(
                    "{} removes every solution of the patterns before it.",
                    s.text
                ),
            };
            out["first"] = f;
        } else if complete {
            message = "Every pattern, join and filter has solutions, so the empty result comes from a part the check does not cut.".into();
        } else {
            message = "No checked pattern, join or filter is empty, but some checks ran out of time or some parts were not checked.".into();
        }
        if !unchecked.is_empty() {
            out["unchecked"] = json!(unchecked);
        }
        out["complete"] = complete.into();
        out["message"] = message.into();
        out["prefixes"] = json!(terms.used());
        Ok(Outcome::Structured(out))
    }
}
