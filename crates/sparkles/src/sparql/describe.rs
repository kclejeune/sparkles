//! DESCRIBE: the description of each resource a query names or binds.
//!
//! The description is read from the query's dataset, as Jena's default handler
//! (`DescribeBNodeClosure`) reads it: from the default graph, and from each named graph
//! of the dataset on its own. Blank nodes are followed inside the graph they were found
//! in, so a closure never crosses from one named graph into another. The triples of all
//! the graphs are merged into one result graph.
//!
//! [`DescribeMode`] picks what a description holds:
//! * `cbd`, the default, is the concise bounded description of the W3C member submission
//!   (<https://www.w3.org/submissions/CBD/>): the resource's triples, the triples of the
//!   blank nodes they lead to, recursively, and the description of every reifier of an
//!   included triple. On data without reifiers it equals Jena's answer.
//! * `scbd` is the submission's symmetric description: the CBD, plus the triples whose
//!   object is the resource, followed backwards through blank-node subjects.
//! * `outgoing` is the resource's own triples and nothing else.
//!
//! Reifiers are RDF 1.2's (`?r rdf:reifies <<( s p o )>>`) and RDF 1.1's reification
//! (`?r rdf:subject s ; rdf:predicate p ; rdf:object o`). Options add the labels of the
//! IRIs the description links to, and bound its depth and its size.

use super::ctx::{Ctx, PlanWarning, TermKind};
use super::plan::GraphFilter;
use super::table::Table;
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::store::Chunk;
use oxrdf::{Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const SKOS_PREF_LABEL: &str = "http://www.w3.org/2004/02/skos/core#prefLabel";
/// The graph of materialized inferences, which reaches a description through the
/// default graph when reasoning is on, and is never read as a named graph.
const INFERRED_GRAPH: &str = crate::access::triples::INFERRED_GRAPH;

/// What a description of a resource holds (see the module documentation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DescribeMode {
    /// the concise bounded description
    #[default]
    Cbd,
    /// the symmetric concise bounded description
    Scbd,
    /// the resource's own triples
    Outgoing,
}

impl DescribeMode {
    pub const ALL: [DescribeMode; 3] = [
        DescribeMode::Cbd,
        DescribeMode::Scbd,
        DescribeMode::Outgoing,
    ];

    pub fn name(self) -> &'static str {
        match self {
            DescribeMode::Cbd => "cbd",
            DescribeMode::Scbd => "scbd",
            DescribeMode::Outgoing => "outgoing",
        }
    }

    /// A mode by its name, in any case.
    pub fn parse(s: &str) -> Result<DescribeMode> {
        Self::ALL
            .into_iter()
            .find(|m| m.name().eq_ignore_ascii_case(s.trim()))
            .ok_or_else(|| {
                Error::invalid(format!(
                    "unknown DESCRIBE mode {s:?}: expected cbd, scbd or outgoing"
                ))
            })
    }
}

/// How DESCRIBE describes a resource: a dataset's setting, or a request's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct DescribeOptions {
    pub mode: DescribeMode,
    /// Add the `rdfs:label` and `skos:prefLabel` triples of the IRIs in the description.
    pub labels: bool,
    /// Include the descriptions of the reifiers of included triples (`cbd` and `scbd`).
    pub reifiers: bool,
    /// The most triples one DESCRIBE returns (`None`: no limit). A description that
    /// reaches it stops there and warns `describe-truncated`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_triples: Option<u64>,
    /// The most levels a description follows (`None`: no limit). The resource's own
    /// triples are level 1, and each blank node or reifier followed adds a level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
}

impl Default for DescribeOptions {
    fn default() -> Self {
        DescribeOptions {
            mode: DescribeMode::Cbd,
            labels: false,
            reifiers: true,
            max_triples: None,
            max_depth: None,
        }
    }
}

/// The names [`DescribeOptions::set`] takes.
pub const DESCRIBE_KEYS: [&str; 5] = ["mode", "labels", "reifiers", "maxTriples", "maxDepth"];

impl DescribeOptions {
    /// Whether these are the built-in defaults.
    pub fn is_default(&self) -> bool {
        *self == DescribeOptions::default()
    }

