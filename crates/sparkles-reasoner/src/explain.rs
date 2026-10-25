//! Explanations of inferred triples (spec F18 §3, U10 §3.3).
//!
//! Two engines give the facts a search reads. Over a materialized closure, the search reads
//! the input graphs and the inferred graph of a snapshot through the reasoner's base tier,
//! so nothing is loaded up front ([`explain`]). Over F17's demand engine, a run of the demand
//! program with the triple as its only seed derives the facts the triple needs, generalized
//! ones included, and the search reads the input graphs and those facts ([`explain_demand`]).
//! The demand program of an explanation does not rewrite linear recursion, so every fact
//! it derives has, among the facts derived, the premises of an instance of an original
//! rule, and a tree exists for every fact it derives.
//!
//! The search walks backward from the triple a level at a time. Each level enumerates, for
//! the facts it reached, the rule instances whose head is the fact and whose premises are
//! among the facts read, with the same backward plans as the incremental search. After
//! each level it works out, for every fact reached, the smallest depth at which it is
//! supported: an asserted fact has depth 0, and a fact has depth `d + 1` when one of its
//! instances has premises of depth `d` or less. A fact that only facts depending on it
//! support never gets a depth, so every tree the search returns is well founded, whatever
//! order incremental runs or a semi-naive evaluation left the rows in. The search stops at
//! the first level where the triple has a depth, and the depths of the facts it reached
//! then describe a tree of the smallest depth. Each instance keeps its bindings, so a node
//! names the values its rule bound and the builtins it passed.
//!
//! The leaves are the facts of the input graphs the caller passes, which are the graphs
//! the caller can read, in the snapshot of the caller's view, and for a why-not answer the
//! facts of the hypothesis. A derivation is returned only when it reaches such facts, and
//! when none exists the answer is the same as for a triple that is not entailed.
//!
//! The search keeps every instance it enumerates until it answers. Its memory grows with
//! the instance budget, about 100 bytes per instance and its bindings, so a request at the
//! default budget of 100,000 instances holds about 10 to 20 MB at most, and all of it is
//! charged to the request's memory budget. The deadline is read between rules and inside
//! each rule's evaluation, so a rule whose candidates all fail stops at it too.

use crate::budget::Budget;
use crate::engine::{self, CRule, Eval, Mode, RuleInfo, Slot};
use crate::facts::Base;
use crate::graph::{Graph, Triple};
use crate::incremental::{Plans, bind_head};
use crate::query::QueryReasoner;
use crate::terms::Terms;
use crate::{Extras, Profile};
use oxrdf::Term;
use rustc_hash::{FxHashMap, FxHashSet};
pub use sparkles_core::explain::{
    Derivation, DerivationKind, ExplainBudgetExceeded, Explained, RuleRef,
};
use sparkles_core::id::{Id, Tag};
use sparkles_core::index::Perm;
use sparkles_core::sparql::reasoning::{Demand, DemandTerm, SessionRequest};
use sparkles_core::store::Snapshot;
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// The rule instances one explanation visits by default.
pub const DEFAULT_MAX_INSTANCES: usize = 100_000;
/// The depth of a returned tree by default. Deeper subtrees are cut as `ref` nodes.
pub const DEFAULT_MAX_DEPTH: u32 = 32;
/// The largest depth a request may ask for.
pub const MAX_DEPTH_LIMIT: u32 = 64;
/// The most distinct top-level derivations a request may ask for.
pub const MAX_TREES_LIMIT: usize = 10;
/// The most levels the search walks back from the triple. A derivation deeper than this
/// is reported as over the budget.
pub const SEARCH_DEPTH: u32 = 128;
/// After the first tree is found, the levels the search walks further to find derivations
/// whose top rule differs. Their trees may be deeper than the first one.
const EXTRA_LEVELS: u32 = 3;
/// The rule index of the GeoSPARQL default geometry, which no rule of the program derives.
const GEO_RULE: u32 = u32::MAX - 1;

/// Limits of one explanation.
#[derive(Clone, Debug)]
pub struct ExplainOptions {
    /// distinct top-level derivations, at least 1 and at most [`MAX_TREES_LIMIT`]
    pub max_trees: usize,
    /// the depth of the returned trees, at most [`MAX_DEPTH_LIMIT`]
    pub max_depth: u32,
    /// the rule instances the search may visit
    pub max_instances: usize,
    /// when the search gives up
    pub deadline: Option<Instant>,
    /// set to `true` to stop the search
    pub cancel: Option<Arc<AtomicBool>>,
    /// the memory the search and its rule evaluations may hold, unbounded when `None`
    pub max_memory: Option<usize>,
}

impl Default for ExplainOptions {
    fn default() -> ExplainOptions {
        ExplainOptions {
            max_trees: 1,
            max_depth: DEFAULT_MAX_DEPTH,
            max_instances: DEFAULT_MAX_INSTANCES,
            deadline: None,
            cancel: None,
            max_memory: None,
        }
    }
}

/// What the search over a materialized closure reads.
pub struct ExplainInput<'a> {
    /// the snapshot of the caller's view, at the commit the closure was computed
    pub snap: Arc<Snapshot>,
    /// the profile and the vocabularies the closure was computed with
    pub profile: &'a Profile,
    pub extras: &'a Extras,
    /// the input graphs whose facts are leaves, in the order the run resolved them, and
    /// only those the caller can read
    pub inputs: &'a [Id],
    /// the inferred graph, when the caller can read it
    pub inferred: Option<Id>,
}

/// What an explanation over the demand engine reads (spec F18 §3.1).
pub struct DemandInput<'a> {
    /// the rules, the input graphs and the limit of derived facts of query-time reasoning
    pub reasoner: &'a QueryReasoner,
    /// the snapshot of the caller's view
    pub snap: Arc<Snapshot>,
    /// whether the caller may read a graph, `None` for every graph
    pub readable: Option<Arc<dyn Fn(Id) -> bool + Send + Sync>>,
}

