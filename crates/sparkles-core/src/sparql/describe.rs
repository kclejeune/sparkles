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

use super::ctx::{Charge, Ctx, OutputCredit, PlanWarning, RetainedCharge, TermKind};
use super::plan::GraphFilter;
use super::table::Table;
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::store::Chunk;
use oxrdf::{Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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

/// Traversal estimates borrow an eager context, while cursor state keeps its existing
/// weak owner. Both paths check the same lexical and container estimates.
struct DescriptionCharge<'a> {
    inner: DescriptionChargeKind<'a>,
    bytes: u64,
}

enum DescriptionChargeKind<'a> {
    Eager(Charge<'a>),
    Cursor(RetainedCharge),
}

impl<'a> DescriptionCharge<'a> {
    fn new(ctx: &'a Ctx, bytes: u64) -> Result<Self> {
        let inner = match ctx.retained_charge(bytes)? {
            Some(charge) => DescriptionChargeKind::Cursor(charge),
            None => DescriptionChargeKind::Eager(ctx.charge(bytes)?),
        };
        Ok(Self { inner, bytes })
    }

    fn resize(&mut self, bytes: u64) -> Result<()> {
        debug_assert!(bytes >= self.bytes, "DESCRIBE estimates only grow");
        match &mut self.inner {
            DescriptionChargeKind::Eager(charge) => charge.add(bytes - self.bytes)?,
            DescriptionChargeKind::Cursor(charge) => charge.resize(bytes)?,
        }
        self.bytes = bytes;
        Ok(())
    }
}

/// The triples of a DESCRIBE, and whether [`DescribeOptions::max_triples`] cut them short.
pub(super) struct Described<'a> {
    triples: Vec<Triple>,
    pub truncated: bool,
    _charge: Option<DescriptionCharge<'a>>,
}

impl Described<'_> {
    /// Reconcile only downward while the physical graph still has its original
    /// admission. No allocation or reserve is permitted at this boundary.
    pub(super) fn into_output(mut self, ctx: &Arc<Ctx>) -> Result<(Vec<Triple>, OutputCredit)> {
        ctx.check()?;
        let charge = self._charge.as_mut().expect("live description credit");
        let same = match &charge.inner {
            DescriptionChargeKind::Eager(held) => held.belongs_to(ctx),
            DescriptionChargeKind::Cursor(held) => {
                held.owner().is_some_and(|owner| Arc::ptr_eq(&owner, ctx))
            }
        };
        if !same {
            return Err(Error::invalid(
                "description belongs to another query account",
            ));
        }
        let mut bytes = (self.triples.capacity() as u64)
            .checked_mul(std::mem::size_of::<Triple>() as u64)
            .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
        for triple in &self.triples {
            ctx.check()?;
            // The outer Triple slots are already included above. This envelope
            // conservatively includes each owned lexical allocation and nested box.
            let subject = match &triple.subject {
                oxrdf::NamedOrBlankNode::NamedNode(n) => n.as_str().len(),
                oxrdf::NamedOrBlankNode::BlankNode(b) => b.as_str().len(),
            };
            let payload = super::output::term_bytes(&triple.object)
                .and_then(|n| n.checked_add(subject as u64))
                .and_then(|n| n.checked_add(triple.predicate.as_str().len() as u64))
                .and_then(|n| n.checked_add(256))
                .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
            bytes = bytes
                .checked_add(payload)
                .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
        }
        if bytes > charge.bytes {
            return Err(Error::invalid("description exceeds its original admission"));
        }
        match &mut charge.inner {
            DescriptionChargeKind::Eager(held) => held.resize(bytes)?,
            DescriptionChargeKind::Cursor(held) => held.resize(bytes)?,
        }
        charge.bytes = bytes;
        ctx.check()?;
        let charge = self._charge.take().expect("validated description credit");
        let credit = match charge.inner {
            DescriptionChargeKind::Eager(held) => held.into_output(ctx),
            DescriptionChargeKind::Cursor(held) => held.into_output(ctx),
        };
        Ok((std::mem::take(&mut self.triples), credit))
    }
}
impl Drop for Described<'_> {
    fn drop(&mut self) {
        let depth = self
            .triples
            .iter()
            .map(|t| super::output::term_depth(&t.object))
            .max()
            .unwrap_or(0);
        super::depth::with_stack(depth, || self.triples.clear());
    }
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
    charge: DescriptionCharge<'a>,
    bytes: u64,
    /// the triples earlier batches of a streamed DESCRIBE returned
    emitted: u64,
    /// The bytes of `bytes` that the output and the queue hold, which a streamed
    /// DESCRIBE releases once it hands its batch on, keeping only the traversal state.
    transient: u64,
}