    /// Set one option by its JSON name (see [`DESCRIBE_KEYS`]). A limit of `0` or `none`
    /// removes the limit.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let v = value.trim();
        let flag = || match v.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(true),
            "false" | "0" | "no" | "off" => Ok(false),
            _ => Err(Error::invalid(format!(
                "{key}: expected true or false, not {v:?}"
            ))),
        };
        let limit = || -> Result<Option<u64>> {
            if v.eq_ignore_ascii_case("none") {
                return Ok(None);
            }
            let n: u64 = v.parse().map_err(|_| {
                Error::invalid(format!("{key}: expected a whole number, not {v:?}"))
            })?;
            Ok((n > 0).then_some(n))
        };
        match key {
            "mode" => self.mode = DescribeMode::parse(v)?,
            "labels" => self.labels = flag()?,
            "reifiers" => self.reifiers = flag()?,
            "maxTriples" | "max-triples" => self.max_triples = limit()?,
            "maxDepth" | "max-depth" => {
                self.max_depth = match limit()? {
                    Some(n) => Some(
                        u32::try_from(n)
                            .map_err(|_| Error::invalid(format!("{key}: at most {}", u32::MAX)))?,
                    ),
                    None => None,
                }
            }
            _ => {
                return Err(Error::invalid(format!(
                    "unknown DESCRIBE option {key:?}: expected one of {}",
                    DESCRIBE_KEYS.join(", ")
                )));
            }
        }
        Ok(())
    }

    /// Options from a JSON object with the names of [`DESCRIBE_KEYS`]. Missing names keep
    /// their defaults, and a limit of `0` or `null` is no limit.
    pub fn from_json(v: &serde_json::Value) -> Result<DescribeOptions> {
        let Some(obj) = v.as_object() else {
            return Err(Error::invalid("DESCRIBE options must be a JSON object"));
        };
        let mut o = DescribeOptions::default();
        for (k, val) in obj {
            if k == "format" {
                continue;
            }
            let s = match val {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Null if k.starts_with("max") => "0".into(),
                _ => {
                    return Err(Error::invalid(format!(
                        "DESCRIBE option {k}: unexpected value {val}"
                    )));
                }
            };
            o.set(k, &s)?;
        }
        Ok(o)
    }

    /// These options with a request's limits, which may only lower them.
    pub fn lowered(mut self, max_triples: Option<u64>, max_depth: Option<u32>) -> Self {
        fn low<T: Ord + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        self.max_triples = low(self.max_triples, max_triples);
        self.max_depth = low(self.max_depth, max_depth);
        self
    }
}

/// The triples of a DESCRIBE, and whether [`DescribeOptions::max_triples`] cut them short.
pub(super) struct Described {
    pub triples: Vec<Triple>,
    pub truncated: bool,
}

/// A graph a description reads: the query's default graph, or one named graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Source {
    Default,
    Named(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Dir {
    /// triples whose subject is the node
    Out,
    /// triples whose object is the node
    In,
}

/// A node to describe: in `src` (`None` for a resource of the query, which is described
/// in every graph), whose triples are at `level`.
struct Item {
    node: Id,
    src: Option<Source>,
    dir: Dir,
    level: u32,
}

struct Describer<'a> {
    ctx: &'a Ctx,
    opts: &'a DescribeOptions,
    /// the default graph's filter; `None` when the default graph is empty
    default: Option<GraphFilter>,
    /// the named graphs' filter; `None` when the dataset has none
    named: Option<GraphFilter>,
    inferred: Option<u64>,
    reifies: Option<Id>,
    /// RDF 1.1 reification: `rdf:subject`, `rdf:predicate`, `rdf:object`
    statement: Option<[Id; 3]>,
    out: Vec<Triple>,
    seen: FxHashSet<(Id, Id, Id)>,
    /// the lowest level each node was described at, per graph and direction
    visited: FxHashMap<(Id, Option<Source>, Dir), u32>,
    /// the reifiers of a triple, with their graphs
    reifiers: FxHashMap<(Id, Id, Id), Vec<(Id, u64)>>,
    /// IRIs of the description, for their labels
    linked: FxHashSet<(Id, Source)>,
    queue: Vec<Item>,
    truncated: bool,
}

