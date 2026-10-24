//! Facet histograms of a set of entities (spec U11 §3.2): for each facet property, the
//! counts of the set's entities by ranges of round width of its numbers or of the years of
//! its dates, or by its most frequent values, with the entities that have no value. The
//! brushes of the request filter the set, and each facet counts the set filtered by every
//! brush except its own, so that the facets filter each other while a brushed facet still
//! shows what a wider brush would add.
//!
//! Every count comes from SPARQL queries run with the caller's options, as the groups of
//! the overview do, so the view, the budgets, the deadline and the cancellation apply to
//! all of them. A brush is applied as `FILTER EXISTS { ?s <p> ?f . FILTER(…) }`, whose
//! inner filter is the text that the UI's **Apply to query** writes, so the count of the
//! applied query equals the answer's `matching`.
//!
//! The ranges of a facet are computed over the whole set, not over the brushed set, so a
//! histogram keeps its bins while other facets are brushed. A request can hand back the
//! `edges` or `bars` of an earlier answer, which skips the queries that find them.

use super::groups::{
    LABELS, RDF_TYPE, Rows, Run, XSD, count, get, is_date_type, is_integer_type, is_numeric_type,
    literal_type, nice_width, number,
};
use crate::error::{Error, Result};
use crate::history::At;
use crate::sparql::QueryOptions;
use crate::store::Snapshot;
use oxrdf::Term;
use serde_json::{Value as J, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Facets in one request at most.
pub const MAX_FACETS: usize = 6;
/// Brushes in one request at most.
pub const MAX_FILTERS: usize = 6;
/// Bins of a histogram, by default and at most.
pub const DEFAULT_BINS: usize = 20;
pub const MAX_BINS: usize = 40;
/// Values of a bar list at most, which is also the default.
pub const MAX_TOP: usize = 12;
/// Terms of a `nodes` set at most.
pub const MAX_NODES: usize = 20_000;
/// Entities of a set at most. A larger set is refused with a rows budget (`507`).
pub const MAX_SET: u64 = 1_000_000;
/// Values one brush of a bar list may name at most.
pub const MAX_BRUSH_VALUES: usize = 100;
/// Entities of the set read to list the candidate properties, and the most listed.
const CANDIDATE_SAMPLE: usize = 1_000;
const MAX_CANDIDATES: usize = 40;
/// Values read to tell a property's kind.
const KIND_SAMPLE: usize = 500;

/// What a facet counts by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// ranges of the numbers
    Numeric,
    /// ranges of the calendar years of `xsd:date`, `xsd:dateTime` and `xsd:dateTimeStamp`
    Date,
    /// the most frequent values, IRIs or literals
    Values,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Numeric => "numeric",
            Kind::Date => "date",
            Kind::Values => "values",
        }
    }

    fn parse(s: &str, what: &str) -> Result<Kind> {
        match s {
            "numeric" => Ok(Kind::Numeric),
            "date" => Ok(Kind::Date),
            "values" => Ok(Kind::Values),
            _ => Err(bad(format!(
                "{what} must be \"numeric\", \"date\" or \"values\""
            ))),
        }
    }
}

/// The set the facets count.
#[derive(Clone, Debug, PartialEq)]
pub enum FacetSet {
    /// the instances of a class
    Class(String),
    /// these IRIs
    Nodes(Vec<String>),
    /// the distinct values of `variable` in the solutions of a `SELECT` query
    Query { query: String, variable: String },
}

/// One facet to count.
#[derive(Clone, Debug, PartialEq)]
pub struct FacetSpec {
    pub property: String,
    pub bins: usize,
    pub top: usize,
    /// the kind, told from a sample of the values when `None`
    pub kind: Option<Kind>,
    /// the edges of the ranges from an earlier answer, kept as they are
    pub edges: Option<Vec<f64>>,
    /// the values of a bar list from an earlier answer, as SPARQL terms
    pub bars: Option<Vec<String>>,
}

/// A brush: a range of a numeric or date facet, or values of a bar list.
#[derive(Clone, Debug, PartialEq)]
pub struct Brush {
    pub property: String,
    pub kind: Kind,
    /// `[from, to)`, years for a date facet
    pub range: Option<(f64, f64)>,
    /// SPARQL terms
    pub values: Vec<String>,
}

/// A facets request (`POST /$/graph/{ds}/facets`).
#[derive(Clone, Debug, PartialEq)]
pub struct FacetsRequest {
    pub within: FacetSet,
    pub facets: Vec<FacetSpec>,
    pub filters: Vec<Brush>,
    pub candidates: bool,
    pub at: Option<At>,
}

fn bad(msg: impl Into<String>) -> Error {
    Error::invalid(msg)
}