/// Where a description reads: the graphs of the dataset and the reification
/// predicates the store uses.
#[derive(Clone)]
struct Setup {
    default: Option<GraphFilter>,
    named: Option<GraphFilter>,
    inferred: Option<u64>,
    reifies: Option<Id>,
    statement: Option<[Id; 3]>,
}

fn setup(ctx: &Ctx, opts: &DescribeOptions, explicit: bool) -> Result<Setup> {
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
    Ok(Setup {
        default,
        named,
        inferred: iri(INFERRED_GRAPH).map(|g| g.0),
        reifies,
        statement,
    })
}

/// The IRIs and blank nodes of the solutions `t`, column by column, that `seen` does
/// not hold yet, which it then holds.
fn resources(ctx: &Ctx, t: &Table, seen: &mut FxHashSet<Id>) -> Vec<Id> {
    let mut resources: Vec<Id> = Vec::new();
    for col in &t.cols {
        for id in col {
            if !id.is_undef()
                && id.tag() != Tag::Local
                && matches!(ctx.kind(*id), TermKind::Iri | TermKind::BNode)
                && seen.insert(*id)
            {
                resources.push(*id);
            }
        }
    }
    resources
}

impl Describer<'_> {
    /// Describe `resources` in turn, and add the labels when `last` says no resources
    /// follow.
    fn describe_all(&mut self, resources: Vec<Id>, last: bool) -> Result<()> {
        for r in resources {
            if self.truncated {
                break;
            }
            self.push(r, None, Dir::Out, 1)?;
            if self.opts.mode == DescribeMode::Scbd {
                self.push(r, None, Dir::In, 1)?;
            }
            self.run()?;
        }
        if last && self.opts.labels && !self.truncated {
            self.labels()?;
        }
        Ok(())
    }

    fn warn_truncated(&self) {
        self.ctx.warn(PlanWarning {
            code: "describe-truncated",
            message: format!(
                "the DESCRIBE result stopped at its limit of {} triples",
                self.opts.max_triples.unwrap_or_default()
            ),
        });
    }
}

/// What a streamed DESCRIBE keeps from one batch of solutions to the next: the
/// resources it described, the triples it returned, the nodes it visited, the reifiers
/// it found and the IRIs whose labels it adds at the end. They grow with the
/// description, as the eager traversal's state does, under one retained charge.
#[derive(Default)]
pub(super) struct Streamed {
    setup: Option<Setup>,
    described: FxHashSet<Id>,
    seen: FxHashSet<(Id, Id, Id)>,
    visited: FxHashMap<(Id, Option<Source>, Dir), u32>,
    reifiers: FxHashMap<(Id, Id, Id), Vec<(Id, u64)>>,
    linked: FxHashSet<(Id, Source)>,
    truncated: bool,
    emitted: u64,
    charge: Option<RetainedCharge>,
    bytes: u64,
}

impl Streamed {
    pub(super) fn truncated(&self) -> bool {
        self.truncated
    }
}