/// Describe the IRIs and blank nodes of the solutions `t`. `explicit` says whether the
/// query or the protocol gave a dataset (or the inference overlay changed the default
/// graph). Without one, the default graph is the store's default graph, as in Jena,
/// even when the store's queries see the union of its named graphs.
pub(super) fn describe(
    ctx: &Ctx,
    t: &Table,
    opts: &DescribeOptions,
    explicit: bool,
) -> Result<Described> {
    let mut resources: Vec<Id> = Vec::new();
    let mut seen_r = FxHashSet::default();
    for col in &t.cols {
        for id in col {
            if !id.is_undef()
                && id.tag() != Tag::Local
                && matches!(ctx.kind(*id), TermKind::Iri | TermKind::BNode)
                && seen_r.insert(*id)
            {
                resources.push(*id);
            }
        }
    }
    let ds = &ctx.dataset;
    let set_of = |set: &[Id]| {
        let mut v: Vec<u64> = set.iter().map(|g| g.0).collect();
        v.sort_unstable();
        v.dedup();
        GraphFilter::Set(v)
    };
    let default = if explicit {
        match &ds.default {
            Some(set) if set.is_empty() => None,
            Some(set) => Some(set_of(set)),
            None if ds.union_default => Some(GraphFilter::Named),
            None => Some(GraphFilter::Default),
        }
    } else if ctx.graphs.as_ref().is_none_or(|a| a.read.default_graph()) {
        Some(GraphFilter::Default)
    } else {
        None
    };
    let named = match &ds.named {
        None => Some(GraphFilter::Named),
        Some(set) if set.is_empty() => None,
        Some(set) => Some(set_of(set)),
    };
    let snap = &ctx.snap;
    let iri = |s: &str| snap.lookup_iri(s);
    let used = |p: Option<Id>| -> Result<Option<Id>> {
        let Some(p) = p else { return Ok(None) };
        let mut any = false;
        snap.scan(Perm::Pso, &[p.0], |_| {
            any = true;
            Ok(false)
        })?;
        Ok(any.then_some(p))
    };
    let follow = opts.reifiers && opts.mode != DescribeMode::Outgoing;
    let (reifies, statement) = if follow {
        let reifies = used(iri(&format!("{RDF}reifies")))?;
        let statement = match (
            used(iri(&format!("{RDF}subject")))?,
            iri(&format!("{RDF}predicate")),
            iri(&format!("{RDF}object")),
        ) {
            (Some(s), Some(p), Some(o)) => Some([s, p, o]),
            _ => None,
        };
        (reifies, statement)
    } else {
        (None, None)
    };
    let mut d = Describer {
        ctx,
        opts,
        default,
        named,
        inferred: iri(INFERRED_GRAPH).map(|g| g.0),
        reifies,
        statement,
        out: Vec::new(),
        seen: FxHashSet::default(),
        visited: FxHashMap::default(),
        reifiers: FxHashMap::default(),
        linked: FxHashSet::default(),
        queue: Vec::new(),
        truncated: false,
    };
    for r in resources {
        if d.truncated {
            break;
        }
        d.push(r, None, Dir::Out, 1);
        if opts.mode == DescribeMode::Scbd {
            d.push(r, None, Dir::In, 1);
        }
        d.run()?;
    }
    if opts.labels && !d.truncated {
        d.labels()?;
    }
    if d.truncated {
        ctx.warn(PlanWarning {
            code: "describe-truncated",
            message: format!(
                "the DESCRIBE result stopped at its limit of {} triples",
                opts.max_triples.unwrap_or_default()
            ),
        });
    }
    Ok(Described {
        triples: d.out,
        truncated: d.truncated,
    })
}

