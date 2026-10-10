//! `check_query` (C17 §5.2): a query's terms compared with the caller's view of the
//! dataset, without running the query.

use super::text::{edit_distance, words};
use super::{RDF_TYPE, Reader, exists_term, local_name};
use crate::mcp::Outcome;
use crate::mcp::errors::{ToolError, syntax_summary};
use crate::mcp::render::{Prefixes, Terms};
use crate::mcp::tools::{Tools, bounded, dataset_prefixes, parse};
use oxrdf::{Literal, NamedNode, Term};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use spargebra::Query;
use spargebra::algebra::{Expression, GraphPattern, PropertyPathExpression};
use spargebra::term::{NamedNodePattern, TermPattern};
use sparkles::error::Error;
use sparkles::schema::{LiteralGroup, SchemaReport};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";
const DIR_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#dirLangString";

/// The most distinct terms one call checks against the data (a check of a term the
/// schema report does not settle is one indexed lookup).
const MAX_LOOKUPS: usize = 200;
/// The instances of a class whose predicates suggest replacements for a mismatch.
const PROFILE_SAMPLE: usize = 200;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CheckArgs {
    dataset: Option<String>,
    query: String,
    explain: Option<bool>,
    max_suggestions: Option<u64>,
    reasoning: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
    timeout_seconds: Option<f64>,
}

/// How a literal meets the objects of a predicate.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Meet {
    /// as the object of a triple pattern: term equality
    Pattern,
    /// compared in a FILTER: value comparison
    Filter,
}

/// The terms a query uses, by position.
#[derive(Default)]
struct Usage {
    predicates: Vec<NamedNode>,
    classes: Vec<NamedNode>,
    others: Vec<NamedNode>,
    /// literals matched with the objects of a constant predicate
    matched: Vec<(NamedNode, Literal, Meet)>,
    /// (variable, class, predicate): the variable is typed with the class and is the
    /// subject of the predicate in one basic graph pattern
    typed: Vec<(String, NamedNode, NamedNode)>,
    /// variable → the constant predicates whose object it is
    objects_of: HashMap<String, Vec<NamedNode>>,
    /// (variable, literal) compared in filters
    compared: Vec<(String, Literal)>,
}

impl Usage {
    fn path(&mut self, p: &PropertyPathExpression) {
        use PropertyPathExpression as P;
        match p {
            P::NamedNode(n) => self.predicates.push(n.clone()),
            P::Reverse(a)
            | P::ZeroOrMore(a)
            | P::OneOrMore(a)
            | P::ZeroOrOne(a)
            | P::Distinct(a)
            | P::Multi(a)
            | P::Shortest(a) => self.path(a),
            P::Sequence(a, b) | P::Alternative(a, b) => {
                self.path(a);
                self.path(b);
            }
            // excluded predicates match nothing whether or not they exist
            P::NegatedPropertySet(_) => {}
            P::Range { path, .. } => self.path(path),
        }
    }

    fn end(&mut self, t: &TermPattern) {
        if let TermPattern::NamedNode(n) = t {
            self.others.push(n.clone());
        }
    }

    fn expression(&mut self, e: &Expression) {
        use Expression as E;
        match e {
            E::Equal(a, b)
            | E::SameTerm(a, b)
            | E::Greater(a, b)
            | E::GreaterOrEqual(a, b)
            | E::Less(a, b)
            | E::LessOrEqual(a, b) => match (a.as_ref(), b.as_ref()) {
                (E::Variable(v), E::Literal(l)) | (E::Literal(l), E::Variable(v)) => {
                    self.compared.push((v.as_str().to_string(), l.clone()));
                }
                _ => {}
            },
            E::And(a, b) | E::Or(a, b) => {
                self.expression(a);
                self.expression(b);
            }
            E::Not(a) => self.expression(a),
            _ => {}
        }
    }