/// The triples that describe the resources of the solutions `t` that no earlier batch
/// named, after those that earlier batches returned. `last` says that no batch follows,
/// so the labels of the linked IRIs come last. Only a cursor streams a DESCRIBE, and the
/// state it keeps holds a retained charge.
pub(super) fn describe_batch(
    ctx: &Ctx,
    t: &Table,
    opts: &DescribeOptions,
    explicit: bool,
    state: &mut Streamed,
    last: bool,
) -> Result<Vec<Triple>> {
    if state.truncated {
        return Ok(Vec::new());
    }
    let setup = match &state.setup {
        Some(setup) => setup.clone(),
        None => {
            let setup = setup(ctx, opts, explicit)?;
            state.setup = Some(setup.clone());
            setup
        }
    };
    let before = state.described.len();
    let resources = resources(ctx, t, &mut state.described);
    let charge = match state.charge.take() {
        Some(charge) => charge,
        None => {
            state.bytes = 4096;
            ctx.retained_charge(state.bytes)?
                .ok_or_else(|| Error::invalid("a streamed DESCRIBE runs in a cursor"))?
        }
    };
    let mut d = Describer {
        ctx,
        opts,
        default: setup.default,
        named: setup.named,
        inferred: setup.inferred,
        reifies: setup.reifies,
        statement: setup.statement,
        out: Vec::new(),
        seen: std::mem::take(&mut state.seen),
        visited: std::mem::take(&mut state.visited),
        reifiers: std::mem::take(&mut state.reifiers),
        linked: std::mem::take(&mut state.linked),
        queue: Vec::new(),
        truncated: false,
        charge: DescriptionCharge {
            inner: DescriptionChargeKind::Cursor(charge),
            bytes: state.bytes,
        },
        bytes: state.bytes,
        emitted: state.emitted,
        transient: 0,
    };
    // the resources this batch added, in the set of described resources
    let added = (state.described.len() - before) as u64 * 48;
    let result = d
        .reserve(added)
        .and_then(|()| d.describe_all(resources, last));
    if d.truncated {
        d.warn_truncated();
    }
    state.seen = std::mem::take(&mut d.seen);
    state.visited = std::mem::take(&mut d.visited);
    state.reifiers = std::mem::take(&mut d.reifiers);
    state.linked = std::mem::take(&mut d.linked);
    state.truncated = d.truncated;
    state.emitted = d.emitted.saturating_add(d.out.len() as u64);
    // The triples go to the caller, which charges them itself, and the queue is empty.
    state.bytes = d.bytes.saturating_sub(d.transient).max(4096);
    let DescriptionCharge { inner, .. } = d.charge;
    if let DescriptionChargeKind::Cursor(mut charge) = inner {
        let _ = charge.resize(state.bytes);
        state.charge = Some(charge);
    }
    result?;
    Ok(d.out)
}

/// Describe the IRIs and blank nodes of the solutions `t`. `explicit` says whether the
/// query or the protocol gave a dataset (or the inference overlay changed the default
/// graph). Without one, the default graph is the store's default graph, as in Jena,
/// even when the store's queries see the union of its named graphs.
pub(super) fn describe<'a>(
    ctx: &'a Ctx,
    t: &Table,
    opts: &'a DescribeOptions,
    explicit: bool,
) -> Result<Described<'a>> {
    let _resource_charge =
        DescriptionCharge::new(ctx, t.mem_bytes().saturating_mul(8).saturating_add(4096))?;
    let resources = resources(ctx, t, &mut FxHashSet::default());
    let setup = setup(ctx, opts, explicit)?;
    let mut d = Describer {
        ctx,
        opts,
        default: setup.default,
        named: setup.named,
        inferred: setup.inferred,
        reifies: setup.reifies,
        statement: setup.statement,
        out: Vec::new(),
        seen: FxHashSet::default(),
        visited: FxHashMap::default(),
        reifiers: FxHashMap::default(),
        linked: FxHashSet::default(),
        queue: Vec::new(),
        truncated: false,
        charge: DescriptionCharge::new(ctx, 4096)?,
        bytes: 4096,
        emitted: 0,
        transient: 0,
    };
    d.describe_all(resources, true)?;
    if d.truncated {
        d.warn_truncated();
    }
    Ok(Described {
        triples: d.out,
        truncated: d.truncated,
        _charge: Some(d.charge),
    })
}