/// The OWL 2 RL rule id of a rule of the `owl-rl` profile: its name when the name follows
/// the profile's tables.
pub fn owl2rl_id(profile: &str, name: &str) -> Option<String> {
    const PREFIXES: &[&str] = &["prp-", "cax-", "scm-", "eq-", "cls-", "dt-"];
    (profile == "owl-rl" && PREFIXES.iter().any(|p| name.starts_with(p))).then(|| name.to_string())
}

/// One rule instance the search enumerated.
struct Inst {
    rule: u32,
    head: u32,
    /// the premises, as a range of `Search::premises`
    from: u32,
    to: u32,
    /// the bindings, as a range of `Search::binds`
    binds: u32,
}

const NONE: u32 = u32::MAX;

/// Per fact, its depth and the instance that gives it, and the root's top instances.
type RootTops = (Vec<u32>, Vec<u32>, Vec<u32>);

/// What a leaf of a tree is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Leaf {
    /// a fact of an input graph the caller reads
    Graph(Id),
    /// a missing fact of a why-not hypothesis
    Missing,
}

/// The rules a search reads, with what a tree names of them.
pub(crate) struct RuleSet<'r> {
    pub rules: &'r [CRule],
    pub infos: &'r [RuleInfo],
    /// the profile or vocabulary of each parsed rule
    pub sources: &'r [String],
    /// the ids of `geo:hasGeometry` and `geo:hasDefaultGeometry`, when default geometries
    /// are facts of the closure
    pub geo: Option<(u64, u64)>,
}

struct Search<'a> {
    snap: &'a Snapshot,
    g: &'a Graph,
    terms: &'a Terms,
    rs: &'a RuleSet<'a>,
    plans: &'a Plans,
    inputs: &'a [Id],
    /// the facts of a why-not hypothesis, leaves of their own
    extra: &'a FxHashSet<Triple>,
    budget: &'a Arc<Budget>,
    opts: &'a ExplainOptions,
    abort: AtomicBool,
    facts: Vec<Triple>,
    index: FxHashMap<Triple, u32>,
    /// per fact: what makes it a leaf, or `None`
    leaf: Vec<Option<Leaf>>,
    /// per fact: its instances
    by_head: Vec<Vec<u32>>,
    insts: Vec<Inst>,
    premises: Vec<u32>,
    binds: Vec<u64>,
    visited: usize,
    /// per fact, after [`settle`](Self::settle): its depth (`NONE` when it has none) and
    /// the instance that gives it (`NONE` for a leaf)
    depth: Vec<u32>,
    via: Vec<u32>,
    /// the bytes of `depth` and `via` charged to the budget
    kept: usize,
}

/// Why a search stopped before its answer.
enum Stop {
    Budget(String),
    Error(anyhow::Error),
}

/// The heap one fact takes in the search: its triple, its index entry, its leaf and its
/// list of instances.
const FACT_BYTES: usize = 3 * 8 + 48 + 16 + 24 + 8;
/// The heap one instance takes, without its premises and bindings.
const INST_BYTES: usize = std::mem::size_of::<Inst>() + 4;

impl<'a> Search<'a> {
    /// Charge `n` bytes of the search's own state to the budget.
    fn charge(&self, n: usize) -> Result<(), Stop> {
        self.budget.try_charge(n).map_err(|e| {
            Stop::Error(
                e.with_context(" while explaining a triple".to_string())
                    .into(),
            )
        })
    }

    fn fact(&mut self, t: Triple) -> Result<(u32, bool), Stop> {
        if let Some(&i) = self.index.get(&t) {
            return Ok((i, false));
        }
        self.charge(FACT_BYTES + 8)?;
        let i = self.facts.len() as u32;
        let leaf = self.asserted(&t);
        self.facts.push(t);
        self.index.insert(t, i);
        self.leaf.push(leaf);
        self.by_head.push(Vec::new());
        // Expansion can be the last work completed before the deadline. Keep these
        // arrays aligned even before settle gives derived premises their proof depths.
        self.depth.push(if leaf.is_some() { 0 } else { NONE });
        self.via.push(NONE);
        self.kept += 8;
        Ok((i, true))
    }

    /// What makes `t` a leaf: a hypothesis holds it, or an input graph does.
    fn asserted(&self, t: &Triple) -> Option<Leaf> {
        if self.extra.contains(t) {
            return Some(Leaf::Missing);
        }
        if t.iter().any(|&x| Id(x).tag() == Tag::Local) {
            return None;
        }
        self.inputs
            .iter()
            .copied()
            .find(|g| {
                self.snap
                    .count(Perm::Gspo, &[g.0, t[0], t[1], t[2]])
                    .is_ok_and(|n| n > 0)
            })
            .map(Leaf::Graph)
    }