    fn walk(&mut self, p: &GraphPattern) {
        match p {
            GraphPattern::Bgp { patterns } => {
                let mut types: Vec<(String, NamedNode)> = Vec::new();
                let mut uses: Vec<(String, NamedNode)> = Vec::new();
                for tp in patterns {
                    self.end(&tp.subject);
                    let NamedNodePattern::NamedNode(pred) = &tp.predicate else {
                        self.end(&tp.object);
                        continue;
                    };
                    self.predicates.push(pred.clone());
                    let subject = match &tp.subject {
                        TermPattern::Variable(v) => Some(v.as_str().to_string()),
                        _ => None,
                    };
                    match &tp.object {
                        TermPattern::NamedNode(o) if pred.as_str() == RDF_TYPE => {
                            self.classes.push(o.clone());
                            if let Some(v) = &subject {
                                types.push((v.clone(), o.clone()));
                            }
                        }
                        TermPattern::NamedNode(o) => self.others.push(o.clone()),
                        TermPattern::Literal(l) => {
                            self.matched.push((pred.clone(), l.clone(), Meet::Pattern));
                        }
                        TermPattern::Variable(v) => {
                            self.objects_of
                                .entry(v.as_str().to_string())
                                .or_default()
                                .push(pred.clone());
                        }
                        _ => {}
                    }
                    if pred.as_str() != RDF_TYPE
                        && let Some(v) = subject
                    {
                        uses.push((v, pred.clone()));
                    }
                }
                for (v, c) in &types {
                    for (u, p) in &uses {
                        if u == v {
                            self.typed.push((v.clone(), c.clone(), p.clone()));
                        }
                    }
                }
            }
            GraphPattern::Path {
                subject,
                path,
                object,
            } => {
                self.end(subject);
                self.path(path);
                self.end(object);
            }
            GraphPattern::Filter { expr, inner } => {
                self.expression(expr);
                self.walk(inner);
            }
            GraphPattern::Service { .. } => {}
            p => {
                for c in crate::mcp::tools::children(p) {
                    self.walk(c);
                }
            }
        }
    }
}

fn dedup(v: &mut Vec<NamedNode>) {
    let mut seen = BTreeSet::new();
    v.retain(|n| seen.insert(n.as_str().to_string()));
}

/// One suggested term.
pub(super) struct Suggestion {
    iri: String,
    label: Option<String>,
    count: u64,
    why: &'static str,
}

impl Suggestion {
    pub(super) fn json(&self, terms: &mut Terms) -> Value {
        let mut s = json!({ "term": terms.iri(&self.iri), "count": self.count, "why": self.why });
        if let Some(l) = &self.label {
            s["label"] = l.clone().into();
        }
        s
    }
}

/// The candidates that resemble `unknown`, best first: the same local name in another
/// namespace, then a small edit distance between the local names' words, then a label
/// whose words hold the local name's words. Ties go to the most used.
pub(super) fn suggestions<'a>(
    unknown: &str,
    candidates: impl Iterator<Item = (&'a str, &'a [sparkles::schema::Lit], u64)>,
    max: usize,
) -> Vec<Suggestion> {
    let ul = local_name(unknown);
    let uw = words(ul);
    let un = uw.join(" ");
    let mut out: Vec<(usize, Suggestion)> = Vec::new();
    for (c, labels, count) in candidates {
        if c == unknown {
            continue;
        }
        let cl = local_name(c);
        let ns_differs = c[..c.len() - cl.len()] != unknown[..unknown.len() - ul.len()];
        let rank = if !ul.is_empty() && ul.eq_ignore_ascii_case(cl) && ns_differs {
            0
        } else {
            let cn = words(cl).join(" ");
            let longer = un.chars().count().max(cn.chars().count());
            if !un.is_empty() && !cn.is_empty() && edit_distance(&un, &cn) * 3 <= longer {
                1
            } else if !uw.is_empty()
                && labels.iter().any(|l| {
                    let lw = words(&l.value);
                    uw.iter().all(|w| lw.contains(w))
                })
            {
                2
            } else {
                continue;
            }
        };
        let why = ["same-local-name", "edit-distance", "label"][rank];
        let label = crate::mcp::render::choose(
            labels.iter().map(|l| (l.value.as_str(), l.lang.as_deref())),
            "en",
        );
        out.push((
            rank,
            Suggestion {
                iri: c.to_string(),
                label,
                count,
                why,
            },
        ));
    }
    out.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.count.cmp(&a.1.count))
            .then_with(|| a.1.iri.cmp(&b.1.iri))
    });
    out.into_iter().take(max).map(|(_, s)| s).collect()
}

fn numeric(dt: &str) -> bool {
    dt.strip_prefix(XSD).is_some_and(|l| {
        matches!(
            l,
            "integer"
                | "decimal"
                | "double"
                | "float"
                | "int"
                | "long"
                | "short"
                | "byte"
                | "nonNegativeInteger"
                | "positiveInteger"
                | "negativeInteger"
                | "nonPositiveInteger"
                | "unsignedInt"
                | "unsignedLong"
                | "unsignedShort"
                | "unsignedByte"
        )
    })
}