struct Quads<'a> {
    values: Vec<[Id; 4]>,
    _charge: DescriptionCharge<'a>,
}
impl std::ops::Deref for Quads<'_> {
    type Target = [[Id; 4]];
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}
struct QuadIter<'a> {
    values: std::vec::IntoIter<[Id; 4]>,
    _charge: DescriptionCharge<'a>,
}
impl Iterator for QuadIter<'_> {
    type Item = [Id; 4];
    fn next(&mut self) -> Option<Self::Item> {
        self.values.next()
    }
}
impl<'a> IntoIterator for Quads<'a> {
    type Item = [Id; 4];
    type IntoIter = QuadIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        QuadIter {
            values: self.values.into_iter(),
            _charge: self._charge,
        }
    }
}

impl<'a> Describer<'a> {
    fn reserve(&mut self, bytes: u64) -> Result<()> {
        self.bytes = self.bytes.saturating_add(bytes);
        self.charge.resize(self.bytes)
    }

    fn push(&mut self, node: Id, src: Option<Source>, dir: Dir, level: u32) -> Result<()> {
        if self.opts.max_depth.is_some_and(|m| level > m) {
            return Ok(());
        }
        if self.queue.len() == self.queue.capacity() {
            let bytes = (self.queue.capacity().max(4) as u64).saturating_mul(128);
            self.reserve(bytes)?;
            self.transient = self.transient.saturating_add(bytes);
        }
        self.queue.push(Item {
            node,
            src,
            dir,
            level,
        });
        Ok(())
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

    /// The quads whose permuted key starts with `prefix`, as `[s, p, o, g]`. The `rows`
    /// budget, the deadline and cancellation stop a scan of a hub's many quads.
    fn quads(&self, perm: Perm, prefix: &[u64]) -> Result<Quads<'a>> {
        let mut keys = Vec::new();
        let mut charge = DescriptionCharge::new(self.ctx, 1024)?;
        self.ctx.snap.scan(perm, prefix, |c| {
            let added = match &c {
                Chunk::Block(_, s, e) => e - s,
                Chunk::Row(_) => 1,
            };
            charge.resize(
                (keys.len().saturating_add(added) as u64)
                    .saturating_mul(128)
                    .saturating_add(1024),
            )?;
            match c {
                Chunk::Block(b, s, e) => keys.extend((s..e).map(|i| b.key(i))),
                Chunk::Row(k) => keys.push(k),
            }
            self.ctx.check()?;
            self.ctx.check_rows(keys.len())?;
            Ok(true)
        })?;
        Ok(Quads {
            values: keys.iter().map(|k| perm.to_quad(k)).collect(),
            _charge: charge,
        })
    }