    fn check_time(&self) -> Result<(), Stop> {
        if self
            .opts
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            return Err(Stop::Error(sparkles_core::Error::Cancelled.into()));
        }
        if self.opts.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(Stop::Budget(
                "the explanation passed its timeout".to_string(),
            ));
        }
        Ok(())
    }

    /// Enumerate the instances of fact `f`, returning the facts they reached first.
    fn expand(&mut self, f: u32, next: &mut Vec<u32>) -> Result<(), Stop> {
        self.check_time()?;
        let t = self.facts[f as usize];
        // a default geometry follows from the feature's one geometry
        if let Some((has, default)) = self.rs.geo
            && t[1] == default
        {
            let premise = [t[0], has, t[2]];
            if self.g.contains(&premise) {
                self.visited += 1;
                self.keep_one(f, GEO_RULE, vec![premise], Vec::new(), next)?;
            }
        }
        let heads: Vec<(usize, usize)> = self.plans.heads(t[1]).collect();
        for (ri, hi) in heads {
            self.check_time()?;
            let Some((steps, pre)) = &self.plans.back[ri][hi] else {
                continue;
            };
            let rule = &self.rs.rules[ri];
            let Some(mut b) = bind_head(rule, hi, pre, t) else {
                continue;
            };
            let mut found: Vec<(Vec<Triple>, Vec<u64>)> = Vec::new();
            let over = Cell::new(false);
            let no_room = Cell::new(None);
            let held = Cell::new(0usize);
            let room = self.opts.max_instances.saturating_sub(self.visited);
            let deadline = self.opts.deadline;
            let budget = self.budget;
            let failed = {
                let mut sink = |matched: &[Triple], implicit: &[Triple], vals: &[u64]| -> bool {
                    if found.len() >= room
                        || deadline.is_some_and(|d| found.len() % 64 == 63 && Instant::now() >= d)
                    {
                        over.set(true);
                        return false;
                    }
                    let mut body: Vec<Triple> = Vec::with_capacity(matched.len() + implicit.len());
                    for u in matched.iter().chain(implicit) {
                        if !body.contains(u) {
                            body.push(*u);
                        }
                    }
                    // the buffers until the instances are kept, given back below
                    let n = 48 + body.capacity() * std::mem::size_of::<Triple>() + vals.len() * 8;
                    if let Err(e) = budget.try_charge(n) {
                        no_room.set(Some(e));
                        return false;
                    }
                    held.set(held.get() + n);
                    found.push((body, vals.to_vec()));
                    true
                };
                let mut ev = Eval::new(
                    self.g,
                    self.terms,
                    rule,
                    steps,
                    self.g.len(),
                    &self.abort,
                    self.opts.cancel.as_deref(),
                    usize::MAX,
                    Mode::Instances {
                        head: hi,
                        target: t,
                        sink: &mut sink,
                    },
                    self.budget,
                );
                ev.set_deadline(deadline);
                ev.run(0, &mut b, None);
                ev.err.take()
            };
            let held = held.get();
            if let Some(e) = no_room.take() {
                self.budget.release(held);
                return Err(Stop::Error(
                    e.with_context(" while explaining a triple".to_string())
                        .into(),
                ));
            }
            if let Some(e) = failed {
                self.budget.release(held);
                return Err(Stop::Error(e));
            }
            // a rule whose candidates all failed stops at the deadline without an instance
            if let Err(e) = self.check_time() {
                self.budget.release(held);
                return Err(e);
            }
            let mut kept = Ok(());
            for (body, vals) in found {
                self.visited += 1;
                kept = self.keep_one(f, ri as u32, body, vals, next);
                if kept.is_err() {
                    break;
                }
            }
            self.budget.release(held);
            kept?;
            if over.get() {
                self.check_time()?;
                return Err(Stop::Budget(format!(
                    "the explanation visited its limit of {} rule instances",
                    self.opts.max_instances
                )));
            }
        }
        Ok(())
    }

    /// Keep one instance of rule `ri` for fact `f`, with its premises `body` and its
    /// bindings `vals`.
    fn keep_one(
        &mut self,
        f: u32,
        ri: u32,
        body: Vec<Triple>,
        vals: Vec<u64>,
        next: &mut Vec<u32>,
    ) -> Result<(), Stop> {
        let t = self.facts[f as usize];
        // an instance among whose premises is its own head supports nothing
        if body.contains(&t) {
            return Ok(());
        }
        self.charge(INST_BYTES + 4 * body.len() + 8 * vals.len())?;
        let from = self.premises.len() as u32;
        for u in body {
            let (m, new) = self.fact(u)?;
            self.premises.push(m);
            if new && self.leaf[m as usize].is_none() {
                next.push(m);
            }
        }
        let binds = self.binds.len() as u32;
        self.binds.extend(vals);
        let i = self.insts.len() as u32;
        self.insts.push(Inst {
            rule: ri,
            head: f,
            from,
            to: self.premises.len() as u32,
            binds,
        });
        self.by_head[f as usize].push(i);
        Ok(())
    }

    /// The depth of every fact reached, and the instance that gives it.
    fn settle(&mut self) -> Result<(), Stop> {
        let (depth, via) = self.settle_with(None)?;
        // the new arrays are charged and the ones they replace given back
        let bytes = 8 * depth.len();
        self.charge(bytes)?;
        self.budget.release(self.kept);
        self.kept = bytes;
        self.depth = depth;
        self.via = via;
        Ok(())
    }

    /// The depth of every fact reached and the instance that gives it, with `exclude`
    /// given no depth, so that no derivation of another fact goes through it.
    fn settle_with(&self, exclude: Option<u32>) -> Result<(Vec<u32>, Vec<u32>), Stop> {
        let n = self.facts.len();
        // the depths, the instances, the users of each fact and the open counts
        let work = n * (4 + 4 + 24) + self.premises.len() * 4 + self.insts.len() * 4;
        self.charge(work)?;
        let mut depth = vec![NONE; n];
        let mut via = vec![NONE; n];
        let mut users: Vec<Vec<u32>> = vec![Vec::new(); n];
        let mut open: Vec<u32> = Vec::with_capacity(self.insts.len());
        let mut bucket: Vec<u32> = Vec::new();
        let mut later: Vec<u32> = Vec::new();
        for (f, l) in self.leaf.iter().enumerate() {
            if l.is_some() && exclude != Some(f as u32) {
                depth[f] = 0;
                bucket.push(f as u32);
            }
        }
        for (i, inst) in self.insts.iter().enumerate() {
            let ps = &self.premises[inst.from as usize..inst.to as usize];
            open.push(ps.len() as u32);
            for &p in ps {
                users[p as usize].push(i as u32);
            }
            let h = inst.head;
            if ps.is_empty() && depth[h as usize] == NONE && exclude != Some(h) {
                depth[h as usize] = 1;
                via[h as usize] = i as u32;
                later.push(h);
            }
        }
        let mut d = 0u32;
        while !bucket.is_empty() || !later.is_empty() {
            for f in std::mem::take(&mut bucket) {
                for &i in &users[f as usize] {
                    open[i as usize] -= 1;
                    if open[i as usize] == 0 {
                        let h = self.insts[i as usize].head;
                        if depth[h as usize] == NONE && exclude != Some(h) {
                            depth[h as usize] = d + 1;
                            via[h as usize] = i;
                            later.push(h);
                        }
                    }
                }
            }
            d += 1;
            bucket = std::mem::take(&mut later)
                .into_iter()
                .filter(|&f| depth[f as usize] == d)
                .collect();
        }
        // the workspace is given back, and a caller that keeps the depths charges them
        self.budget.release(work);
        Ok((depth, via))
    }

    /// The instances of fact `f` whose premises all have a depth in `depth`, each with a
    /// set of premises of its own (spec F18 §3.1: two trees differ at least at the root's
    /// rule instance), `first` first and the others by depth, those of a rule not given
    /// yet before another instance of a rule already given.
    fn tops(&self, f: u32, depth: &[u32], first: u32) -> Vec<u32> {
        let mut v: Vec<(u32, u32)> = Vec::new();
        let mut seen: FxHashSet<(u32, Vec<u32>)> = FxHashSet::default();
        for &i in &self.by_head[f as usize] {
            let inst = &self.insts[i as usize];
            let ps = &self.premises[inst.from as usize..inst.to as usize];
            if ps.iter().any(|&p| depth[p as usize] == NONE) {
                continue;
            }
            let mut key: Vec<u32> = ps.to_vec();
            key.sort_unstable();
            if !seen.insert((inst.rule, key)) {
                continue;
            }
            let d = 1 + ps.iter().map(|&p| depth[p as usize]).max().unwrap_or(0);
            v.push((d, i));
        }
        v.sort_by_key(|&(d, i)| (i != first, d, i));
        // a rule's first instance before the second instances of rules already given
        let mut given: FxHashSet<u32> = FxHashSet::default();
        let (mut new, mut again): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
        for (_, i) in v {
            if given.insert(self.insts[i as usize].rule) {
                new.push(i);
            } else {
                again.push(i);
            }
        }
        new.extend(again);
        new
    }

    /// The derivations of the root `r` whose top rules differ: the depths with `r`
    /// excluded, so that no tree derives the root from itself, and its instances by them.
    /// The instance of the root's least depth needs no exclusion, since every premise
    /// under it is shallower than the root, and it comes first.
    fn root_tops(&self, r: u32) -> Result<RootTops, Stop> {
        let (depth, via) = self.settle_with(Some(r))?;
        let tops = self.tops(r, &depth, self.via[r as usize]);
        Ok((depth, via, tops))
    }
}