fn tagged(dt: &str) -> bool {
    dt == LANG_STRING || dt == DIR_LANG_STRING
}

/// The `(line, column)` (1-based, in characters) of the first place the query writes
/// `iri`: as `<iri>` or as a prefixed name of the query's or the dataset's prefixes.
fn locate(query: &str, iri: &str, prefixes: &BTreeMap<String, String>) -> Option<(usize, usize)> {
    static DECL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)PREFIX\s+([A-Za-z][\w.-]*)?:\s*<([^>\s]*)>").unwrap()
    });
    let mut all: Vec<(String, String)> = prefixes
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for c in DECL.captures_iter(query) {
        let name = c.get(1).map_or("", |m| m.as_str()).to_string();
        all.push((name, c[2].to_string()));
    }
    let mut at = query.find(&format!("<{iri}>"));
    for (name, ns) in &all {
        if at.is_some() {
            break;
        }
        let Some(local) = iri.strip_prefix(ns.as_str()) else {
            continue;
        };
        let needle = format!("{name}:{local}");
        at = query.match_indices(&needle).map(|(i, _)| i).find(|&i| {
            let before = query[..i].chars().next_back();
            let after = query[i + needle.len()..].chars().next();
            !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':')
                && !after.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-')
        });
    }
    let i = at?;
    let line = query[..i].matches('\n').count() + 1;
    let start = query[..i].rfind('\n').map_or(0, |n| n + 1);
    Some((line, query[start..i].chars().count() + 1))
}

/// An issue of the result.
pub(super) struct Issue {
    pub code: &'static str,
    pub error: bool,
    pub message: String,
    pub term: Option<String>,
    pub at: Option<(usize, usize)>,
    pub suggestions: Vec<Value>,
}

impl Issue {
    fn json(&self) -> Value {
        let mut j = json!({
            "code": self.code,
            "severity": if self.error { "error" } else { "warning" },
            "message": self.message,
        });
        if let Some(t) = &self.term {
            j["term"] = t.clone().into();
        }
        if let Some((l, c)) = self.at {
            j["line"] = l.into();
            j["column"] = c.into();
        }
        if !self.suggestions.is_empty() {
            j["suggestions"] = self.suggestions.clone().into();
        }
        j
    }
}

/// The prefixes whose name or namespace resembles an undefined prefix name.
fn prefix_suggestions(name: &str, prefixes: &BTreeMap<String, String>) -> Vec<Value> {
    let lname = name.to_lowercase();
    let mut out: Vec<Value> = Vec::new();
    for (k, ns) in prefixes {
        let same = k.eq_ignore_ascii_case(name);
        let in_ns = !lname.is_empty() && words(ns).contains(&lname);
        let close = !lname.is_empty()
            && edit_distance(&k.to_lowercase(), &lname)
                <= k.chars().count().max(lname.chars().count()) / 3;
        let why = if same {
            "same-name"
        } else if in_ns {
            "namespace"
        } else if close {
            "edit-distance"
        } else {
            continue;
        };
        out.push(json!({
            "term": format!("{k}:"),
            "label": crate::mcp::render::label_text(ns),
            "count": 0,
            "why": why,
        }));
    }
    let rank = |v: &Value| match v["why"].as_str() {
        Some("same-name") => 0,
        Some("namespace") => 1,
        _ => 2,
    };
    out.sort_by_key(rank);
    out.truncate(10);
    out
}