fn iri(v: &J, what: &str) -> Result<String> {
    let s = v
        .as_str()
        .ok_or_else(|| bad(format!("{what} must be an IRI string")))?;
    let s = s
        .strip_prefix('<')
        .and_then(|i| i.strip_suffix('>'))
        .unwrap_or(s);
    oxrdf::NamedNode::new(s).map_err(|e| bad(format!("{what}: invalid IRI '{s}': {e}")))?;
    Ok(s.to_string())
}

fn check_keys(o: &serde_json::Map<String, J>, allowed: &[&str], what: &str) -> Result<()> {
    for k in o.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(bad(format!(
                "unknown member '{k}' of {what}: {}",
                allowed.join(", ")
            )));
        }
    }
    Ok(())
}

/// A term of the SPARQL JSON results format, `{"type": "uri", "value": …}` or
/// `{"type": "literal", "value": …, "datatype"?: …, "xml:lang"?: …}`, as SPARQL text.
pub fn term_from_json(v: &J, what: &str) -> Result<String> {
    let o = v.as_object().ok_or_else(|| {
        bad(format!(
            "{what} must be a term such as {{\"type\": \"literal\", \"value\": \"FR\"}}"
        ))
    })?;
    let value = o
        .get("value")
        .and_then(J::as_str)
        .ok_or_else(|| bad(format!("{what}.value must be a string")))?;
    match o.get("type").and_then(J::as_str) {
        Some("uri") => {
            let n = oxrdf::NamedNode::new(value)
                .map_err(|e| bad(format!("{what}: invalid IRI '{value}': {e}")))?;
            Ok(n.to_string())
        }
        Some("literal") | Some("typed-literal") => {
            let lit = match (
                o.get("xml:lang").and_then(J::as_str),
                o.get("datatype").and_then(J::as_str),
            ) {
                (Some(lang), _) => oxrdf::Literal::new_language_tagged_literal(value, lang)
                    .map_err(|e| bad(format!("{what}: invalid language tag: {e}")))?,
                (None, Some(dt)) => {
                    let dt = oxrdf::NamedNode::new(dt)
                        .map_err(|e| bad(format!("{what}: invalid datatype: {e}")))?;
                    oxrdf::Literal::new_typed_literal(value, dt)
                }
                (None, None) => oxrdf::Literal::new_simple_literal(value),
            };
            Ok(lit.to_string())
        }
        _ => Err(bad(format!(
            "{what}.type must be \"uri\" or \"literal\": blank nodes cannot be named in a request"
        ))),
    }
}

/// A term as the SPARQL JSON results format writes it.
pub fn term_json(t: &Term) -> J {
    match t {
        Term::NamedNode(n) => json!({ "type": "uri", "value": n.as_str() }),
        Term::Literal(l) => {
            let mut o = json!({ "type": "literal", "value": l.value() });
            if let Some(lang) = l.language() {
                o["xml:lang"] = lang.into();
            } else if l.datatype().as_str() != format!("{XSD}string") {
                o["datatype"] = l.datatype().as_str().into();
            }
            o
        }
        Term::BlankNode(b) => json!({ "type": "bnode", "value": b.as_str() }),
        #[allow(unreachable_patterns)]
        other => json!({ "type": "triple", "value": other.to_string() }),
    }
}

fn finite(v: &J, what: &str) -> Result<f64> {
    v.as_f64()
        .filter(|f| f.is_finite())
        .ok_or_else(|| bad(format!("{what} must be a finite number")))
}

fn bounded(
    o: &serde_json::Map<String, J>,
    k: &str,
    lo: usize,
    hi: usize,
    dflt: usize,
) -> Result<usize> {
    match o.get(k) {
        None | Some(J::Null) => Ok(dflt),
        Some(v) => v
            .as_u64()
            .filter(|n| (lo as u64..=hi as u64).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| bad(format!("{k} must be an integer from {lo} to {hi}"))),
    }
}