impl Describer<'_> {
    fn push(&mut self, node: Id, src: Option<Source>, dir: Dir, level: u32) {
        if self.opts.max_depth.is_some_and(|m| level > m) {
            return;
        }
        self.queue.push(Item {
            node,
            src,
            dir,
            level,
        });
    }

    /// The graphs `g` is part of: the default graph, a named graph, or both.
    fn sources(&self, g: u64) -> impl Iterator<Item = Source> {
        let d = self
            .default
            .as_ref()
            .is_some_and(|f| f.accepts(g))
            .then_some(Source::Default);
        let n = (g != Id::DEFAULT_GRAPH.0
            && Some(g) != self.inferred
            && self.named.as_ref().is_some_and(|f| f.accepts(g)))
        .then_some(Source::Named(g));
        d.into_iter().chain(n)
    }

    fn accepts(&self, src: Source, g: u64) -> bool {
        self.sources(g).any(|s| s == src)
    }

    /// The quads whose permuted key starts with `prefix`, as `[s, p, o, g]`.
    fn quads(&self, perm: Perm, prefix: &[u64]) -> Result<Vec<[Id; 4]>> {
        let mut keys = Vec::new();
        self.ctx.snap.scan(perm, prefix, |c| {
            match c {
                Chunk::Block(b, s, e) => keys.extend((s..e).map(|i| b.key(i))),
                Chunk::Row(k) => keys.push(k),
            }
            Ok(true)
        })?;
        Ok(keys.iter().map(|k| perm.to_quad(k)).collect())
    }

    fn run(&mut self) -> Result<()> {
        while let Some(it) = self.queue.pop() {
            if self.truncated {
                self.queue.clear();
                break;
            }
            self.ctx.check()?;
            self.ctx.check_rows(self.out.len())?;
            let key = (it.node, it.src, it.dir);
            if self.visited.get(&key).is_some_and(|&l| l <= it.level) {
                continue;
            }
            self.visited.insert(key, it.level);
            let perm = match it.dir {
                Dir::Out => Perm::Spo,
                Dir::In => Perm::Osp,
            };
            for [s, p, o, g] in self.quads(perm, &[it.node.0])? {
                let srcs: Vec<Source> = match it.src {
                    Some(src) => self.accepts(src, g.0).then_some(src).into_iter().collect(),
                    None => self.sources(g.0).collect(),
                };
                for src in srcs {
                    self.emit(s, p, o, src)?;
                    if self.truncated {
                        return Ok(());
                    }
                    let next = it.level + 1;
                    match it.dir {
                        Dir::Out if self.opts.mode != DescribeMode::Outgoing => {
                            if o.tag() == Tag::BNode {
                                self.push(o, Some(src), Dir::Out, next);
                            }
                        }
                        Dir::In if s.tag() == Tag::BNode && s != it.node => {
                            self.push(s, Some(src), Dir::In, next);
                        }
                        _ => {}
                    }
                    if self.reifies.is_some() || self.statement.is_some() {
                        for r in self.reifiers_of(s, p, o, src)? {
                            self.push(r, Some(src), Dir::Out, next);
                            if self.opts.mode == DescribeMode::Scbd {
                                self.push(r, Some(src), Dir::In, next);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Add a triple to the result, unless it is there already.
    fn emit(&mut self, s: Id, p: Id, o: Id, src: Source) -> Result<()> {
        if self.opts.labels {
            for x in [s, o] {
                if self.ctx.kind(x) == TermKind::Iri {
                    self.linked.insert((x, src));
                }
            }
        }
        if !self.seen.insert((s, p, o)) {
            return Ok(());
        }
        if self
            .opts
            .max_triples
            .is_some_and(|m| self.out.len() as u64 >= m)
        {
            self.truncated = true;
            return Ok(());
        }
        if let Some(q) = self.ctx.snap.quad_to_terms(&[s, p, o, Id::DEFAULT_GRAPH]) {
            self.out.push(Triple::new(q.subject, q.predicate, q.object));
        }
        Ok(())
    }

    /// The reifiers of the triple `s p o` in `src`: RDF 1.2's `?r rdf:reifies <<( s p o )>>`
    /// and RDF 1.1's `?r rdf:subject s ; rdf:predicate p ; rdf:object o`.
    fn reifiers_of(&mut self, s: Id, p: Id, o: Id, src: Source) -> Result<Vec<Id>> {
        if !self.reifiers.contains_key(&(s, p, o)) {
            let mut found: Vec<(Id, u64)> = Vec::new();
            if let Some(reifies) = self.reifies
                && let Some(q) = self.ctx.snap.quad_to_terms(&[s, p, o, Id::DEFAULT_GRAPH])
            {
                let tt = Term::Triple(Box::new(Triple::new(q.subject, q.predicate, q.object)));
                if let Some(tt) = self.ctx.snap.lookup_term(&tt) {
                    for [r, _, _, g] in self.quads(Perm::Pos, &[reifies.0, tt.0])? {
                        found.push((r, g.0));
                    }
                }
            }
            if let Some([rs, rp, ro]) = self.statement {
                for [r, _, _, g] in self.quads(Perm::Pos, &[rs.0, s.0])? {
                    let has = |pred: Id, obj: Id| -> Result<bool> {
                        Ok(self
                            .quads(Perm::Spo, &[r.0, pred.0, obj.0])?
                            .iter()
                            .any(|q| q[3] == g))
                    };
                    if has(rp, p)? && has(ro, o)? {
                        found.push((r, g.0));
                    }
                }
            }
            self.reifiers.insert((s, p, o), found);
        }
        Ok(self.reifiers[&(s, p, o)]
            .iter()
            .filter(|(_, g)| self.accepts(src, *g))
            .map(|(r, _)| *r)
            .collect())
    }

    /// The labels of the IRIs of the description, from the graphs they were found in.
    fn labels(&mut self) -> Result<()> {
        let preds: Vec<Id> = [RDFS_LABEL, SKOS_PREF_LABEL]
            .iter()
            .filter_map(|p| self.ctx.snap.lookup_iri(p))
            .collect();
        let mut linked: Vec<(Id, Source)> = self.linked.iter().copied().collect();
        linked.sort_unstable_by_key(|(x, s)| {
            (
                x.0,
                match s {
                    Source::Default => 0,
                    Source::Named(g) => g.saturating_add(1),
                },
            )
        });
        for (x, src) in linked {
            self.ctx.check()?;
            for &lp in &preds {
                for [s, p, o, g] in self.quads(Perm::Spo, &[x.0, lp.0])? {
                    if self.accepts(src, g.0) {
                        self.emit(s, p, o, src)?;
                        if self.truncated {
                            return Ok(());
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