impl Tools<'_> {
    pub(crate) fn check_query(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: CheckArgs = parse(args)?;
        if a.query.trim().is_empty() {
            return Err(ToolError::bad_argument("query must not be empty"));
        }
        if a.query.chars().count() > 65536 {
            return Err(ToolError::bad_argument(
                "query must be at most 65536 characters",
            ));
        }
        let max = bounded("maxSuggestions", a.max_suggestions, 3, 0, 10)? as usize;
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let r = self.reader(&ds, a.at_commit, a.at.as_ref(), a.reasoning, deadline, &ctx)?;
        let mut terms = Terms::new(&prefixes, 500);
        let mut issues: Vec<Issue> = Vec::new();
        let pv: Vec<(String, String)> = prefix_map
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut out = json!({ "dataset": ds.name, "commit": r.snap.commit });
        let parsed = match sparkles::sparql::parse_query(&a.query, None, &pv) {
            Ok(q) => q,
            Err(e) => {
                issues.push(self.parse_issue(&a.query, e, &pv, &prefix_map));
                return Ok(Outcome::Structured(finish(out, &issues, None, &terms)));
            }
        };
        let mut usage = Usage::default();
        usage.walk(pattern(&parsed));
        for (v, l) in std::mem::take(&mut usage.compared) {
            for p in usage.objects_of.get(&v).cloned().unwrap_or_default() {
                usage.matched.push((p, l.clone(), Meet::Filter));
            }
        }
        dedup(&mut usage.predicates);
        dedup(&mut usage.classes);
        dedup(&mut usage.others);
        let report = self.view_report(&ds, &r, &ctx)?;
        let mut lookups = 0usize;
        let mut known: HashMap<String, bool> = HashMap::new();
        // predicates
        for p in &usage.predicates {
            let entry = report.as_ref().and_then(|rep| predicate(rep, p.as_str()));
            let mut ok =
                entry.is_some_and(|e| e.observed.triples > 0 || !e.declared.types.is_empty());
            if !ok && lookups < MAX_LOOKUPS {
                lookups += 1;
                ok = r
                    .ask(&exists_predicate(&r), vec![("pp".into(), p.clone().into())])
                    .map_err(|e| ctx.engine(e))?;
            }
            known.insert(p.as_str().to_string(), ok);
            if ok {
                continue;
            }
            let t = terms.iri(p.as_str());
            let suggestions = report.as_ref().map_or_else(Vec::new, |rep| {
                suggestions(
                    p.as_str(),
                    rep.predicates
                        .iter()
                        .filter(|e| e.observed.triples > 0 || !e.declared.types.is_empty())
                        .map(|e| {
                            (
                                e.iri.as_str(),
                                e.declared.labels.as_slice(),
                                e.observed.triples,
                            )
                        }),
                    max,
                )
            });
            issues.push(Issue {
                code: "unknown-predicate",
                error: true,
                message: format!(
                    "{t} has no triples in dataset {} and is not declared as a property; patterns using it match nothing",
                    ds.name
                ),
                at: locate(&a.query, p.as_str(), &prefix_map),
                suggestions: suggestions.iter().map(|s| s.json(&mut terms)).collect(),
                term: Some(t),
            });
        }
        // classes
        for c in &usage.classes {
            let entry = report.as_ref().and_then(|rep| class(rep, c.as_str()));
            let mut ok =
                entry.is_some_and(|e| e.observed.instances > 0 || !e.declared.types.is_empty());
            if !ok && lookups < MAX_LOOKUPS {
                lookups += 1;
                ok = r
                    .ask(&exists_class(&r), vec![("c".into(), c.clone().into())])
                    .map_err(|e| ctx.engine(e))?;
            }
            known.insert(c.as_str().to_string(), ok);
            if ok {
                continue;
            }
            let t = terms.iri(c.as_str());
            let suggestions = report.as_ref().map_or_else(Vec::new, |rep| {
                suggestions(
                    c.as_str(),
                    rep.classes
                        .iter()
                        .filter(|e| e.observed.instances > 0 || !e.declared.types.is_empty())
                        .map(|e| {
                            (
                                e.iri.as_str(),
                                e.declared.labels.as_slice(),
                                e.observed.instances,
                            )
                        }),
                    max,
                )
            });
            issues.push(Issue {
                code: "unknown-class",
                error: true,
                message: format!(
                    "{t} has no instances in dataset {} and is not declared as a class; patterns using it match nothing",
                    ds.name
                ),
                at: locate(&a.query, c.as_str(), &prefix_map),
                suggestions: suggestions.iter().map(|s| s.json(&mut terms)).collect(),
                term: Some(t),
            });
        }
        // other constant IRIs
        for o in &usage.others {
            if known.contains_key(o.as_str()) || lookups >= MAX_LOOKUPS {
                continue;
            }
            let ok = match r.snap.lookup_term(&Term::NamedNode(o.clone())) {
                // never stored: absent everywhere
                None => false,
                Some(_) => {
                    lookups += 1;
                    r.ask(&exists_term(&r), vec![("t".into(), o.clone().into())])
                        .map_err(|e| ctx.engine(e))?
                }
            };
            known.insert(o.as_str().to_string(), ok);
            if ok {
                continue;
            }
            let t = terms.iri(o.as_str());
            issues.push(Issue {
                code: "unknown-term",
                error: false,
                message: format!(
                    "{t} does not occur in dataset {}; patterns using it match nothing. Check spelling with describe_schema, or find the entity with link_entities.",
                    ds.name
                ),
                at: locate(&a.query, o.as_str(), &prefix_map),
                suggestions: Vec::new(),
                term: Some(t),
            });
        }
        // a typed variable used with a predicate no instance of its class has
        let mut seen = BTreeSet::new();
        for (v, c, p) in &usage.typed {
            if known.get(c.as_str()) != Some(&true)
                || known.get(p.as_str()) != Some(&true)
                || !seen.insert((c.as_str().to_string(), p.as_str().to_string()))
                || lookups >= MAX_LOOKUPS
            {
                continue;
            }
            lookups += 1;
            let used = r
                .ask(
                    &format!(
                        "ASK {{ {} {} }}",
                        r.quads("?s a ?c", &[]),
                        r.quads_in("?s ?pp ?o", &[], "h")
                    ),
                    vec![
                        ("c".into(), c.clone().into()),
                        ("pp".into(), p.clone().into()),
                    ],
                )
                .map_err(|e| ctx.engine(e))?;
            if used {
                continue;
            }
            let (tc, tp) = (terms.iri(c.as_str()), terms.iri(p.as_str()));
            let suggestions = self
                .class_predicates(&r, c, max)
                .map_err(|e| ctx.engine(e))?
                .into_iter()
                .map(|(p, n)| json!({ "term": terms.iri(p.as_str()), "count": n, "why": "class-profile" }))
                .collect();
            issues.push(Issue {
                code: "class-mismatch",
                error: false,
                message: format!(
                    "?{v} is a {tc}, and no {tc} in dataset {} has {tp}; the pattern matches nothing",
                    ds.name
                ),
                term: Some(tp),
                at: locate(&a.query, p.as_str(), &prefix_map),
                suggestions,
            });
        }
        // literals against the objects of their predicate
        if let Some(rep) = &report {
            let mut seen = BTreeSet::new();
            for (p, l, meet) in &usage.matched {
                if !seen.insert((p.as_str().to_string(), l.to_string())) {
                    continue;
                }
                let Some(entry) = predicate(rep, p.as_str()).filter(|e| e.observed.triples > 0)
                else {
                    continue;
                };
                if let Some(issue) =
                    literal_issue(p, l, *meet, &entry.observed.objects.literals, &mut terms)
                {
                    let mut issue = issue;
                    issue.at = locate(&a.query, p.as_str(), &prefix_map);
                    issues.push(issue);
                }
            }
        }
        // projected variables bound nowhere
        if let Query::Select { pattern, .. } = &parsed
            && let Some((vars, inner)) = projection(pattern)
        {
            let mut scope = BTreeSet::new();
            inner.on_in_scope_variable(|v| {
                scope.insert(v.as_str().to_string());
            });
            for v in vars {
                if !scope.contains(v.as_str()) {
                    issues.push(Issue {
                        code: "unbound-projection",
                        error: false,
                        message: format!(
                            "?{} is projected but occurs nowhere in the pattern; it is always unbound",
                            v.as_str()
                        ),
                        term: Some(format!("?{}", v.as_str())),
                        at: None,
                        suggestions: Vec::new(),
                    });
                }
            }
        }
        let mut estimated = None;
        if a.explain.unwrap_or(false) {
            let mut opts = r.opts.clone();
            opts.prefixes = pv.clone();
            opts.timeout =
                Some(super::super::tools::remaining(deadline).map_err(|e| ctx.engine(e))?);
            let (_, plan) = sparkles::sparql::explain(r.snap.clone(), &a.query, &opts)
                .map_err(|e| ctx.engine(e))?;
            // a view restricted by grants sees no estimates (-1)
            let root = plan.estimated_rows;
            estimated = (root >= 0.0).then(|| root.round() as u64);
            for w in crate::mcp::tools::plan_warnings(&plan, &parsed, 100.min(self.cfg().max_rows))
            {
                issues.push(Issue {
                    code: w.0,
                    error: false,
                    message: w.1,
                    term: None,
                    at: None,
                    suggestions: Vec::new(),
                });
            }
        }
        out["commit"] = r.snap.commit.into();
        Ok(Outcome::Structured(finish(out, &issues, estimated, &terms)))
    }

    /// The issue of a query that does not parse: an update is `not-a-query`, anything
    /// else `syntax` with its line and column, and an undefined prefix suggests the
    /// known prefixes that resemble it.
    fn parse_issue(
        &self,
        q: &str,
        e: Error,
        pv: &[(String, String)],
        prefixes: &BTreeMap<String, String>,
    ) -> Issue {
        let mut p = Some(spargebra::SparqlParser::new());
        for (k, v) in pv {
            p = p.and_then(|p| p.with_prefix(k, v).ok());
        }
        if p.is_some_and(|p| p.parse_update(q).is_ok()) {
            return Issue {
                code: "not-a-query",
                error: true,
                message: "this is SPARQL Update; check_query checks queries only, and the tools here never run an update through a query"
                    .into(),
                term: None,
                at: None,
                suggestions: Vec::new(),
            };
        }
        let text = e.to_string();
        static POS: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new(r"error at (\d+):(\d+)").unwrap());
        let at = POS
            .captures(&text)
            .and_then(|c| Some((c[1].parse::<usize>().ok()?, c[2].parse::<usize>().ok()?)));
        let mut suggestions = Vec::new();
        if text.contains("Prefix not found")
            && let Some((line, col)) = at
            && let Some(name) = prefix_at(q, line, col)
        {
            suggestions = prefix_suggestions(&name, prefixes);
        }
        Issue {
            code: "syntax",
            error: true,
            message: syntax_summary(&text),
            term: None,
            at,
            suggestions,
        }
    }

    /// The predicates that a sample of the instances of `c` use, most used first.
    fn class_predicates(
        &self,
        r: &Reader,
        c: &NamedNode,
        max: usize,
    ) -> Result<Vec<(NamedNode, u64)>, Error> {
        if max == 0 {
            return Ok(Vec::new());
        }
        let q = format!(
            "SELECT ?p (COUNT(*) AS ?n) WHERE {{ {{ SELECT DISTINCT ?s WHERE {{ {} }} LIMIT {PROFILE_SAMPLE} }} {} FILTER(?p != <{RDF_TYPE}>) }} GROUP BY ?p ORDER BY DESC(?n) ?p LIMIT {max}",
            r.quads("?s a ?c", &[]),
            r.quads_in("?s ?p ?o", &[], "h"),
        );
        Ok(r.rows(&q, vec![("c".into(), c.clone().into())])?
            .into_iter()
            .filter_map(|row| match row.as_slice() {
                [Some(Term::NamedNode(p)), Some(Term::Literal(n))] => {
                    Some((p.clone(), n.value().parse().unwrap_or(0)))
                }
                _ => None,
            })
            .collect())
    }
}