impl FacetsRequest {
    /// Parse and check a request body.
    pub fn from_json(j: &J) -> Result<FacetsRequest> {
        let o = j
            .as_object()
            .ok_or_else(|| bad("the request must be a JSON object"))?;
        for k in o.keys() {
            if !matches!(
                k.as_str(),
                "within" | "facets" | "filters" | "candidates" | "at"
            ) {
                return Err(bad(if k == "branch" {
                    "name the branch with ?branch=, as on the other routes".to_string()
                } else if k == "reasoning" {
                    "ask for reasoning with ?reasoning=, as on the other routes".to_string()
                } else {
                    format!("unknown member '{k}': within, facets, filters, candidates or at")
                }));
            }
        }
        let within = match o.get("within") {
            Some(J::Object(w)) if w.len() == 1 && w.contains_key("class") => {
                FacetSet::Class(iri(&w["class"], "within.class")?)
            }
            Some(J::Object(w)) if w.len() == 1 && w.contains_key("nodes") => {
                let list = w["nodes"]
                    .as_array()
                    .ok_or_else(|| bad("within.nodes must be an array of IRIs"))?;
                if list.is_empty() || list.len() > MAX_NODES {
                    return Err(bad(format!(
                        "within.nodes must hold from 1 to {MAX_NODES} IRIs"
                    )));
                }
                FacetSet::Nodes(
                    list.iter()
                        .map(|v| iri(v, "within.nodes[]"))
                        .collect::<Result<_>>()?,
                )
            }
            Some(J::Object(w))
                if w.contains_key("query") && w.contains_key("variable") && w.len() == 2 =>
            {
                let query = w["query"]
                    .as_str()
                    .ok_or_else(|| bad("within.query must be a SELECT query"))?
                    .to_string();
                let variable = w["variable"]
                    .as_str()
                    .ok_or_else(|| bad("within.variable must be a variable name"))?;
                let variable = variable.trim_start_matches(['?', '$']).to_string();
                oxrdf::Variable::new(&variable)
                    .map_err(|e| bad(format!("within.variable: {e}")))?;
                FacetSet::Query { query, variable }
            }
            _ => {
                return Err(bad(
                    "within must be {\"class\": IRI}, {\"nodes\": [IRI, …]} or {\"query\": SELECT, \"variable\": name}",
                ));
            }
        };
        let facets = match o.get("facets") {
            None | Some(J::Null) => Vec::new(),
            Some(J::Array(a)) if a.len() <= MAX_FACETS => {
                a.iter().map(facet_spec).collect::<Result<Vec<_>>>()?
            }
            Some(_) => {
                return Err(bad(format!(
                    "facets must be an array of at most {MAX_FACETS} facets"
                )));
            }
        };
        let filters = match o.get("filters") {
            None | Some(J::Null) => Vec::new(),
            Some(J::Array(a)) if a.len() <= MAX_FILTERS => {
                a.iter().map(brush).collect::<Result<Vec<_>>>()?
            }
            Some(_) => {
                return Err(bad(format!(
                    "filters must be an array of at most {MAX_FILTERS} brushes"
                )));
            }
        };
        let candidates = match o.get("candidates") {
            None | Some(J::Null) => false,
            Some(J::Bool(b)) => *b,
            Some(_) => return Err(bad("candidates must be true or false")),
        };
        if facets.is_empty() && !candidates {
            return Err(bad(
                "name at least one facet, or ask for the candidates with \"candidates\": true",
            ));
        }
        let at = match o.get("at") {
            None | Some(J::Null) => None,
            Some(J::String(s)) => Some(s.parse::<At>().map_err(|e| bad(format!("at: {e}")))?)
                .filter(|a| *a != At::Head),
            Some(_) => return Err(bad("at must be a string: a commit, a time or a snapshot")),
        };
        Ok(FacetsRequest {
            within,
            facets,
            filters,
            candidates,
            at,
        })
    }

    /// The request as a cache key: every member, in a fixed order.
    pub fn key(&self) -> String {
        format!("facets:{self:?}")
    }
}

fn facet_spec(v: &J) -> Result<FacetSpec> {
    let o = v
        .as_object()
        .ok_or_else(|| bad("a facet must be an object such as {\"property\": IRI}"))?;
    check_keys(
        o,
        &["property", "bins", "top", "kind", "edges", "bars"],
        "a facet",
    )?;
    let property = iri(o.get("property").unwrap_or(&J::Null), "facets[].property")?;
    let bins = bounded(o, "bins", 2, MAX_BINS, DEFAULT_BINS)?;
    let top = bounded(o, "top", 1, MAX_TOP, MAX_TOP)?;
    let kind = match o.get("kind") {
        None | Some(J::Null) => None,
        Some(J::String(s)) => Some(Kind::parse(s, "facets[].kind")?),
        Some(_) => return Err(bad("facets[].kind must be a string")),
    };
    let edges = match o.get("edges") {
        None | Some(J::Null) => None,
        Some(J::Array(a)) => {
            if !matches!(kind, Some(Kind::Numeric | Kind::Date)) {
                return Err(bad("facets[].edges needs kind \"numeric\" or \"date\""));
            }
            if a.len() < 2 || a.len() > MAX_BINS + 1 {
                return Err(bad(format!(
                    "facets[].edges must hold from 2 to {} numbers",
                    MAX_BINS + 1
                )));
            }
            let e = a
                .iter()
                .map(|x| finite(x, "facets[].edges[]"))
                .collect::<Result<Vec<_>>>()?;
            if e.windows(2).any(|w| w[0] >= w[1]) {
                return Err(bad("facets[].edges must increase"));
            }
            if kind == Some(Kind::Date) && e.iter().any(|x| x.fract() != 0.0) {
                return Err(bad("the edges of a date facet are whole years"));
            }
            Some(e)
        }
        Some(_) => return Err(bad("facets[].edges must be an array of numbers")),
    };
    let bars = match o.get("bars") {
        None | Some(J::Null) => None,
        Some(J::Array(a)) => {
            if kind != Some(Kind::Values) {
                return Err(bad("facets[].bars needs kind \"values\""));
            }
            if a.len() > top {
                return Err(bad("facets[].bars holds at most top values"));
            }
            Some(
                a.iter()
                    .map(|t| term_from_json(t, "facets[].bars[]"))
                    .collect::<Result<Vec<_>>>()?,
            )
        }
        Some(_) => return Err(bad("facets[].bars must be an array of terms")),
    };
    Ok(FacetSpec {
        property,
        bins,
        top,
        kind,
        edges,
        bars,
    })
}