/// The heap a decoded term holds.
fn term_bytes(t: &Term) -> usize {
    sparkles_core::explain::term_bytes(t)
}

/// A term written for a builtin call: N-Triples, or `_` for a wildcard.
fn call_arg(t: Option<Term>) -> String {
    t.map_or_else(|| "_".to_string(), |t| t.to_string())
}

/// Builds the answer's trees, numbering facts once across them.
struct Builder<'s, 'a> {
    s: &'s Search<'a>,
    /// charged with the answer's nodes and decoded terms, which it holds until it returns
    budget: &'s Arc<Budget>,
    max_depth: u32,
    ids: FxHashMap<u32, u32>,
    next: u32,
}

impl Builder<'_, '_> {
    fn term(&self, id: u64) -> anyhow::Result<Term> {
        self.s
            .terms
            .term(id)
            .ok_or_else(|| anyhow::anyhow!("term {id} of a derivation is unknown"))
    }

    fn triple(&self, f: u32) -> anyhow::Result<[Term; 3]> {
        let t = self.s.facts[f as usize];
        let triple = [self.term(t[0])?, self.term(t[1])?, self.term(t[2])?];
        // the node and its decoded terms, which a large literal can make large
        let bytes =
            std::mem::size_of::<Derivation>() + triple.iter().map(term_bytes).sum::<usize>();
        self.budget
            .try_charge(bytes)
            .map_err(|e| e.with_context(" while building an explanation".to_string()))?;
        Ok(triple)
    }

    fn graph(&self, g: Id) -> anyhow::Result<String> {
        if g == Id::DEFAULT_GRAPH {
            return Ok("default".to_string());
        }
        match self.term(g.0)? {
            Term::NamedNode(n) => Ok(n.into_string()),
            t => anyhow::bail!("graph {t} is not an IRI"),
        }
    }

    fn rule(&self, inst: u32) -> RuleRef {
        let ri = self.s.insts[inst as usize].rule;
        if ri == GEO_RULE {
            return RuleRef {
                name: "geo-default-geometry".to_string(),
                profile: "geosparql".to_string(),
                owl2rl: None,
            };
        }
        let rs = self.s.rs;
        let name = rs.rules[ri as usize].name.clone();
        let profile = rs
            .infos
            .get(ri as usize)
            .and_then(|i| rs.sources.get(i.parsed))
            .cloned()
            .unwrap_or_default();
        RuleRef {
            owl2rl: owl2rl_id(&profile, &name),
            name,
            profile,
        }
    }