/// The result object.
fn finish(mut out: Value, issues: &[Issue], estimated: Option<u64>, terms: &Terms) -> Value {
    out["ok"] = (!issues.iter().any(|i| i.error)).into();
    out["issues"] = issues.iter().map(Issue::json).collect();
    if let Some(e) = estimated {
        out["estimatedRows"] = e.into();
    }
    out["prefixes"] = json!(terms.used());
    out
}

/// The prefix name of the prefixed name at a 1-based line and column.
fn prefix_at(q: &str, line: usize, col: usize) -> Option<String> {
    let l: Vec<char> = q.lines().nth(line.checked_sub(1)?)?.chars().collect();
    let pn = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':');
    let at = col.checked_sub(1)?.min(l.len());
    // the prefixed name at the position, or the one that ends just before it
    let (mut start, mut end) = (at, at);
    while end < l.len() && pn(l[end]) {
        end += 1;
    }
    if start == end {
        while start > 0 && l[start - 1].is_whitespace() {
            start -= 1;
        }
        end = start;
    }
    while start > 0 && pn(l[start - 1]) {
        start -= 1;
    }
    let token: String = l[start..end].iter().collect();
    let (name, _) = token.split_once(':')?;
    Some(name.to_string())
}

fn pattern(q: &Query) -> &GraphPattern {
    match q {
        Query::Select { pattern, .. }
        | Query::Ask { pattern, .. }
        | Query::Construct { pattern, .. }
        | Query::Describe { pattern, .. } => pattern,
    }
}