fn brush(v: &J) -> Result<Brush> {
    let o = v.as_object().ok_or_else(|| {
        bad("a filter must be an object such as {\"property\": IRI, \"range\": [30, 45]}")
    })?;
    check_keys(o, &["property", "range", "values", "kind"], "a filter")?;
    let property = iri(o.get("property").unwrap_or(&J::Null), "filters[].property")?;
    match (o.get("range"), o.get("values")) {
        (Some(J::Array(r)), None) => {
            if r.len() != 2 {
                return Err(bad("filters[].range must be [from, to]"));
            }
            let (a, b) = (
                finite(&r[0], "filters[].range[0]")?,
                finite(&r[1], "filters[].range[1]")?,
            );
            if a >= b {
                return Err(bad("filters[].range must have from below to"));
            }
            let kind = match o.get("kind").and_then(J::as_str) {
                None => Kind::Numeric,
                Some(s) => match Kind::parse(s, "filters[].kind")? {
                    Kind::Values => {
                        return Err(bad("a range filters a \"numeric\" or \"date\" facet"));
                    }
                    k => k,
                },
            };
            if kind == Kind::Date && (a.fract() != 0.0 || b.fract() != 0.0) {
                return Err(bad("the range of a date facet is in whole years"));
            }
            Ok(Brush {
                property,
                kind,
                range: Some((a, b)),
                values: Vec::new(),
            })
        }
        (None, Some(J::Array(vals))) => {
            if vals.is_empty() || vals.len() > MAX_BRUSH_VALUES {
                return Err(bad(format!(
                    "filters[].values must hold from 1 to {MAX_BRUSH_VALUES} terms"
                )));
            }
            if let Some(k) = o.get("kind").and_then(J::as_str)
                && Kind::parse(k, "filters[].kind")? != Kind::Values
            {
                return Err(bad("values filter a \"values\" facet"));
            }
            Ok(Brush {
                property,
                kind: Kind::Values,
                range: None,
                values: vals
                    .iter()
                    .map(|t| term_from_json(t, "filters[].values[]"))
                    .collect::<Result<_>>()?,
            })
        }
        _ => Err(bad(
            "a filter has either range [from, to] or values [term, …]",
        )),
    }
}

/// A number as SPARQL text: an integer when it is whole, else the shortest decimal that
/// reads back as the same number. The UI writes the numbers of a brush the same way.
pub fn number_text(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// The SPARQL condition a brush puts on its value variable `f`. This is the text that
/// **Apply to query** writes in its `FILTER`.
pub fn brush_condition(b: &Brush, f: &str) -> String {
    match (b.kind, b.range) {
        (Kind::Date, Some((a, z))) => format!(
            "YEAR({f}) >= {} && YEAR({f}) < {}",
            number_text(a),
            number_text(z)
        ),
        (_, Some((a, z))) => format!("{f} >= {} && {f} < {}", number_text(a), number_text(z)),
        (_, None) => format!("{f} IN ({})", b.values.join(", ")),
    }
}

fn brush_pattern(b: &Brush, i: usize) -> String {
    let f = format!("?f{i}");
    format!(
        "FILTER EXISTS {{ ?s <{}> {f} . FILTER({}) }}\n",
        b.property,
        brush_condition(b, &f)
    )
}

/// What a facet's own queries need: the set's pattern and the brushes of the others.
struct Ctx<'a> {
    run: Run<'a>,
    set: String,
}

impl Ctx<'_> {
    fn select(&self, q: &str) -> Result<Rows> {
        self.run.select(q)
    }

    fn count(&self, filters: &str) -> Result<u64> {
        let q = format!(
            "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{\n{}{filters}}}",
            self.set
        );
        let r = self.select(&q)?;
        Ok(r.rows.first().map_or(0, |row| count(get(row, r.col("n")))))
    }

    /// A sample of the values of `p` in the set.
    fn sample(&self, p: &str) -> Result<Vec<Term>> {
        let q = format!(
            "SELECT ?v WHERE {{\n{}?s <{p}> ?v .\n}} LIMIT {KIND_SAMPLE}",
            self.set
        );
        let r = self.select(&q)?;
        let c = r.col("v");
        Ok(r.rows
            .iter()
            .filter_map(|row| get(row, c).cloned())
            .collect())
    }
}