    /// The bindings of instance `inst` by variable name, and its builtins as calls.
    #[allow(clippy::type_complexity)]
    fn bindings(&self, inst: u32) -> anyhow::Result<(Vec<(String, Term)>, Vec<String>)> {
        let s = self.s;
        let i = &s.insts[inst as usize];
        if i.rule == GEO_RULE {
            return Ok((Vec::new(), Vec::new()));
        }
        let rule = &s.rs.rules[i.rule as usize];
        let Some(info) = s.rs.infos.get(i.rule as usize) else {
            return Ok((Vec::new(), Vec::new()));
        };
        let vals = &s.binds[i.binds as usize..i.binds as usize + rule.nvars];
        let mut bindings = Vec::new();
        let mut bytes = 0usize;
        for (v, &x) in vals.iter().enumerate() {
            if x == 0 {
                continue;
            }
            let t = self.term(x)?;
            let name = info.vars.get(v).cloned().unwrap_or_else(|| format!("v{v}"));
            bytes += name.len() + term_bytes(&t);
            bindings.push((name, t));
        }
        let mut builtins = Vec::new();
        for (j, b) in rule.builtins.iter().enumerate() {
            let args: Vec<String> = b
                .args
                .iter()
                .map(|a| match *a {
                    Slot::Const(c) => call_arg(s.terms.term(c)),
                    Slot::Var(v) if vals[v] != 0 => call_arg(s.terms.term(vals[v])),
                    _ => call_arg(None),
                })
                .collect();
            let name = info.builtins.get(j).map_or("builtin", String::as_str);
            let call = format!("{name}({})", args.join(", "));
            bytes += call.len();
            builtins.push(call);
        }
        self.budget
            .try_charge(bytes)
            .map_err(|e| e.with_context(" while building an explanation".to_string()))?;
        Ok((bindings, builtins))
    }

    fn generalized(&self, f: u32) -> bool {
        !self.s.terms.valid(&self.s.facts[f as usize])
    }

    fn leaf_node(&self, f: u32, id: u32, shared: bool, l: Leaf) -> anyhow::Result<Derivation> {
        let triple = self.triple(f)?;
        let mut d = Derivation::asserted(triple, None);
        d.id = id;
        d.shared = shared;
        match l {
            Leaf::Graph(g) => d.graph = Some(self.graph(g)?),
            Leaf::Missing => d.missing = true,
        }
        d.generalized = self.generalized(f);
        Ok(d)
    }

    /// The node of fact `f` at `level` of a tree, by instance `inst` (or the fact's own).
    fn node(&mut self, f: u32, level: u32, inst: Option<u32>) -> anyhow::Result<Derivation> {
        let s = self.s;
        self.next += 1;
        let (id, shared) = match (inst, self.ids.get(&f)) {
            (None, Some(&id)) => {
                self.next -= 1;
                (id, true)
            }
            // a root by an instance of its own is a node of its own, and is never a
            // premise, since a premise of a well-founded tree is not its root
            (Some(_), _) => (self.next, false),
            (None, None) => {
                // a node cut at the depth limit is no target for sharing, so a shallower
                // repeat of its fact shows its premises
                let i = s.via[f as usize];
                let cut = s.leaf[f as usize].is_none()
                    && level >= self.max_depth
                    && i != NONE
                    && s.insts[i as usize].from != s.insts[i as usize].to;
                if !cut {
                    self.ids.insert(f, self.next);
                }
                (self.next, false)
            }
        };
        let depth = s.depth[f as usize];
        if let Some(l) = s.leaf[f as usize] {
            return self.leaf_node(f, id, shared, l);
        }
        let triple = self.triple(f)?;
        let i = inst.unwrap_or(s.via[f as usize]);
        let ps = &s.insts[i as usize];
        let ps: Vec<u32> = s.premises[ps.from as usize..ps.to as usize].to_vec();
        let inst_depth = 1 + ps.iter().map(|&p| s.depth[p as usize]).max().unwrap_or(0);
        let kind = if ps.is_empty() {
            DerivationKind::Axiom
        } else if level >= self.max_depth {
            DerivationKind::Ref
        } else {
            DerivationKind::Derived
        };
        let rule = (kind != DerivationKind::Ref).then(|| self.rule(i));
        let (bindings, builtins) = if kind == DerivationKind::Ref || shared {
            (Vec::new(), Vec::new())
        } else {
            self.bindings(i)?
        };
        let mut premises = Vec::new();
        if kind == DerivationKind::Derived && !shared {
            for p in ps {
                premises.push(self.node(p, level + 1, None)?);
            }
        }
        Ok(Derivation {
            id,
            triple,
            kind,
            graph: None,
            rule,
            premises,
            shared,
            depth: if inst.is_some() { inst_depth } else { depth },
            bindings,
            builtins,
            generalized: self.generalized(f),
            missing: false,
        })
    }

    /// The node of fact `f` in a partial tree (ROADMAP item 135): a fact with a depth has
    /// its tree, and one without takes the instance with the most premises that have one,
    /// whose other premises are explained the same way, or is `unexplained`.
    fn partial(&mut self, f: u32, level: u32, path: &mut Vec<u32>) -> anyhow::Result<Derivation> {
        let s = self.s;
        if s.depth[f as usize] != NONE {
            return self.node(f, level, None);
        }
        self.next += 1;
        let id = self.next;
        let triple = self.triple(f)?;
        let mut best: Option<(usize, usize, u32)> = None;
        if level < self.max_depth {
            for &i in &s.by_head[f as usize] {
                let inst = &s.insts[i as usize];
                let ps = &s.premises[inst.from as usize..inst.to as usize];
                if ps.iter().any(|p| path.contains(p) || *p == f) {
                    continue;
                }
                let known = ps.iter().filter(|&&p| s.depth[p as usize] != NONE).count();
                let key = (known, usize::MAX - ps.len(), i);
                if best.is_none_or(|b| key > b) {
                    best = Some(key);
                }
            }
        }
        let Some((_, _, i)) = best else {
            let mut d = Derivation::asserted(triple, None);
            d.id = id;
            d.kind = DerivationKind::Unexplained;
            d.generalized = self.generalized(f);
            return Ok(d);
        };
        let rule = Some(self.rule(i));
        let (bindings, builtins) = self.bindings(i)?;
        let inst = &s.insts[i as usize];
        let ps: Vec<u32> = s.premises[inst.from as usize..inst.to as usize].to_vec();
        path.push(f);
        let mut premises = Vec::new();
        for p in ps {
            premises.push(self.partial(p, level + 1, path)?);
        }
        path.pop();
        let depth = 1 + premises.iter().map(|p| p.depth).max().unwrap_or(0);
        Ok(Derivation {
            id,
            triple,
            kind: DerivationKind::Derived,
            graph: None,
            rule,
            premises,
            shared: false,
            depth,
            bindings,
            builtins,
            generalized: self.generalized(f),
            missing: false,
        })
    }
}

