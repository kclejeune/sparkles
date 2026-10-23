//! The answer of the groups route, from SPARQL queries over the caller's view.
//!
//! A set is a class and a path of partitions with the key of each, as a group id holds
//! them. Its SPARQL pattern types the node with the class and, per partition, requires
//! the value, range or community of the key, or the absence of every one for the
//! partition's "other" group. A partition's key of any node is computed in the query with
//! an `OPTIONAL` and read back here, so one query counts the edges from the groups of a
//! set to the groups of every partition on the path at once. A node with several values
//! of a grouping property is in each of their groups.

use super::ids::{GroupId, Key, Level, MAX_DEPTH, MAX_ID, Step, canonical_term};
use super::{By, GroupsRequest, Within};
use crate::http::{ApiError, ApiResult, err};
use axum::http::StatusCode;
use oxrdf::Term;
use serde_json::{Value as J, json};
use sparkles::sparql::QueryOptions;
use sparkles::store::Snapshot;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Groups or members in one answer at most, and the size under which a set opens into
/// its members.
pub(crate) const MAX_LIMIT: usize = 5_000;
/// Edges in one answer at most.
pub(crate) const MAX_EDGES: usize = 20_000;
/// Values or ranges of a property partition, by default and at most.
pub(crate) const DEFAULT_BUCKETS: usize = 12;
pub(crate) const MAX_BUCKETS: usize = 48;
/// Rows of one aggregated edge query at most, before they are merged.
const EDGE_ROWS: usize = 100_000;
/// Instances read to choose a grouping property.
const SAMPLE: usize = 200;
/// Groups whose first members are listed, and how many.
const TOP_GROUPS: usize = 12;
const TOP_MEMBERS: usize = 3;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const SKOS_BROADER: &str = "http://www.w3.org/2004/02/skos/core#broader";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
/// The IRIs F14's write-back gives community resources.
const COMMUNITY_PREFIX: &str = "urn:x-sparkles:algo:";
/// Label properties, as Explore reads them.
const LABELS: [&str; 5] = [
    "http://www.w3.org/2000/01/rdf-schema#label",
    "http://www.w3.org/2004/02/skos/core#prefLabel",
    "http://xmlns.com/foaf/0.1/name",
    "http://purl.org/dc/terms/title",
    "http://schema.org/name",
];

pub(in crate::http) struct Run<'a> {
    pub snap: Arc<Snapshot>,
    pub opts: &'a QueryOptions,
}

/// The solutions of one query, with their variables.
struct Rows {
    vars: Vec<String>,
    rows: Vec<Vec<Option<Term>>>,
}

impl Rows {
    fn col(&self, name: &str) -> usize {
        self.vars
            .iter()
            .position(|v| v == name)
            .unwrap_or(usize::MAX)
    }
}

fn get(row: &[Option<Term>], i: usize) -> Option<&Term> {
    row.get(i).and_then(Option::as_ref)
}

fn count(t: Option<&Term>) -> u64 {
    match t {
        Some(Term::Literal(l)) => l.value().parse::<u64>().unwrap_or(0),
        _ => 0,
    }
}

fn number(t: Option<&Term>) -> Option<f64> {
    match t {
        Some(Term::Literal(l)) => l.value().parse::<f64>().ok().filter(|f| f.is_finite()),
        _ => None,
    }
}

fn iri_of(t: Option<&Term>) -> Option<&str> {
    match t {
        Some(Term::NamedNode(n)) => Some(n.as_str()),
        _ => None,
    }
}

/// A finite number as an `xsd:double` literal.
fn dbl(x: f64) -> String {
    format!("\"{x:?}\"^^<{XSD}double>")
}

fn label_path() -> String {
    format!(
        "({})",
        LABELS
            .iter()
            .map(|l| format!("<{l}>"))
            .collect::<Vec<_>>()
            .join("|")
    )
}