/// The kind of a property from a sample of its values: numeric or date when nine in ten
/// of them are, else values.
fn kind_of(vals: &[Term]) -> Kind {
    let n = vals.len();
    if n == 0 {
        return Kind::Values;
    }
    let typed = |f: fn(&str) -> bool| {
        vals.iter()
            .filter(|v| literal_type(v).is_some_and(f))
            .count() as f64
            >= 0.9 * n as f64
    };
    if typed(is_numeric_type) {
        Kind::Numeric
    } else if typed(is_date_type) {
        Kind::Date
    } else {
        Kind::Values
    }
}

/// The value a range facet bins: the number, or the year of a date.
fn binned(kind: Kind, v: &str) -> String {
    match kind {
        Kind::Date => format!("YEAR({v})"),
        _ => v.to_string(),
    }
}

fn type_check(kind: Kind, v: &str) -> String {
    match kind {
        Kind::Date => {
            format!("DATATYPE({v}) IN (<{XSD}date>, <{XSD}dateTime>, <{XSD}dateTimeStamp>)")
        }
        _ => format!("isNumeric({v})"),
    }
}

/// The edges of `bins` or fewer ranges of round width over `[lo, hi]`, rounded so that
/// they read as round numbers. `None` when the values are too far apart for finite ranges.
pub fn round_edges(lo: f64, hi: f64, bins: usize, integral: bool) -> Option<Vec<f64>> {
    if !lo.is_finite() || !hi.is_finite() || hi < lo {
        return None;
    }
    let span = if integral { hi - lo + 1.0 } else { hi - lo };
    if !span.is_finite() {
        return None;
    }
    let raw = if hi > lo { span / bins as f64 } else { 1.0 };
    let mut w = nice_width(raw, integral);
    let (start, n) = loop {
        let start = (lo / w).floor() * w;
        let count = ((hi - start) / w).floor();
        if !w.is_finite() || !start.is_finite() || !count.is_finite() {
            return None;
        }
        let n = count as u64 + 1;
        if n <= bins as u64 {
            break (start, n.max(1) as usize);
        }
        let wider = nice_width(w * (1.0 + 1e-6), integral);
        if wider <= w {
            return None;
        }
        w = wider;
    };
    let decimals = if integral {
        0
    } else {
        (-(w.log10().floor()) as i32 + 1).clamp(0, 12) as usize
    };
    let mut edges: Vec<f64> = (0..=n)
        .map(|i| {
            let x = start + i as f64 * w;
            format!("{x:.decimals$}").parse::<f64>().unwrap_or(x)
        })
        .collect();
    edges.dedup();
    // the largest value lies below the last edge, which the rounding must not move
    if edges.len() < 2 || *edges.last()? <= hi || edges[0] > lo {
        return None;
    }
    Some(edges)
}

/// The answer to `req` at snapshot `snap`, as the JSON object that API.md describes under
/// "Facets", with the snapshot's `commit`. `req.at` is not read: the caller chose the
/// snapshot.
pub fn facets_at(snap: Arc<Snapshot>, req: &FacetsRequest, opts: &QueryOptions) -> Result<J> {
    let commit = snap.commit;
    let set = set_pattern(&req.within, opts)?;
    let ctx = Ctx {
        run: Run { snap, opts },
        set,
    };
    let size = ctx.count("")?;
    if size > MAX_SET {
        return Err(Error::BudgetExceeded(crate::Budget {
            kind: crate::BudgetKind::Rows,
            limit: MAX_SET,
            requested: size,
        }));
    }
    let all: String = req
        .filters
        .iter()
        .enumerate()
        .map(|(i, b)| brush_pattern(b, i))
        .collect();
    let matching = if req.filters.is_empty() {
        size
    } else {
        ctx.count(&all)?
    };
    let mut facets = Vec::with_capacity(req.facets.len());
    for spec in &req.facets {
        // every brush but the facet's own
        let others: String = req
            .filters
            .iter()
            .enumerate()
            .filter(|(_, b)| b.property != spec.property)
            .map(|(i, b)| brush_pattern(b, i))
            .collect();
        match facet(&ctx, spec, &others) {
            Ok(f) => facets.push(f),
            // a facet over its budget says why, and the others stay
            Err(Error::BudgetExceeded(b)) => facets.push(json!({
                "property": spec.property,
                "error": { "budget": b.kind.as_str(), "message": b.to_string() },
            })),
            Err(e) => return Err(e),
        }
    }
    let mut answer = json!({
        "commit": commit,
        "size": size,
        "matching": matching,
        "facets": facets,
        "truncated": false,
    });
    if req.candidates {
        let (list, truncated) = candidates(&ctx, &req.within, size)?;
        answer["candidates"] = J::Array(list);
        answer["truncated"] = truncated.into();
    }
    Ok(answer)
}