/// Explain why the closure holds `triple`: up to `opts.max_trees` derivations from the
/// facts of `input.inputs`, whose top rules differ. Fails with [`ExplainBudgetExceeded`]
/// when the search passes its limits before it reaches a rule instance of the triple, and
/// answers [`Explained::Partial`] when it passes them after.
pub fn explain(
    input: &ExplainInput<'_>,
    triple: &[Term; 3],
    opts: &ExplainOptions,
) -> anyhow::Result<Explained> {
    // the memory limit is one of the explanation's limits, answered as the others are
    budget_errors(materialized(input, triple, opts))
}

pub(crate) fn budget_errors<T>(r: anyhow::Result<T>) -> anyhow::Result<T> {
    r.map_err(|e| {
        if e.chain().any(|c| c.is::<crate::budget::MemoryExceeded>()) {
            ExplainBudgetExceeded(format!("{e:#}")).into()
        } else {
            e
        }
    })
}

fn materialized(
    input: &ExplainInput<'_>,
    triple: &[Term; 3],
    opts: &ExplainOptions,
) -> anyhow::Result<Explained> {
    let snap = input.snap.clone();
    let budget = Budget::new(opts.max_memory);
    let mut graphs: Vec<Id> = input.inputs.to_vec();
    graphs.extend(input.inferred);
    let base = Arc::new(Base::new(snap.clone(), &graphs)?);
    let mut g = Graph::with_base(base);
    g.set_budget(&budget)?;
    let terms = Terms::new(snap.clone());
    terms.set_budget(&budget)?;
    let mut warnings = Vec::new();
    let parsed = crate::profile_rules(input.profile, input.extras)?;
    let sources = crate::rule_sources(input.profile, input.extras)?;
    let (rules, infos) = engine::compile_info(&parsed, &terms, &mut warnings, false);
    let rs = RuleSet {
        rules: &rules,
        infos: &infos,
        sources: &sources,
        geo: input
            .extras
            .geo_default_geometry
            .then(|| crate::geo_ids(&terms)),
    };
    let mut root = [0u64; 3];
    for (k, t) in triple.iter().enumerate() {
        let id = terms.try_id_for(t)?;
        if Id(id).tag() == Tag::Local {
            return Ok(Explained::NotFound);
        }
        root[k] = id;
    }
    if !g.contains(&root) {
        return Ok(Explained::NotFound);
    }
    let extra = FxHashSet::default();
    search(
        &snap,
        &mut g,
        &terms,
        &rs,
        input.inputs,
        &extra,
        &budget,
        root,
        opts,
    )
}

/// The search for `root` over `g`, whose leaves are the facts of `inputs` and `extra`.
#[allow(clippy::too_many_arguments)]
fn search(
    snap: &Snapshot,
    g: &mut Graph,
    terms: &Terms,
    rs: &RuleSet<'_>,
    inputs: &[Id],
    extra: &FxHashSet<Triple>,
    budget: &Arc<Budget>,
    root: Triple,
    opts: &ExplainOptions,
) -> anyhow::Result<Explained> {
    let plans = Plans::new(g, rs.rules)?;
    let g: &Graph = g;
    let max_trees = opts.max_trees.clamp(1, MAX_TREES_LIMIT);
    let mut s = Search {
        snap,
        g,
        terms,
        rs,
        plans: &plans,
        inputs,
        extra,
        budget,
        opts,
        abort: AtomicBool::new(false),
        facts: Vec::new(),
        index: FxHashMap::default(),
        leaf: Vec::new(),
        by_head: Vec::new(),
        insts: Vec::new(),
        premises: Vec::new(),
        binds: Vec::new(),
        visited: 0,
        depth: Vec::new(),
        via: Vec::new(),
        kept: 0,
    };
    let stop = |e: Stop| -> anyhow::Error {
        match e {
            Stop::Budget(m) => ExplainBudgetExceeded(m).into(),
            Stop::Error(e) => e,
        }
    };
    let (r, _) = s.fact(root).map_err(stop)?;
    let mut frontier = if s.leaf[r as usize].is_some() {
        Vec::new()
    } else {
        vec![r]
    };
    let mut found_at: Option<u32> = None;
    let mut level = 0u32;
    // the limit the search passed before it found a tree, when it reached instances of
    // the root: the answer is then a partial tree
    let mut short: Option<String> = None;
    loop {
        // the controls hold at every level, also when the root needs no expansion
        if let Err(e) = s.check_time() {
            match e {
                Stop::Budget(m) if !s.by_head[r as usize].is_empty() => {
                    short = Some(m);
                    break;
                }
                e => return Err(stop(e)),
            }
        }
        s.settle().map_err(stop)?;
        if s.depth[r as usize] != NONE && found_at.is_none() {
            found_at = Some(level);
        }
        let enough = match found_at {
            None => false,
            Some(at) => {
                level >= at + EXTRA_LEVELS || s.root_tops(r).map_err(stop)?.2.len() > max_trees
            }
        };
        if enough || frontier.is_empty() {
            break;
        }
        if level >= SEARCH_DEPTH {
            if found_at.is_some() {
                break;
            }
            let m =
                format!("the derivation is deeper than the search allows ({SEARCH_DEPTH} levels)");
            if s.by_head[r as usize].is_empty() {
                return Err(ExplainBudgetExceeded(m).into());
            }
            short = Some(m);
            break;
        }
        let mut next = Vec::new();
        let mut stopped = None;
        for f in std::mem::take(&mut frontier) {
            if let Err(e) = s.expand(f, &mut next) {
                stopped = Some(e);
                break;
            }
        }
        match stopped {
            None => {}
            // the trees found so far stand
            Some(Stop::Budget(_)) if found_at.is_some() => {
                s.settle().map_err(stop)?;
                break;
            }
            Some(Stop::Budget(m)) if !s.by_head[r as usize].is_empty() => {
                s.settle().map_err(stop)?;
                if s.depth[r as usize] != NONE {
                    found_at = Some(level);
                    break;
                }
                short = Some(m);
                break;
            }
            Some(e) => return Err(stop(e)),
        }
        frontier = next;
        level += 1;
    }
    if found_at.is_none() {
        if let Some(reason) = short {
            let mut b = Builder {
                s: &s,
                budget,
                max_depth: opts.max_depth.min(MAX_DEPTH_LIMIT),
                ids: FxHashMap::default(),
                next: 0,
            };
            let tree = b.partial(r, 0, &mut Vec::new())?;
            return Ok(Explained::Partial {
                tree: Box::new(tree),
                reason,
            });
        }
        return Ok(Explained::NotFound);
    }
    // every tree reads the depths with the root excluded, which equal the plain depths
    // under the root's first instance. They replace arrays of the same length, so the
    // charge for the kept arrays stands.
    let (depth, via, tops) = s.root_tops(r).map_err(stop)?;
    s.depth = depth;
    s.via = via;
    let mut b = Builder {
        s: &s,
        budget,
        max_depth: opts.max_depth.min(MAX_DEPTH_LIMIT),
        ids: FxHashMap::default(),
        next: 0,
    };
    let mut trees = Vec::new();
    if s.leaf[r as usize].is_some() {
        trees.push(b.node(r, 0, None)?);
    }
    for &i in tops.iter().take(max_trees.saturating_sub(trees.len())) {
        // a tree shares repeated facts within itself only, so each reads on its own
        b.ids.clear();
        trees.push(b.node(r, 0, Some(i))?);
    }
    let more = tops.len() + usize::from(s.leaf[r as usize].is_some()) > trees.len();
    Ok(Explained::Found { trees, more })
}