fn is_numeric_type(dt: &str) -> bool {
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

fn is_integer_type(dt: &str) -> bool {
    is_numeric_type(dt) && !matches!(dt.strip_prefix(XSD), Some("decimal" | "double" | "float"))
}

fn is_date_type(dt: &str) -> bool {
    matches!(
        dt.strip_prefix(XSD),
        Some("date" | "dateTime" | "dateTimeStamp")
    )
}

fn literal_type(t: &Term) -> Option<&str> {
    match t {
        Term::Literal(l) => Some(l.datatype().as_str()),
        _ => None,
    }
}

// ------------------------------------------------------------------- patterns ------

fn range_type(x: &str, year: bool) -> String {
    if year {
        format!("DATATYPE({x}) IN (<{XSD}date>, <{XSD}dateTime>, <{XSD}dateTimeStamp>)")
    } else {
        format!("isNumeric({x})")
    }
}

fn range_expr(x: &str, lo: f64, w: f64, year: bool) -> String {
    let num = if year {
        format!("YEAR({x})")
    } else {
        x.to_string()
    };
    format!("FLOOR(({num} - {}) / {})", dbl(lo), dbl(w))
}

fn community_cond(x: &str, parent: Option<&str>, u: &str) -> String {
    match parent {
        Some(g) => format!("{x} <{SKOS_BROADER}> <{g}> ."),
        None => format!("FILTER NOT EXISTS {{ {x} <{SKOS_BROADER}> {u} }}"),
    }
}

/// The pattern of the nodes of `step`'s group, for node variable `v` at path index `i`.
fn step_filter(step: &Step, v: &str, i: usize) -> String {
    let x = format!("?{v}_x{i}");
    match (&step.level, &step.key) {
        (Level::Values { p, vals }, Key::Value(j)) => format!("?{v} <{p}> {} .\n", vals[*j]),
        (Level::Values { p, vals }, Key::Other) if !vals.is_empty() => format!(
            "FILTER NOT EXISTS {{ VALUES {x} {{ {} }} ?{v} <{p}> {x} }}\n",
            vals.join(" ")
        ),
        (Level::Range { p, lo, w, n, year }, Key::Bucket(b)) => {
            let e = range_expr(&x, *lo, *w, *year);
            let mut f = vec![range_type(&x, *year)];
            match (*b == 0, *b + 1 >= *n) {
                (true, true) => {}
                (true, false) => f.push(format!("{e} <= 0")),
                (false, true) => f.push(format!("{e} >= {}", n - 1)),
                (false, false) => f.push(format!("{e} = {b}")),
            }
            // an existence test, so a node with several values in the range is bound
            // once and its links are not counted once per value
            format!(
                "FILTER EXISTS {{ ?{v} <{p}> {x} . FILTER({}) }}\n",
                f.join(" && ")
            )
        }
        (Level::Range { p, year, .. }, Key::Other) => format!(
            "FILTER NOT EXISTS {{ ?{v} <{p}> {x} . FILTER({}) }}\n",
            range_type(&x, *year)
        ),
        (Level::Community { p, .. }, Key::Community(c)) => format!("?{v} <{p}> <{c}> .\n"),
        (Level::Community { p, parent }, Key::Other) => format!(
            "FILTER NOT EXISTS {{ ?{v} <{p}> {x} . {} }}\n",
            community_cond(&x, parent.as_deref(), &format!("?{v}_u{i}"))
        ),
        _ => String::new(),
    }
}

/// The pattern of a set: the class and each partition's key.
fn set_pattern(class: &str, path: &[Step], v: &str) -> String {
    let mut q = format!("?{v} <{RDF_TYPE}> <{class}> .\n");
    for (i, s) in path.iter().enumerate() {
        q.push_str(&step_filter(s, v, i));
    }
    q
}

/// An `OPTIONAL` that binds `out` to node `v`'s key in partition `level` (unbound: the
/// "other" group).
fn key_expr(level: &Level, v: &str, i: usize, out: &str) -> String {
    let y = format!("?{v}_y{i}");
    match level {
        Level::Values { p, vals } if vals.is_empty() => {
            let _ = p;
            String::new()
        }
        Level::Values { p, vals } => format!(
            "OPTIONAL {{ VALUES {out} {{ {} }} ?{v} <{p}> {out} }}\n",
            vals.join(" ")
        ),
        Level::Range { p, lo, w, year, .. } => format!(
            "OPTIONAL {{ ?{v} <{p}> {y} . FILTER({}) BIND({} AS {out}) }}\n",
            range_type(&y, *year),
            range_expr(&y, *lo, *w, *year)
        ),
        Level::Community { p, parent } => format!(
            "OPTIONAL {{ ?{v} <{p}> {out} . {} }}\n",
            community_cond(out, parent.as_deref(), &format!("?{v}_w{i}"))
        ),
    }
}

/// The key a node has in partition `level`, from the term its key expression bound.
fn key_of(level: &Level, t: Option<&Term>) -> Key {
    let Some(t) = t else { return Key::Other };
    match level {
        Level::Values { vals, .. } => {
            let s = t.to_string();
            vals.iter()
                .position(|v| *v == s)
                .map_or(Key::Other, Key::Value)
        }
        Level::Range { n, .. } => match number(Some(t)) {
            Some(f) => Key::Bucket(f.max(0.0).min(f64::from(n - 1)) as u32),
            None => Key::Other,
        },
        Level::Community { .. } => match t {
            Term::NamedNode(c) => Key::Community(c.as_str().to_string()),
            _ => Key::Other,
        },
    }
}

fn child(class: &str, path: &[Step], level: &Level, key: Key) -> GroupId {
    let mut p = path.to_vec();
    p.push(Step {
        level: level.clone(),
        key,
    });
    GroupId {
        class: class.to_string(),
        path: p,
    }
}

/// Where a node of the class lies relative to a set: inside it, or in the group of the
/// first partition on the path where its key differs from the set's.
enum Owner {
    Inside,
    Sibling(String),
}

fn owner(class: &str, path: &[Step], keys: &[Option<&Term>]) -> Owner {
    for (i, s) in path.iter().enumerate() {
        let k = key_of(&s.level, keys.get(i).copied().flatten());
        if k != s.key {
            return Owner::Sibling(child(class, &path[..i], &s.level, k).encode());
        }
    }
    Owner::Inside
}

// --------------------------------------------------------------------- answer ------

/// One group of a partition.
struct Child {
    key: Key,
    size: u64,
    label: String,
}

struct Partition {
    level: Level,
    children: Vec<Child>,
    truncated: bool,
    kind: &'static str,
}

/// Edges keyed by source, target and predicate, with their counts.
#[derive(Default)]
struct Edges {
    map: HashMap<(String, String, Option<String>), u64>,
    truncated: bool,
}

impl Edges {
    fn add(&mut self, source: String, target: String, predicate: Option<&str>, n: u64) {
        if source == target && source.starts_with("c:") {
            return;
        }
        let key = (source, target, predicate.map(str::to_string));
        if !self.map.contains_key(&key) && self.map.len() >= MAX_EDGES {
            self.truncated = true;
            return;
        }
        *self.map.entry(key).or_insert(0) += n;
    }

    fn json(self) -> (Vec<J>, bool) {
        let mut v: Vec<_> = self.map.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        (
            v.into_iter()
                .map(|((s, t, p), n)| json!({ "source": s, "target": t, "predicate": p, "count": n }))
                .collect(),
            self.truncated,
        )
    }
}

impl Run<'_> {
    fn select(&self, q: &str) -> ApiResult<Rows> {
        let r = sparkles::sparql::query_with_plan(self.snap.clone(), q, self.opts)
            .map_err(|f| ApiError::from(f.error))?;
        Ok(Rows {
            rows: r.rows(),
            vars: r.vars,
        })
    }

    /// The answer to a request.
    pub(in crate::http) fn answer(&self, req: &GroupsRequest) -> ApiResult<J> {
        let (class, path, group) = match &req.within {
            Within::Class(c) => (c.clone(), Vec::new(), None),
            Within::Group(id) => {
                let g = GroupId::decode(id).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
                (g.class, g.path, Some(id.clone()))
            }
        };
        let size = self.total(&class, &path)?;
        let mut note = None;
        // a group at the deepest level opens into its members only, since the ids of its
        // groups would be refused
        let deepest = path.len() >= MAX_DEPTH;
        let partition = match &req.by {
            By::Members => None,
            _ if deepest => {
                note = Some(format!(
                    "This group is {MAX_DEPTH} levels deep, the most a group id holds, so the answer lists its members."
                ));
                None
            }
            By::Auto if size <= req.limit as u64 => None,
            By::Auto => {
                let p = self.auto(&class, &path, req.limit)?;
                if p.is_none() {
                    note = Some(format!(
                        "No property groups these {size} instances well, so the answer lists the first {} of them.",
                        req.limit
                    ));
                }
                p
            }
            By::Community(p) => Some(self.communities(&class, &path, p, req.limit)?.ok_or_else(
                || {
                    err(
                        StatusCode::BAD_REQUEST,
                        format!("no communities written to <{p}> hold these instances"),
                    )
                },
            )?),
            By::Property { predicate, buckets } => Some(
                self.property(&class, &path, predicate, *buckets)?
                    .ok_or_else(|| {
                        err(
                            StatusCode::BAD_REQUEST,
                            format!("these instances have no values of <{predicate}>"),
                        )
                    })?,
            ),
        };
        let mut within = json!({ "class": class, "size": size });
        if let Some(g) = &group {
            within["group"] = J::from(g.clone());
        }
        let mut answer = match partition {
            Some(part) => match self.groups_answer(&class, &path, part, req.limit)? {
                Some(a) => a,
                None => {
                    note = Some(
                        "The ids of these groups would be longer than a group id may be, so the answer lists the members.".into(),
                    );
                    self.members_answer(&class, &path, req.limit, size)?
                }
            },
            None => self.members_answer(&class, &path, req.limit, size)?,
        };
        if let Some(o) = answer.as_object_mut() {
            o.insert("within".into(), within);
            if let Some(n) = note {
                o.insert("note".into(), n.into());
            }
        }
        Ok(answer)
    }

    fn total(&self, class: &str, path: &[Step]) -> ApiResult<u64> {
        let q = format!(
            "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{\n{}}}",
            set_pattern(class, path, "s")
        );
        let r = self.select(&q)?;
        Ok(r.rows.first().map_or(0, |row| count(get(row, r.col("n")))))
    }

    /// The groups of partition `level` over a set, largest first, at most `limit`.
    fn sizes(
        &self,
        class: &str,
        path: &[Step],
        level: &Level,
        limit: usize,
    ) -> ApiResult<(Vec<(Key, u64)>, bool)> {
        let q = format!(
            "SELECT ?k (COUNT(DISTINCT ?s) AS ?n) WHERE {{\n{}{}}} GROUP BY ?k ORDER BY DESC(?n) ?k LIMIT {}",
            set_pattern(class, path, "s"),
            key_expr(level, "s", path.len(), "?k"),
            limit + 2
        );
        let r = self.select(&q)?;
        let (k, n) = (r.col("k"), r.col("n"));
        let mut out: Vec<(Key, u64)> = Vec::new();
        for row in &r.rows {
            let key = key_of(level, get(row, k));
            let size = count(get(row, n));
            match out.iter_mut().find(|(o, _)| *o == key) {
                Some((_, s)) => *s += size,
                None => out.push((key, size)),
            }
        }
        out.sort_by_key(|b| std::cmp::Reverse(b.1));
        let truncated = out.len() > limit;
        out.truncate(limit);
        Ok((out, truncated))
    }

    /// Labels of IRIs, from the label properties.
    fn labels(&self, iris: &[&str]) -> ApiResult<HashMap<String, String>> {
        let mut out = HashMap::default();
        for chunk in iris.chunks(1_000) {
            if chunk.is_empty() {
                continue;
            }
            let values: Vec<String> = chunk.iter().map(|i| format!("<{i}>")).collect();
            let q = format!(
                "SELECT ?s (SAMPLE(?l) AS ?label) WHERE {{ VALUES ?s {{ {} }} ?s {} ?l . FILTER(isLiteral(?l)) }} GROUP BY ?s",
                values.join(" "),
                label_path()
            );
            let r = self.select(&q)?;
            let (s, l) = (r.col("s"), r.col("label"));
            for row in &r.rows {
                if let (Some(iri), Some(Term::Literal(lit))) = (iri_of(get(row, s)), get(row, l)) {
                    out.insert(iri.to_string(), lit.value().to_string());
                }
            }
        }
        Ok(out)
    }

    /// The partition by property `p`: its most frequent values, or ranges of its numbers
    /// or of the years of its dates when it has more values than `buckets`. `None` when
    /// the set has no values of `p`.
    fn property(
        &self,
        class: &str,
        path: &[Step],
        p: &str,
        buckets: usize,
    ) -> ApiResult<Option<Partition>> {
        let q = format!(
            "SELECT ?v (COUNT(DISTINCT ?s) AS ?n) WHERE {{\n{}?s <{p}> ?v .\n}} GROUP BY ?v ORDER BY DESC(?n) ?v LIMIT {}",
            set_pattern(class, path, "s"),
            buckets + 1
        );
        let r = self.select(&q)?;
        let vcol = r.col("v");
        let top: Vec<Term> = r
            .rows
            .iter()
            .filter_map(|row| get(row, vcol).cloned())
            .filter(|t| matches!(t, Term::NamedNode(_) | Term::Literal(_)))
            .collect();
        if top.is_empty() {
            return Ok(None);
        }
        let types: Vec<&str> = top.iter().filter_map(literal_type).collect();
        let numeric = types.len() == top.len() && types.iter().all(|t| is_numeric_type(t));
        let dates = types.len() == top.len() && types.iter().all(|t| is_date_type(t));
        if top.len() > buckets && (numeric || dates) {
            let integral = dates || types.iter().all(|t| is_integer_type(t));
            if let Some(part) = self.ranges(class, path, p, buckets, dates, integral)? {
                return Ok(Some(part));
            }
        }
        let vals: Vec<Term> = if top.len() <= buckets {
            top
        } else {
            top.into_iter().take(buckets - 1).collect()
        };
        let more = r.rows.len() > buckets;
        let level = Level::Values {
            p: p.to_string(),
            vals: vals
                .iter()
                .map(|t| canonical_term(&t.to_string()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?,
        };
        let (sizes, _) = self.sizes(class, path, &level, buckets + 1)?;
        let iris: Vec<&str> = vals
            .iter()
            .filter_map(|t| match t {
                Term::NamedNode(n) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        let names = self.labels(&iris)?;
        let children = sizes
            .into_iter()
            .map(|(key, size)| {
                let label = match &key {
                    Key::Value(i) => match &vals[*i] {
                        Term::NamedNode(n) => names
                            .get(n.as_str())
                            .cloned()
                            .unwrap_or_else(|| n.as_str().to_string()),
                        Term::Literal(l) => l.value().to_string(),
                        t => t.to_string(),
                    },
                    _ if more => "Other values".to_string(),
                    _ => "No value".to_string(),
                };
                Child { key, size, label }
            })
            .collect();
        Ok(Some(Partition {
            level,
            children,
            truncated: false,
            kind: "values",
        }))
    }

    /// The partition by ranges of the numbers of `p`, or of the years of its dates.
    fn ranges(
        &self,
        class: &str,
        path: &[Step],
        p: &str,
        buckets: usize,
        year: bool,
        integral: bool,
    ) -> ApiResult<Option<Partition>> {
        let num = if year { "YEAR(?x)" } else { "?x" };
        let q = format!(
            "SELECT (MIN({num}) AS ?lo) (MAX({num}) AS ?hi) WHERE {{\n{}?s <{p}> ?x .\nFILTER({})\n}}",
            set_pattern(class, path, "s"),
            range_type("?x", year)
        );
        let r = self.select(&q)?;
        let Some(row) = r.rows.first() else {
            return Ok(None);
        };
        let (Some(lo), Some(hi)) = (number(get(row, r.col("lo"))), number(get(row, r.col("hi"))))
        else {
            return Ok(None);
        };
        // a round width (1, 2, 2.5 or 5 times a power of ten) and a start on a multiple of
        // it, so that the labels read as round numbers
        let raw = if hi > lo {
            let span = if integral { hi - lo + 1.0 } else { hi - lo };
            span / buckets as f64
        } else {
            1.0
        };
        // aligning the start can add a range: widen to the next round width until the
        // ranges are at most the requested number
        let mut w = nice_width(raw, integral);
        let (lo, n) = loop {
            let start = (lo / w).floor() * w;
            let n = ((hi - start) / w).floor() as u64 + 1;
            if n <= buckets as u64 || !w.is_finite() {
                break (start, n.clamp(1, buckets as u64) as u32);
            }
            w = nice_width(w * (1.0 + 1e-6), integral);
        };
        let level = Level::Range {
            p: p.to_string(),
            lo,
            w,
            n,
            year,
        };
        let (mut sizes, _) = self.sizes(class, path, &level, n as usize + 1)?;
        // in the order of the ranges, the values-less group last
        sizes.sort_by_key(|(k, _)| match k {
            Key::Bucket(b) => *b,
            _ => u32::MAX,
        });
        let decimals = if integral {
            0
        } else {
            (-(w.log10().floor()) as i32 + 1).clamp(0, 12) as usize
        };
        let fmt = |x: f64| {
            let s = format!("{x:.decimals$}");
            if decimals == 0 {
                s
            } else {
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            }
        };
        let children = sizes
            .into_iter()
            .map(|(key, size)| {
                let label = match &key {
                    Key::Bucket(b) => {
                        let a = lo + f64::from(*b) * w;
                        let z = if *b + 1 >= n {
                            hi.max(a)
                        } else if integral {
                            a + w - 1.0
                        } else {
                            a + w
                        };
                        if integral && (z - a).abs() < 0.5 {
                            fmt(a)
                        } else {
                            format!("{} – {}", fmt(a), fmt(z))
                        }
                    }
                    _ => "No value".to_string(),
                };
                Child { key, size, label }
            })
            .collect();
        Ok(Some(Partition {
            level,
            children,
            truncated: false,
            kind: "ranges",
        }))
    }

    /// The partition by the communities F14 wrote to `p`: the coarsest ones, or the
    /// children of the community the set is. `None` when no community holds the set.
    fn communities(
        &self,
        class: &str,
        path: &[Step],
        p: &str,
        limit: usize,
    ) -> ApiResult<Option<Partition>> {
        let parent = match path.last() {
            Some(Step {
                level: Level::Community { p: lp, .. },
                key: Key::Community(c),
            }) if lp == p => Some(c.clone()),
            _ => None,
        };
        let level = Level::Community {
            p: p.to_string(),
            parent,
        };
        let (sizes, truncated) = self.sizes(class, path, &level, limit)?;
        if !sizes.iter().any(|(k, _)| matches!(k, Key::Community(_))) {
            return Ok(None);
        }
        let iris: Vec<&str> = sizes
            .iter()
            .filter_map(|(k, _)| match k {
                Key::Community(c) => Some(c.as_str()),
                _ => None,
            })
            .collect();
        let names = self.labels(&iris)?;
        let children = sizes
            .iter()
            .map(|(key, size)| {
                let label = match key {
                    Key::Community(c) => names.get(c).cloned().unwrap_or_else(|| {
                        match c.strip_prefix(COMMUNITY_PREFIX).map(|r| r.rsplit(':')) {
                            Some(mut parts) => {
                                format!("Community {}", parts.next().unwrap_or(c))
                            }
                            None => c.clone(),
                        }
                    }),
                    _ => "No community".to_string(),
                };
                Child {
                    key: key.clone(),
                    size: *size,
                    label,
                }
            })
            .collect();
        Ok(Some(Partition {
            level,
            children,
            truncated,
            kind: "communities",
        }))
    }

    /// The grouping for a set too large to list: F14's communities when its instances
    /// have some, else the property whose values split a sample of them best.
    fn auto(&self, class: &str, path: &[Step], limit: usize) -> ApiResult<Option<Partition>> {
        // a set that is a community opens into its child communities first
        if let Some(Step {
            level: Level::Community { p, .. },
            key: Key::Community(_),
        }) = path.last()
            && let Some(part) = self.communities(class, path, p, limit)?
        {
            return Ok(Some(part));
        }
        let q = format!(
            "SELECT ?s ?p ?o WHERE {{ {{ SELECT DISTINCT ?s WHERE {{\n{}}} LIMIT {SAMPLE} }} ?s ?p ?o . }} LIMIT 20000",
            set_pattern(class, path, "s")
        );
        let r = self.select(&q)?;
        let (sc, pc, oc) = (r.col("s"), r.col("p"), r.col("o"));
        let mut subjects: HashSet<String> = HashSet::default();
        let mut by_p: HashMap<&str, (HashSet<String>, Vec<&Term>)> = HashMap::default();
        for row in &r.rows {
            let (Some(s), Some(p), Some(o)) = (get(row, sc), iri_of(get(row, pc)), get(row, oc))
            else {
                continue;
            };
            let s = s.to_string();
            subjects.insert(s.clone());
            let e = by_p.entry(p).or_default();
            e.0.insert(s);
            e.1.push(o);
        }
        let used: HashSet<&str> = path
            .iter()
            .map(|s| match &s.level {
                Level::Values { p, .. } | Level::Range { p, .. } | Level::Community { p, .. } => {
                    p.as_str()
                }
            })
            .collect();
        let n = subjects.len().max(1) as f64;
        let mut community: Option<(f64, &str)> = None;
        let mut best: Option<(u8, f64, &str)> = None;
        for (p, (subs, vals)) in &by_p {
            if *p == RDF_TYPE || LABELS.contains(p) || used.contains(p) {
                continue;
            }
            let cov = subs.len() as f64 / n;
            if cov < 0.5 {
                continue;
            }
            let communities = vals.iter().all(
                |v| matches!(v, Term::NamedNode(c) if c.as_str().starts_with(COMMUNITY_PREFIX)),
            );
            if communities {
                if community.is_none_or(|(c, q)| cov > c || (cov == c && *p < q)) {
                    community = Some((cov, p));
                }
                continue;
            }
            let distinct: HashSet<String> = vals.iter().map(|v| v.to_string()).collect();
            let d = distinct.len();
            if d < 2 {
                continue;
            }
            let typed = |f: fn(&str) -> bool| {
                vals.iter()
                    .filter(|v| literal_type(v).is_some_and(f))
                    .count() as f64
                    >= 0.9 * vals.len() as f64
            };
            let tier = if d <= DEFAULT_BUCKETS {
                0
            } else if typed(is_numeric_type) || typed(is_date_type) {
                1
            } else if d * 2 <= vals.len() {
                2
            } else {
                continue;
            };
            let better = match best {
                None => true,
                Some((t, c, q)) => (tier, -cov, *p) < (t, -c, q),
            };
            if better {
                best = Some((tier, cov, p));
            }
        }
        if let Some((_, p)) = community
            && let Some(part) = self.communities(class, path, p, limit)?
        {
            return Ok(Some(part));
        }
        match best {
            Some((_, _, p)) => self.property(class, path, p, DEFAULT_BUCKETS),
            None => Ok(None),
        }
    }

    /// The first members of a group, with their labels.
    fn top(&self, class: &str, path: &[Step]) -> ApiResult<Vec<J>> {
        let q = format!(
            "SELECT ?s (SAMPLE(?l) AS ?label) WHERE {{ {{ SELECT DISTINCT ?s WHERE {{\n{}FILTER(isIRI(?s))\n}} LIMIT {TOP_MEMBERS} }} OPTIONAL {{ ?s {} ?l }} }} GROUP BY ?s",
            set_pattern(class, path, "s"),
            label_path()
        );
        let r = self.select(&q)?;
        let (s, l) = (r.col("s"), r.col("label"));
        let mut out: Vec<J> = r
            .rows
            .iter()
            .filter_map(|row| {
                let iri = iri_of(get(row, s))?;
                let mut m = json!({ "iri": iri });
                if let Some(Term::Literal(lit)) = get(row, l) {
                    m["label"] = lit.value().into();
                }
                Some(m)
            })
            .collect();
        out.sort_by(|a, b| a["iri"].as_str().cmp(&b["iri"].as_str()));
        Ok(out)
    }

    /// The edges from the members or groups of a set to the class's other nodes, by the
    /// owner each such node has on the path, and to the other classes. `source` names the
    /// set's side of a row (`?s`), whose variable `side` the query binds.
    #[allow(clippy::too_many_arguments)]
    fn outward_edges(
        &self,
        edges: &mut Edges,
        class: &str,
        path: &[Step],
        head: &str,
        side: &str,
        new_level: Option<&Level>,
        name: &dyn Fn(Option<&Term>) -> Option<String>,
    ) -> ApiResult<()> {
        let l = path.len();
        let mut keys = String::new();
        let mut vars = Vec::new();
        for (i, s) in path.iter().enumerate() {
            let v = format!("?ok{i}");
            keys.push_str(&key_expr(&s.level, "o", i, &v));
            vars.push(v);
        }
        if let Some(level) = new_level {
            let v = format!("?ok{l}");
            keys.push_str(&key_expr(level, "o", l, &v));
            vars.push(v);
        }
        for outgoing in [true, false] {
            let triple = if outgoing { "?s ?p ?o ." } else { "?o ?p ?s ." };
            // the class's own nodes, by their owner on the path
            if !vars.is_empty() {
                let q = format!(
                    "SELECT {side} ?p {} (COUNT(*) AS ?n) WHERE {{\n{head}{triple}\nFILTER(?p != <{RDF_TYPE}>)\n?o <{RDF_TYPE}> <{class}> .\n{keys}}} GROUP BY {side} ?p {} LIMIT {EDGE_ROWS}",
                    vars.join(" "),
                    vars.join(" ")
                );
                let r = self.select(&q)?;
                if r.rows.len() >= EDGE_ROWS {
                    edges.truncated = true;
                }
                let sc = r.col(&side[1..]);
                let pc = r.col("p");
                let nc = r.col("n");
                let kc: Vec<usize> = vars.iter().map(|v| r.col(&v[1..])).collect();
                for row in &r.rows {
                    let Some(me) = name(get(row, sc)) else {
                        continue;
                    };
                    let ks: Vec<Option<&Term>> = kc.iter().map(|c| get(row, *c)).collect();
                    let other = match owner(class, path, &ks[..l]) {
                        Owner::Sibling(id) => format!("g:{id}"),
                        // an edge between two nodes of the set is counted once, outgoing
                        Owner::Inside if !outgoing => continue,
                        Owner::Inside => match new_level {
                            Some(level) => {
                                format!(
                                    "g:{}",
                                    child(class, path, level, key_of(level, ks[l])).encode()
                                )
                            }
                            // members among themselves are counted on their own
                            None => continue,
                        },
                    };
                    let p = iri_of(get(row, pc));
                    let n = count(get(row, nc));
                    if outgoing {
                        edges.add(me, other, p, n);
                    } else {
                        edges.add(other, me, p, n);
                    }
                }
            }
            // the other classes
            let q = format!(
                "SELECT {side} ?p ?d (COUNT(*) AS ?n) WHERE {{\n{head}{triple}\n?o <{RDF_TYPE}> ?d .\nFILTER(?d != <{class}> && ?p != <{RDF_TYPE}>)\n}} GROUP BY {side} ?p ?d LIMIT {EDGE_ROWS}"
            );
            let r = self.select(&q)?;
            if r.rows.len() >= EDGE_ROWS {
                edges.truncated = true;
            }
            let (sc, pc, dc, nc) = (r.col(&side[1..]), r.col("p"), r.col("d"), r.col("n"));
            for row in &r.rows {
                let (Some(me), Some(d)) = (name(get(row, sc)), iri_of(get(row, dc))) else {
                    continue;
                };
                let other = format!("c:{d}");
                let p = iri_of(get(row, pc));
                let n = count(get(row, nc));
                if outgoing {
                    edges.add(me, other, p, n);
                } else {
                    edges.add(other, me, p, n);
                }
            }
        }
        Ok(())
    }

    /// The answer of a partition, at most `limit` of its groups. `None` when a group's id
    /// would be longer than [`GroupId::decode`] accepts.
    fn groups_answer(
        &self,
        class: &str,
        path: &[Step],
        mut part: Partition,
        limit: usize,
    ) -> ApiResult<Option<J>> {
        let l = path.len();
        if part.children.len() > limit {
            part.children.truncate(limit);
            part.truncated = true;
        }
        let mut groups = Vec::with_capacity(part.children.len());
        for (i, c) in part.children.iter().enumerate() {
            let id = child(class, path, &part.level, c.key.clone());
            if id.encode().len() > MAX_ID {
                return Ok(None);
            }
            let top = if i < TOP_GROUPS {
                self.top(class, &id.path)?
            } else {
                Vec::new()
            };
            groups.push(json!({ "id": id.encode(), "label": c.label, "size": c.size, "top": top }));
        }
        let mut edges = Edges::default();
        let head = format!(
            "{}{}",
            set_pattern(class, path, "s"),
            key_expr(&part.level, "s", l, "?ks")
        );
        let level = part.level.clone();
        // an unbound `?ks` is the "other" group
        let name = |t: Option<&Term>| -> Option<String> {
            Some(format!(
                "g:{}",
                child(class, path, &level, key_of(&level, t)).encode()
            ))
        };
        self.outward_edges(
            &mut edges,
            class,
            path,
            &head,
            "?ks",
            Some(&part.level),
            &name,
        )?;
        let (edges, edges_truncated) = edges.json();
        Ok(Some(json!({
            "grouping": { "kind": part.kind, "predicate": partition_predicate(&part.level) },
            "communities": part.kind == "communities",
            "groups": groups,
            "edges": edges,
            "truncated": part.truncated || edges_truncated,
        })))
    }

    fn members_answer(&self, class: &str, path: &[Step], limit: usize, size: u64) -> ApiResult<J> {
        let q = format!(
            "SELECT ?s (SAMPLE(?l) AS ?label) WHERE {{ {{ SELECT DISTINCT ?s WHERE {{\n{}FILTER(isIRI(?s))\n}} LIMIT {} }} OPTIONAL {{ ?s {} ?l }} }} GROUP BY ?s",
            set_pattern(class, path, "s"),
            limit + 1,
            label_path()
        );
        let r = self.select(&q)?;
        let (sc, lc) = (r.col("s"), r.col("label"));
        let mut members: Vec<(String, Option<String>)> = r
            .rows
            .iter()
            .filter_map(|row| {
                let iri = iri_of(get(row, sc))?.to_string();
                let label = match get(row, lc) {
                    Some(Term::Literal(l)) => Some(l.value().to_string()),
                    _ => None,
                };
                Some((iri, label))
            })
            .collect();
        members.sort();
        let mut truncated = members.len() > limit || size > limit as u64;
        members.truncate(limit);
        let set: HashSet<&str> = members.iter().map(|(i, _)| i.as_str()).collect();
        let mut edges = Edges::default();
        if !members.is_empty() {
            let values = format!(
                "VALUES ?s {{ {} }}\n",
                members
                    .iter()
                    .map(|(i, _)| format!("<{i}>"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            // the members among themselves
            let q = format!(
                "SELECT ?s ?p ?o WHERE {{\n{values}?s ?p ?o .\nFILTER(isIRI(?o) && ?p != <{RDF_TYPE}>)\n{}}} LIMIT {}",
                set_pattern(class, path, "o"),
                MAX_EDGES + 1
            );
            let r2 = self.select(&q)?;
            if r2.rows.len() > MAX_EDGES {
                truncated = true;
            }
            let (s, p, o) = (r2.col("s"), r2.col("p"), r2.col("o"));
            for row in &r2.rows {
                let (Some(a), Some(b)) = (iri_of(get(row, s)), iri_of(get(row, o))) else {
                    continue;
                };
                if set.contains(a) && set.contains(b) {
                    edges.add(format!("m:{a}"), format!("m:{b}"), iri_of(get(row, p)), 1);
                }
            }
            let name = |t: Option<&Term>| -> Option<String> { iri_of(t).map(|i| format!("m:{i}")) };
            self.outward_edges(&mut edges, class, path, &values, "?s", None, &name)?;
        }
        let (edges, edges_truncated) = edges.json();
        let members: Vec<J> = members
            .into_iter()
            .map(|(iri, label)| {
                let mut m = json!({ "iri": iri, "class": class });
                if let Some(l) = label {
                    m["label"] = l.into();
                }
                m
            })
            .collect();
        Ok(json!({
            "grouping": { "kind": "members", "predicate": null },
            "communities": false,
            "groups": [],
            "members": members,
            "edges": edges,
            "truncated": truncated || edges_truncated,
        }))
    }
}

fn partition_predicate(level: &Level) -> &str {
    match level {
        Level::Values { p, .. } | Level::Range { p, .. } | Level::Community { p, .. } => p,
    }
}

/// The smallest round width (1, 2, 2.5 or 5 times a power of ten) of at least `raw`, and
/// a whole number of at least 1 when the values are integers.
fn nice_width(raw: f64, integral: bool) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return 1.0;
    }
    let mag = 10f64.powf(raw.log10().floor());
    let w = [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|w| *w >= raw * (1.0 - 1e-9))
        .unwrap_or(10.0 * mag);
    if integral { w.ceil().max(1.0) } else { w }
}

#[cfg(test)]
mod tests {
    use super::nice_width;

    #[test]
    fn range_widths_are_round() {
        assert_eq!(nice_width(14166.7, false), 20000.0);
        assert_eq!(nice_width(0.37, false), 0.5);
        assert_eq!(nice_width(2.1, false), 2.5);
        assert_eq!(nice_width(2.1, true), 3.0);
        assert_eq!(nice_width(10.0, true), 10.0);
        assert_eq!(nice_width(0.2, true), 1.0);
        assert_eq!(nice_width(f64::NAN, false), 1.0);
    }
}