/// The pattern that binds `?s` to each entity of the set once.
fn set_pattern(set: &FacetSet, opts: &QueryOptions) -> Result<String> {
    Ok(match set {
        FacetSet::Class(c) => format!("?s <{RDF_TYPE}> <{c}> .\n"),
        FacetSet::Nodes(nodes) => {
            let mut seen = HashSet::new();
            let terms: Vec<String> = nodes
                .iter()
                .filter(|n| seen.insert(n.as_str()))
                .map(|n| format!("<{n}>"))
                .collect();
            format!("VALUES ?s {{ {} }}\n", terms.join(" "))
        }
        FacetSet::Query { query, variable } => {
            let parsed =
                crate::sparql::parse_query(query, opts.base_iri.as_deref(), &opts.prefixes)?;
            let spargebra::Query::Select {
                dataset, pattern, ..
            } = parsed
            else {
                return Err(bad("within.query must be a SELECT query"));
            };
            if dataset.is_some() {
                return Err(bad(
                    "within.query may not have FROM or FROM NAMED: name the graphs with default-graph-uri and named-graph-uri",
                ));
            }
            let mut projected = false;
            pattern.on_in_scope_variable(|v| projected |= v.as_str() == variable);
            if !projected {
                return Err(bad(format!("within.query does not project ?{variable}")));
            }
            // the query as text with every IRI written out, so that it needs no prologue,
            // and nested twice so that its variables never meet those of the facets
            let text = spargebra::Query::Select {
                dataset: None,
                pattern,
                base_iri: None,
            }
            .to_string();
            let inner = if variable == "sparkles_set" {
                "?sparkles_set".to_string()
            } else {
                format!("(?{variable} AS ?sparkles_set)")
            };
            format!(
                "{{ SELECT (?sparkles_set AS ?s) WHERE {{ {{ SELECT DISTINCT {inner} WHERE {{ {text} }} }} FILTER(BOUND(?sparkles_set)) }} }}\n"
            )
        }
    })
}

/// One facet's counts over the set filtered by `others`.
fn facet(ctx: &Ctx, spec: &FacetSpec, others: &str) -> Result<J> {
    let p = &spec.property;
    let (kind, sample) = match (spec.kind, &spec.edges, &spec.bars) {
        (Some(k), Some(_), _) | (Some(k), _, Some(_)) => (k, Vec::new()),
        (k, _, _) => {
            let s = ctx.sample(p)?;
            (k.unwrap_or_else(|| kind_of(&s)), s)
        }
    };
    if kind == Kind::Values {
        return values_facet(ctx, spec, others);
    }
    let edges = match &spec.edges {
        Some(e) => e.clone(),
        None => {
            let num = binned(kind, "?v");
            let q = format!(
                "SELECT (MIN({num}) AS ?lo) (MAX({num}) AS ?hi) WHERE {{\n{}?s <{p}> ?v .\nFILTER({})\n}}",
                ctx.set,
                type_check(kind, "?v")
            );
            let r = ctx.select(&q)?;
            let bounds = r.rows.first().and_then(|row| {
                Some((
                    number(get(row, r.col("lo")))?,
                    number(get(row, r.col("hi")))?,
                ))
            });
            let integral = kind == Kind::Date
                || sample
                    .iter()
                    .filter_map(literal_type)
                    .filter(|t| is_numeric_type(t))
                    .all(is_integer_type);
            match bounds.and_then(|(lo, hi)| round_edges(lo, hi, spec.bins, integral)) {
                Some(e) => e,
                // no values of the kind, or values too far apart for ranges: a bar list
                None if bounds.is_some() => return values_facet(ctx, spec, others),
                None => Vec::new(),
            }
        }
    };
    let mut bins: Vec<J> = Vec::new();
    let mut missing;
    if edges.len() < 2 {
        missing = ctx.count(others)?;
    } else {
        let n = edges.len() - 1;
        let lit: Vec<String> = edges.iter().map(|e| number_text(*e)).collect();
        let key = if n == 1 {
            "0".to_string()
        } else {
            (1..n)
                .map(|i| format!("IF(?x >= {}, 1, 0)", lit[i]))
                .collect::<Vec<_>>()
                .join(" + ")
        };
        let q = format!(
            "SELECT ?k (COUNT(DISTINCT ?s) AS ?n) WHERE {{\n{}{others}OPTIONAL {{ ?s <{p}> ?v . BIND({} AS ?x) FILTER({} && ?x >= {} && ?x < {}) BIND({key} AS ?k) }}\n}} GROUP BY ?k",
            ctx.set,
            binned(kind, "?v"),
            type_check(kind, "?v"),
            lit[0],
            lit[n]
        );
        let r = ctx.select(&q)?;
        let (kc, nc) = (r.col("k"), r.col("n"));
        let mut counts = vec![0u64; n];
        missing = 0;
        for row in &r.rows {
            let c = count(get(row, nc));
            match number(get(row, kc)) {
                Some(k) if k >= 0.0 && (k as usize) < n => counts[k as usize] += c,
                _ => missing += c,
            }
        }
        for (i, c) in counts.iter().enumerate() {
            bins.push(json!({ "from": edges[i], "to": edges[i + 1], "count": c }));
        }
    }
    Ok(json!({
        "property": p,
        "kind": kind.as_str(),
        "bins": bins,
        "missing": missing,
    }))
}