/// The top projection of a SELECT: its variables and the pattern below it.
fn projection(p: &GraphPattern) -> Option<(&[spargebra::term::Variable], &GraphPattern)> {
    match p {
        GraphPattern::Project { inner, variables } => Some((variables, inner)),
        GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. }
        | GraphPattern::OrderBy { inner, .. } => projection(inner),
        _ => None,
    }
}

pub(super) fn predicate<'r>(
    rep: &'r SchemaReport,
    iri: &str,
) -> Option<&'r sparkles::schema::PredicateEntry> {
    rep.predicates
        .binary_search_by(|e| e.iri.as_str().cmp(iri))
        .ok()
        .map(|i| &rep.predicates[i])
}

pub(super) fn class<'r>(
    rep: &'r SchemaReport,
    iri: &str,
) -> Option<&'r sparkles::schema::ClassEntry> {
    rep.classes
        .binary_search_by(|e| e.iri.as_str().cmp(iri))
        .ok()
        .map(|i| &rep.classes[i])
}

/// Whether `?pp` has a triple in the view, or is declared a property there.
pub(super) fn exists_predicate(r: &Reader) -> String {
    format!(
        "ASK {{ {{ {} }} UNION {{ {} VALUES ?k {{ <http://www.w3.org/1999/02/22-rdf-syntax-ns#Property> <http://www.w3.org/2002/07/owl#ObjectProperty> <http://www.w3.org/2002/07/owl#DatatypeProperty> <http://www.w3.org/2002/07/owl#AnnotationProperty> }} }} }}",
        r.quads("?s ?pp ?o", &[]),
        r.quads("?pp a ?k", &[]),
    )
}