    fn run(&mut self) -> Result<()> {
        while let Some(it) = self.queue.pop() {
            if self.truncated {
                self.queue.clear();
                break;
            }
            self.ctx.check()?;
            self.ctx
                .check_rows((self.emitted as usize).saturating_add(self.out.len()))?;
            let key = (it.node, it.src, it.dir);
            if self.visited.get(&key).is_some_and(|&l| l <= it.level) {
                continue;
            }
            if !self.visited.contains_key(&key) {
                self.reserve(256)?;
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
                                self.push(o, Some(src), Dir::Out, next)?;
                            }
                        }
                        Dir::In if s.tag() == Tag::BNode && s != it.node => {
                            self.push(s, Some(src), Dir::In, next)?;
                        }
                        _ => {}
                    }
                    if self.reifies.is_some() || self.statement.is_some() {
                        for r in self.reifiers_of(s, p, o, src)? {
                            self.push(r, Some(src), Dir::Out, next)?;
                            if self.opts.mode == DescribeMode::Scbd {
                                self.push(r, Some(src), Dir::In, next)?;
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
                    if !self.linked.contains(&(x, src)) {
                        self.reserve(128)?;
                    }
                    self.linked.insert((x, src));
                }
            }
        }
        if self.seen.contains(&(s, p, o)) {
            return Ok(());
        }
        self.reserve(128)?;
        self.seen.insert((s, p, o));
        if self
            .opts
            .max_triples
            .is_some_and(|m| self.emitted.saturating_add(self.out.len() as u64) >= m)
        {
            self.truncated = true;
            return Ok(());
        }
        let bytes = [s, p, o].iter().try_fold(512u64, |bytes, id| {
            Ok::<_, Error>(bytes.saturating_add(self.ctx.decoded_bytes(*id)?.saturating_mul(4)))
        })?;
        self.reserve(bytes)?;
        self.transient = self.transient.saturating_add(bytes);
        if let Some(q) = self.ctx.snap.quad_to_terms(&[s, p, o, Id::DEFAULT_GRAPH]) {
            self.out.push(Triple::new(q.subject, q.predicate, q.object));
        }
        Ok(())
    }

    /// The reifiers of the triple `s p o` in `src`: RDF 1.2's `?r rdf:reifies <<( s p o )>>`
    /// and RDF 1.1's `?r rdf:subject s ; rdf:predicate p ; rdf:object o`.
    fn reifiers_of(&mut self, s: Id, p: Id, o: Id, src: Source) -> Result<Vec<Id>> {
        if !self.reifiers.contains_key(&(s, p, o)) {
            let bytes = [s, p, o].iter().try_fold(1024u64, |bytes, id| {
                Ok::<_, Error>(bytes.saturating_add(self.ctx.decoded_bytes(*id)?.saturating_mul(8)))
            })?;
            let _decode_charge = DescriptionCharge::new(self.ctx, bytes)?;
            let mut found: Vec<(Id, u64)> = Vec::new();
            if let Some(reifies) = self.reifies
                && let Some(q) = self.ctx.snap.quad_to_terms(&[s, p, o, Id::DEFAULT_GRAPH])
            {
                let tt = Term::Triple(Box::new(Triple::new(q.subject, q.predicate, q.object)));
                if let Some(tt) = self.ctx.snap.lookup_term(&tt) {
                    for [r, _, _, g] in self.quads(Perm::Pos, &[reifies.0, tt.0])? {
                        self.reserve(128)?;
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
                        self.reserve(128)?;
                        found.push((r, g.0));
                    }
                }
            }
            self.reserve(256)?;
            self.reifiers.insert((s, p, o), found);
        }
        self.reserve(self.reifiers[&(s, p, o)].len() as u64 * 32 + 256)?;
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
        let _linked_charge = DescriptionCharge::new(self.ctx, self.linked.len() as u64 * 64 + 256)?;
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

#[cfg(test)]
mod retained_output_tests {
    use super::*;
    use crate::sparql::{QueryOptions, query};
    use crate::store::{Store, StoreOptions};

    fn context() -> Arc<Ctx> {
        let store = Store::in_memory(StoreOptions::default());
        query(store.snapshot(), "ASK {}", &QueryOptions::default())
            .unwrap()
            .test_context()
            .clone()
    }
    fn graph(ctx: &Ctx) -> Described<'_> {
        let charge = DescriptionCharge::new(ctx, 128 << 10).unwrap();
        let triple = Triple::new(
            oxrdf::NamedNode::new_unchecked("urn:s"),
            oxrdf::NamedNode::new_unchecked("urn:p"),
            oxrdf::Literal::new_simple_literal("x".repeat(64 << 10)),
        );
        Described {
            triples: vec![triple],
            truncated: false,
            _charge: Some(charge),
        }
    }
    #[test]
    fn original_credit_transfers_without_returning_the_payload_admission() {
        let ctx = context();
        let baseline = ctx.live_bytes();
        let described = graph(&ctx);
        assert_eq!(ctx.live_bytes(), baseline + (128 << 10));
        let (triples, credit) = described.into_output(&ctx).unwrap();
        assert_eq!(triples.len(), 1);
        assert!(ctx.live_bytes() >= baseline + (64 << 10));
        assert!(ctx.live_bytes() < baseline + (128 << 10));
        drop(triples);
        assert!(ctx.live_bytes() >= baseline + (64 << 10));
        drop(credit);
        assert_eq!(ctx.live_bytes(), baseline);
    }
    #[test]
    fn foreign_context_refuses_and_returns_only_the_original_owner_credit() {
        let ctx = context();
        let foreign = context();
        let baseline = ctx.live_bytes();
        let other = foreign.live_bytes();
        assert!(matches!(
            graph(&ctx).into_output(&foreign),
            Err(Error::Invalid(_))
        ));
        assert_eq!(ctx.live_bytes(), baseline);
        assert_eq!(foreign.live_bytes(), other);
    }
}