fn values_facet(ctx: &Ctx, spec: &FacetSpec, others: &str) -> Result<J> {
    let p = &spec.property;
    let (bars, terms): (Vec<String>, Vec<Option<Term>>) = match &spec.bars {
        Some(b) => (b.clone(), vec![None; b.len()]),
        None => {
            let q = format!(
                "SELECT ?v (COUNT(DISTINCT ?s) AS ?n) WHERE {{\n{}?s <{p}> ?v .\nFILTER(isIRI(?v) || isLiteral(?v))\n}} GROUP BY ?v ORDER BY DESC(?n) ?v LIMIT {}",
                ctx.set, spec.top
            );
            let r = ctx.select(&q)?;
            let c = r.col("v");
            let t: Vec<Term> = r
                .rows
                .iter()
                .filter_map(|row| get(row, c).cloned())
                .collect();
            (
                t.iter().map(Term::to_string).collect(),
                t.into_iter().map(Some).collect(),
            )
        }
    };
    let base = ctx.count(others)?;
    let q = format!(
        "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{\n{}{others}FILTER NOT EXISTS {{ ?s <{p}> ?any }}\n}}",
        ctx.set
    );
    let r = ctx.select(&q)?;
    let missing = r.rows.first().map_or(0, |row| count(get(row, r.col("n"))));
    let mut counts: HashMap<String, (Term, u64)> = HashMap::new();
    let mut unbound = 0u64;
    if !bars.is_empty() {
        let q = format!(
            "SELECT ?k (COUNT(DISTINCT ?s) AS ?n) WHERE {{\n{}{others}OPTIONAL {{ VALUES ?k {{ {} }} ?s <{p}> ?k }}\n}} GROUP BY ?k",
            ctx.set,
            bars.join(" ")
        );
        let r = ctx.select(&q)?;
        let (kc, nc) = (r.col("k"), r.col("n"));
        for row in &r.rows {
            let c = count(get(row, nc));
            match get(row, kc) {
                Some(t) => {
                    counts.insert(t.to_string(), (t.clone(), c));
                }
                None => unbound += c,
            }
        }
    } else {
        unbound = base;
    }
    let iris: Vec<String> = bars
        .iter()
        .filter_map(|b| b.strip_prefix('<').and_then(|x| x.strip_suffix('>')))
        .map(str::to_string)
        .collect();
    let refs: Vec<&str> = iris.iter().map(String::as_str).collect();
    let names = ctx.run.labels(&refs)?;
    let values: Vec<J> = bars
        .iter()
        .zip(terms)
        .filter_map(|(b, t)| {
            let (term, c) = match counts.get(b) {
                Some((t, c)) => (t.clone(), *c),
                None => (t?, 0),
            };
            let mut o = json!({ "value": term_json(&term), "count": c });
            if let Term::NamedNode(n) = &term
                && let Some(l) = names.get(n.as_str())
            {
                o["label"] = l.clone().into();
            }
            Some(o)
        })
        .collect();
    Ok(json!({
        "property": p,
        "kind": "values",
        "values": values,
        "other": unbound.saturating_sub(missing),
        "missing": missing,
    }))
}