/// The stop function of a session from an explanation's controls.
pub(crate) fn session_stop(
    opts: &ExplainOptions,
) -> Arc<dyn Fn() -> sparkles_core::Result<()> + Send + Sync> {
    let deadline = opts.deadline;
    let cancel = opts.cancel.clone();
    Arc::new(move || {
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(sparkles_core::Error::Cancelled);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(sparkles_core::Error::Timeout);
        }
        Ok(())
    })
}

/// An error of a demand session as an explanation's: a passed limit is
/// [`ExplainBudgetExceeded`].
pub(crate) fn session_error(e: sparkles_core::Error) -> anyhow::Error {
    match e {
        sparkles_core::Error::Timeout => {
            ExplainBudgetExceeded("the explanation passed its timeout".to_string()).into()
        }
        sparkles_core::Error::Component(c) if c.code == crate::query::BUDGET_CODE => {
            ExplainBudgetExceeded(c.message.clone()).into()
        }
        sparkles_core::Error::Cancelled => anyhow::Error::new(sparkles_core::Error::Cancelled),
        e => anyhow::Error::new(e),
    }
}

/// A demand for `t` with its three positions constant.
pub(crate) fn demand_of(t: &[Term; 3]) -> Demand {
    Demand {
        s: DemandTerm::Const(t[0].clone()),
        p: DemandTerm::Const(t[1].clone()),
        o: DemandTerm::Const(t[2].clone()),
    }
}

/// A demand session over `input`'s view, with the GeoSPARQL default geometries added when
/// the materialization adds them, and with `cached` as [`QueryReasoner::explain_session`]
/// takes it.
pub(crate) fn demand_session(
    input: &DemandInput<'_>,
    opts: &ExplainOptions,
    cached: bool,
) -> anyhow::Result<crate::query::Session> {
    let stop = session_stop(opts);
    stop().map_err(session_error)?;
    let mut session = input
        .reasoner
        .explain_session(
            SessionRequest {
                snap: input.snap.clone(),
                readable: input.readable.clone(),
                stop: session_stop(opts),
                max_memory: opts.max_memory.map(|m| m as u64),
                no_cache: false,
            },
            cached,
        )
        .map_err(session_error)?;
    if session.geo_default() {
        let budget = session.budget().clone();
        let base = session
            .graph_mut()
            .map_err(session_error)?
            .base_arc()
            .ok_or_else(|| anyhow::anyhow!("the session's graph has no base tier"))?;
        let (facts, _held) =
            crate::default_geometries_controlled(&base, session.terms(), &budget, || {
                stop().map_err(session_error)
            })?;
        // Every local ID belongs to the same dictionary as the compiled rules.
        session.add_facts(facts).map_err(session_error)?;
    }
    Ok(session)
}

/// The search for `root` over a demand session's facts.
pub(crate) fn search_session(
    session: &mut crate::query::Session,
    extra: &FxHashSet<Triple>,
    root: Triple,
    opts: &ExplainOptions,
) -> anyhow::Result<Explained> {
    let p = session.parts().map_err(session_error)?;
    let rs = RuleSet {
        rules: p.rules,
        infos: p.infos,
        sources: p.sources,
        geo: p.geo_default.then(|| crate::geo_ids(p.terms)),
    };
    search(
        p.snap, p.graph, p.terms, &rs, p.inputs, extra, p.budget, root, opts,
    )
}

/// Explain why `triple` is entailed, with F17's demand engine over the caller's view
/// (spec F18 §3.1): the demand program derives what the triple needs, and the search
/// reads the input graphs and the facts derived. Answers [`Explained::NotFound`] when the
/// program does not derive the triple.
pub fn explain_demand(
    input: &DemandInput<'_>,
    triple: &[Term; 3],
    opts: &ExplainOptions,
) -> anyhow::Result<Explained> {
    budget_errors((|| {
        let mut session = demand_session(input, opts, true)?;
        session
            .derive_all(&[demand_of(triple)])
            .map_err(session_error)?;
        let terms = session.terms();
        let root = [
            terms.try_id_for(&triple[0])?,
            terms.try_id_for(&triple[1])?,
            terms.try_id_for(&triple[2])?,
        ];
        let holds = session.graph().is_some_and(|g| g.contains(&root));
        if !holds {
            return Ok(Explained::NotFound);
        }
        search_session(&mut session, &FxHashSet::default(), root, opts)
    })())
}