/// Whether `?c` has an instance in the view, or is declared a class there.
pub(super) fn exists_class(r: &Reader) -> String {
    format!(
        "ASK {{ {{ {} }} UNION {{ {} VALUES ?k {{ <http://www.w3.org/2000/01/rdf-schema#Class> <http://www.w3.org/2002/07/owl#Class> }} }} }}",
        r.quads("?s a ?c", &[]),
        r.quads("?c a ?k", &[]),
    )
}

/// A literal that cannot match the objects of `p`: a simple literal where every object
/// has a language tag (`language-tag`), or a datatype none of the objects has
/// (`datatype-mismatch`; in a FILTER, numbers compare with numbers of any type).
pub(super) fn literal_issue(
    p: &NamedNode,
    l: &Literal,
    meet: Meet,
    groups: &[LiteralGroup],
    terms: &mut Terms,
) -> Option<Issue> {
    let dt = l.datatype().as_str();
    let tp = terms.iri(p.as_str());
    let lt = terms.term(&Term::Literal(l.clone()));
    if dt == XSD_STRING && !groups.is_empty() && groups.iter().all(|g| tagged(&g.datatype)) {
        // the most common tag
        let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
        for g in groups {
            for lc in g.languages.iter().flatten() {
                *counts.entry(lc.lang.as_str()).or_default() += lc.triples;
            }
        }
        let best = counts
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
            .map(|(t, n)| (t.to_string(), *n));
        let suggestions = best
            .and_then(|(tag, n)| {
                let lit = Literal::new_language_tagged_literal(l.value(), tag).ok()?;
                Some(vec![json!({
                    "term": terms.term(&Term::Literal(lit)),
                    "count": n,
                    "why": "language-tag",
                })])
            })
            .unwrap_or_default();
        return Some(Issue {
            code: "language-tag",
            error: false,
            message: format!(
                "every literal object of {tp} has a language tag, so the simple literal {lt} matches none of them"
            ),
            term: Some(lt),
            at: None,
            suggestions,
        });
    }
    if groups.iter().any(|g| g.datatype == dt)
        || (meet == Meet::Filter && numeric(dt) && groups.iter().any(|g| numeric(&g.datatype)))
        || (tagged(dt) && groups.iter().any(|g| tagged(&g.datatype)))
    {
        return None;
    }
    let mut suggestions: Vec<Value> = groups
        .iter()
        .map(|g| json!({ "term": terms.iri(&g.datatype), "count": g.triples, "why": "datatype" }))
        .collect();
    suggestions.truncate(10);
    let has = if groups.is_empty() {
        format!("{tp} has no literal objects")
    } else {
        format!("no object of {tp} has the datatype {}", terms.iri(dt))
    };
    Some(Issue {
        code: "datatype-mismatch",
        error: false,
        message: format!("{has}, so {lt} matches none of them"),
        term: Some(lt),
        at: None,
        suggestions,
    })
}