/// The properties of a sample of the set, ranked by the share of the sample that has a
/// value, with the kind each would be counted by. The second member says whether the
/// sample was smaller than the set or the list was cut.
fn candidates(ctx: &Ctx, within: &FacetSet, size: u64) -> Result<(Vec<J>, bool)> {
    let q = format!(
        "SELECT ?s ?p ?o WHERE {{ {{ SELECT DISTINCT ?s WHERE {{\n{}}} LIMIT {CANDIDATE_SAMPLE} }} ?s ?p ?o . }} LIMIT 200000",
        ctx.set
    );
    let r = ctx.select(&q)?;
    let (sc, pc, oc) = (r.col("s"), r.col("p"), r.col("o"));
    let mut subjects: HashSet<String> = HashSet::new();
    let mut by_p: HashMap<String, (HashSet<String>, Vec<Term>)> = HashMap::new();
    for row in &r.rows {
        let (Some(s), Some(Term::NamedNode(p)), Some(o)) =
            (get(row, sc), get(row, pc), get(row, oc))
        else {
            continue;
        };
        let s = s.to_string();
        subjects.insert(s.clone());
        let e = by_p.entry(p.as_str().to_string()).or_default();
        e.0.insert(s);
        e.1.push(o.clone());
    }
    let n = subjects.len().max(1) as f64;
    let mut list: Vec<(f64, String, Kind)> = Vec::new();
    for (p, (subs, vals)) in by_p {
        if LABELS.contains(&p.as_str()) {
            continue;
        }
        if p == RDF_TYPE && matches!(within, FacetSet::Class(_)) {
            continue;
        }
        let vals: Vec<Term> = vals
            .into_iter()
            .filter(|v| matches!(v, Term::NamedNode(_) | Term::Literal(_)))
            .collect();
        if vals.is_empty() {
            continue;
        }
        let kind = kind_of(&vals);
        if kind == Kind::Values {
            // values that are nearly all different make a poor bar list
            let distinct: HashSet<String> = vals.iter().map(Term::to_string).collect();
            if distinct.len() > MAX_TOP && distinct.len() * 2 > vals.len() {
                continue;
            }
        }
        list.push((subs.len() as f64 / n, p, kind));
    }
    // literal kinds first at equal share, then by IRI
    list.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| (a.2 == Kind::Values).cmp(&(b.2 == Kind::Values)))
            .then_with(|| a.1.cmp(&b.1))
    });
    let cut = list.len() > MAX_CANDIDATES;
    list.truncate(MAX_CANDIDATES);
    let out = list
        .into_iter()
        .map(|(share, p, kind)| {
            json!({
                "property": p,
                "share": (share * 1000.0).round() / 1000.0,
                "kind": kind.as_str(),
            })
        })
        .collect();
    Ok((out, cut || size > subjects.len() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_edges_hold_every_value() {
        let e = round_edges(18.0, 90.0, 20, true).unwrap();
        assert!(e.len() - 1 <= 20);
        assert!(e[0] <= 18.0 && *e.last().unwrap() > 90.0);
        assert!(e.iter().all(|x| x.fract() == 0.0));
        let e = round_edges(0.05, 0.93, 10, false).unwrap();
        assert_eq!(e[0], 0.0);
        assert!(e.iter().all(|x| format!("{x}").len() <= 4), "{e:?}");
        let e = round_edges(5.0, 5.0, 20, true).unwrap();
        assert_eq!(e, vec![5.0, 6.0]);
        assert!(round_edges(-1e308, 1e308, 20, false).is_none());
    }

    #[test]
    fn brushes_read_as_the_text_apply_writes() {
        let b = Brush {
            property: "http://ex/age".into(),
            kind: Kind::Numeric,
            range: Some((30.0, 45.0)),
            values: vec![],
        };
        assert_eq!(brush_condition(&b, "?age"), "?age >= 30 && ?age < 45");
        let d = Brush {
            kind: Kind::Date,
            range: Some((2001.0, 2010.0)),
            ..b.clone()
        };
        assert_eq!(
            brush_condition(&d, "?d"),
            "YEAR(?d) >= 2001 && YEAR(?d) < 2010"
        );
        let v = Brush {
            kind: Kind::Values,
            range: None,
            values: vec!["\"FR\"".into(), "<http://ex/de>".into()],
            ..b
        };
        assert_eq!(brush_condition(&v, "?c"), "?c IN (\"FR\", <http://ex/de>)");
        assert_eq!(number_text(0.3), "0.3");
        assert_eq!(number_text(-2.5), "-2.5");
    }

    #[test]
    fn requests_are_checked() {
        let ok = |j: J| FacetsRequest::from_json(&j);
        assert!(
            ok(
                json!({"within": {"class": "http://ex/P"}, "facets": [{"property": "http://ex/a"}]})
            )
            .is_ok()
        );
        assert!(ok(json!({"within": {"class": "http://ex/P"}})).is_err());
        assert!(ok(json!({"within": {"nodes": []}, "candidates": true})).is_err());
        assert!(ok(json!({"within": {"class": "http://ex/P"}, "facets": [{"property": "http://ex/a", "bins": 41}]})).is_err());
        assert!(ok(json!({"within": {"class": "http://ex/P"}, "facets": [{"property": "http://ex/a", "edges": [1, 2]}]})).is_err());
        assert!(ok(json!({"within": {"class": "http://ex/P"}, "facets": [{"property": "http://ex/a", "kind": "numeric", "edges": [2, 1]}]})).is_err());
        assert!(ok(json!({"within": {"class": "http://ex/P"}, "candidates": true, "filters": [{"property": "http://ex/a", "range": [3, 3]}]})).is_err());
        assert!(ok(json!({"within": {"class": "http://ex/P"}, "candidates": true, "filters": [{"property": "http://ex/a", "values": [{"type": "bnode", "value": "b"}]}]})).is_err());
        assert!(
            ok(json!({"within": {"class": "http://ex/P"}, "candidates": true, "branch": "x"}))
                .is_err()
        );
        let facets: Vec<J> = (0..7)
            .map(|i| json!({"property": format!("http://ex/{i}")}))
            .collect();
        assert!(ok(json!({"within": {"class": "http://ex/P"}, "facets": facets})).is_err());
    }
}