#[cfg(test)]
mod partial_tests {
    use super::*;

    #[test]
    fn geometry_session_honors_a_preexisting_cancellation() {
        let store = sparkles_core::store::Store::in_memory(Default::default());
        let extras = Extras {
            geo_default_geometry: true,
            ..Default::default()
        };
        let reasoner =
            QueryReasoner::new(Profile::Rdfs, &extras, crate::Inputs::default()).unwrap();
        let input = DemandInput {
            reasoner: &reasoner,
            snap: store.snapshot(),
            readable: None,
        };
        let opts = ExplainOptions {
            cancel: Some(Arc::new(AtomicBool::new(true))),
            ..Default::default()
        };
        let error = demand_session(&input, &opts, true)
            .err()
            .expect("cancel before constructing all defaults");
        assert!(
            error.chain().any(|e| matches!(
                e.downcast_ref::<sparkles_core::Error>(),
                Some(sparkles_core::Error::Cancelled)
            )),
            "{error:#}"
        );
    }

    #[test]
    fn default_geometry_controls_interrupt_both_scans_and_output() {
        use sparkles_core::io::{RdfFormat, Source};
        let store = sparkles_core::store::Store::in_memory(Default::default());
        store
            .load(&[Source::from_bytes(
                br#"@prefix geo: <http://www.opengis.net/ont/geosparql#> .
            <urn:a> geo:hasGeometry <urn:g> . <urn:b> geo:hasGeometry <urn:g> .
            <urn:a> geo:hasDefaultGeometry <urn:g> ."#
                    .to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let reasoner =
            QueryReasoner::new(Profile::Rdfs, &Extras::default(), crate::Inputs::default())
                .unwrap();
        let input = DemandInput {
            reasoner: &reasoner,
            snap: store.snapshot(),
            readable: None,
        };
        let mut session = demand_session(&input, &ExplainOptions::default(), true).unwrap();
        let base = session.graph_mut().unwrap().base_arc().unwrap();
        // Entry, two hasGeometry rows, one explicit-default row, two count rows,
        // and two output rows are deterministic regardless of hash iteration order.
        for stop_at in [2, 4, 7] {
            let mut calls = 0;
            let error = crate::default_geometries_controlled(
                &base,
                session.terms(),
                &Budget::unlimited(),
                || {
                    calls += 1;
                    if calls == stop_at {
                        Err(sparkles_core::Error::Cancelled.into())
                    } else {
                        Ok(())
                    }
                },
            )
            .err()
            .expect("control interrupts geometry construction");
            assert_eq!(calls, stop_at);
            assert!(
                error
                    .downcast_ref::<sparkles_core::Error>()
                    .is_some_and(|e| matches!(e, sparkles_core::Error::Cancelled))
            );
        }
    }

    #[test]
    fn partial_tree_after_expansion_has_depth_entries_for_new_premises() {
        let store = sparkles_core::store::Store::in_memory(Default::default());
        let snap = store.snapshot();
        let budget = Budget::unlimited();
        let terms = Terms::new(snap.clone());
        terms.set_budget(&budget).unwrap();
        let named = |local: &str| {
            Term::NamedNode(oxrdf::NamedNode::new(format!("http://ex.org/{local}")).unwrap())
        };
        let root = [
            terms.id_for(&named("a")),
            terms.id_for(&named("q")),
            terms.id_for(&named("b")),
        ];
        let premise = [root[0], terms.id_for(&named("p")), root[2]];
        let profile = Profile::Rules("[step: (<http://ex.org/a> <http://ex.org/p> <http://ex.org/b>) -> (<http://ex.org/a> <http://ex.org/q> <http://ex.org/b>)]".into());
        let (rules, infos) =
            engine::compile_info(&profile.rules().unwrap(), &terms, &mut Vec::new(), false);
        let sources = vec!["rules".into()];
        let rs = RuleSet {
            rules: &rules,
            infos: &infos,
            sources: &sources,
            geo: None,
        };
        let mut g = Graph::with_capacity(0);
        let plans = Plans::new(&mut g, &rules).unwrap();
        let extra = FxHashSet::from_iter([premise]);
        let opts = ExplainOptions::default();
        let mut search = Search {
            snap: &snap,
            g: &g,
            terms: &terms,
            rs: &rs,
            plans: &plans,
            inputs: &[],
            extra: &extra,
            budget: &budget,
            opts: &opts,
            abort: AtomicBool::new(false),
            facts: Vec::new(),
            index: FxHashMap::default(),
            leaf: Vec::new(),
            by_head: Vec::new(),
            insts: Vec::new(),
            premises: Vec::new(),
            binds: Vec::new(),
            visited: 0,
            depth: Vec::new(),
            via: Vec::new(),
            kept: 0,
        };
        let (root, _) = search.fact(root).ok().expect("unlimited fact allocation");
        search
            .settle()
            .ok()
            .expect("unlimited proof-depth allocation");
        // The deadline can expire after this expansion and before the next settle.
        search
            .keep_one(root, 0, vec![premise], Vec::new(), &mut Vec::new())
            .ok()
            .expect("unlimited rule instance");
        let mut builder = Builder {
            s: &search,
            budget: &budget,
            max_depth: 32,
            ids: FxHashMap::default(),
            next: 0,
        };
        let tree = builder.partial(root, 0, &mut Vec::new()).unwrap();
        assert_eq!(tree.kind, DerivationKind::Derived);
        assert_eq!(tree.premises.len(), 1);
        assert_eq!(tree.premises[0].kind, DerivationKind::Asserted);
        assert!(tree.premises[0].missing);
    }
}
