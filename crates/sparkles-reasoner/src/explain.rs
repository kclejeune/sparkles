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

use crate::budget::{Budget, BudgetFailure, Held, WorkingWarnings};
use crate::engine::{self, CRule, Eval, Mode, RuleInfo, Slot, Step};
use crate::facts::Base;
use crate::graph::{Graph, Triple};
use crate::incremental::{Plans, bind_head};
use crate::query::QueryReasoner;
use crate::terms::Terms;
use crate::{Extras, Profile};
use oxrdf::Term;
use rustc_hash::{FxHashMap, FxHashSet};
pub use sparkles_core::explain::{
    Derivation, DerivationKind, ExplainBudgetExceeded, Explained, RelationAtom, RuleRef,
};
use sparkles_core::id::{Id, Tag};
use sparkles_core::index::Perm;
use sparkles_core::sparql::reasoning::SessionRequest;
#[cfg(test)]
use sparkles_core::sparql::reasoning::{Demand, DemandTerm};
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
    /// parent memory handle that the explanation's budget reserves from (`None`: unpooled)
    pub memory_parent: Option<crate::ParentHandle>,
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
            memory_parent: None,
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
    /// the caller's bound graph access, `None` for every graph
    pub readable: Option<Arc<sparkles_core::access::GraphAccess>>,
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
    /// the declared relations of a rule program (item 839), whose facts are nodes of
    /// their own
    pub relations: Option<&'r crate::ir::Relations>,
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
    /// Complete synchronous positive rounds, for a single minimum-depth tree.
    rounds: &'a [u32],
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
    /// Discovery restarts after a prefix proof, without a fresh search allowance.
    full_schedule: Option<Vec<bool>>,
    _full_schedule_allocation: Option<Held>,
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
        self.charge(FACT_BYTES + 8 + if self.full_schedule.is_some() { 4 } else { 0 })?;
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
        if let Some(scheduled) = &mut self.full_schedule {
            scheduled.push(false);
        }
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
        // Every new positive fact has a witness in the preceding immutable
        // round. Restrict only candidate premises; builtins still see the whole
        // current graph and every advertised instance is replayed there.
        let cutoff = self.g.position(&t).and_then(|row| {
            self.rounds
                .windows(2)
                .find(|ends| ends[0] <= row && row < ends[1])
                .map(|ends| ends[0])
        });
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
            let _limited_allocation = cutoff
                .map(|_| {
                    Held::try_new(
                        self.budget,
                        steps.len().saturating_mul(std::mem::size_of::<Step>()),
                    )
                })
                .transpose()
                .map_err(|e| Stop::Error(e.into()))?;
            let limited;
            let steps = if let Some(end) = cutoff {
                limited = steps
                    .iter()
                    .map(|step| match *step {
                        Step::Atom { atom, lo, hi } => Step::Atom {
                            atom,
                            lo,
                            hi: hi.min(end),
                        },
                        Step::Builtin(builtin) => Step::Builtin(builtin),
                    })
                    .collect::<Vec<_>>();
                &limited
            } else {
                steps
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
                    if cutoff.is_some_and(|end| {
                        matched
                            .iter()
                            .chain(implicit)
                            .any(|fact| self.g.position(fact).is_some_and(|row| row >= end))
                    }) {
                        return true;
                    }
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
            if self.leaf[m as usize].is_none() {
                let queue = if let Some(scheduled) = &mut self.full_schedule {
                    let queued = scheduled[m as usize];
                    scheduled[m as usize] = true;
                    !queued
                } else {
                    new
                };
                if queue {
                    next.push(m);
                }
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

pub(crate) struct CallBytes(pub usize);
impl std::fmt::Write for CallBytes {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0 = self.0.checked_add(s.len()).ok_or(std::fmt::Error)?;
        Ok(())
    }
}

pub(crate) fn write_call(
    out: &mut impl std::fmt::Write,
    prefix: &str,
    name: &str,
    args: &[Option<Term>],
    sep: &str,
) -> std::fmt::Result {
    write!(out, "{prefix}{name}(")?;
    for (i, arg) in args.iter().enumerate() {
        if i != 0 {
            out.write_str(sep)?;
        }
        match arg {
            Some(term) => write!(out, "{term}")?,
            None => out.write_str("_")?,
        }
    }
    out.write_str(")")
}

/// Builds the answer's trees, numbering facts once across them.
struct Builder<'s, 'a> {
    s: &'s Search<'a>,
    /// Own the output's credit until it is transferred to the returned proof.
    memory: Held,
    max_depth: u32,
    ids: FxHashMap<u32, u32>,
    next: u32,
}

impl Builder<'_, '_> {
    fn charge(&mut self, bytes: usize) -> Result<(), BudgetFailure> {
        self.memory
            .try_grow(bytes)
            .map_err(|e| e.with_context(" while building an explanation".to_string()))
    }

    fn term(&mut self, id: u64) -> anyhow::Result<Term> {
        let (term, held) = self.s.terms.term_admitted(id, self.memory.budget())?;
        let term = term.ok_or_else(|| anyhow::anyhow!("term {id} of a derivation is unknown"))?;
        self.memory.absorb(held);
        Ok(term)
    }

    /// The relation fact `f`, when it is one.
    fn relation(&mut self, f: u32) -> anyhow::Result<Option<RelationAtom>> {
        let t = self.s.facts[f as usize];
        let Some(r) = crate::ir::lower::rel_of(t[1]) else {
            return Ok(None);
        };
        let rel = self
            .s
            .rs
            .relations
            .and_then(|rs| rs.get(r))
            .ok_or_else(|| anyhow::anyhow!("relation {} of a derivation is unknown", r.0))?;
        let mut cols = Vec::with_capacity(rel.arity as usize);
        match rel.arity {
            0 => {}
            1 => cols.push(t[0]),
            2 => cols.extend([t[0], t[2]]),
            _ => {
                let (_, tail) = self
                    .s
                    .terms
                    .tail(t[2])
                    .ok_or_else(|| anyhow::anyhow!("a tail of a derivation is unknown"))?;
                cols.push(t[0]);
                cols.extend(tail);
            }
        }
        self.charge(rel.name.len())?;
        let args = cols
            .into_iter()
            .map(|x| self.term(x))
            .collect::<anyhow::Result<Vec<Term>>>()?;
        Ok(Some(RelationAtom {
            name: rel.name.to_string(),
            args,
        }))
    }

    fn triple(&mut self, f: u32) -> anyhow::Result<[Term; 3]> {
        let t = self.s.facts[f as usize];
        if let Some(r) = crate::ir::lower::rel_of(t[1]) {
            // a relation node's triple names its relation, and is no fact
            let name = self
                .s
                .rs
                .relations
                .and_then(|rs| rs.get(r))
                .map(|r| r.name.to_string())
                .unwrap_or_default();
            let iri = Term::NamedNode(
                RelationAtom {
                    name,
                    args: Vec::new(),
                }
                .iri(),
            );
            self.charge(std::mem::size_of::<Derivation>() + 3 * term_bytes(&iri))?;
            return Ok([iri.clone(), iri.clone(), iri]);
        }
        self.charge(std::mem::size_of::<Derivation>())?;
        Ok([self.term(t[0])?, self.term(t[1])?, self.term(t[2])?])
    }

    fn graph(&mut self, g: Id) -> anyhow::Result<String> {
        if g == Id::DEFAULT_GRAPH {
            self.charge("default".len())?;
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
    fn bindings(&mut self, inst: u32) -> anyhow::Result<(Vec<(String, Term)>, Vec<String>)> {
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
        for (v, &x) in vals.iter().enumerate() {
            if x == 0 {
                continue;
            }
            let name = info.vars.get(v).cloned().unwrap_or_else(|| format!("v{v}"));
            // a variable a relation's lowering added, such as a tail handle
            if name.is_empty() {
                continue;
            }
            self.charge(name.len())?;
            bindings.push((name, self.term(x)?));
        }
        let mut builtins = Vec::new();
        for (j, b) in rule.builtins.iter().enumerate() {
            use crate::builtins::BuiltinKind as B;
            let call = match b.kind {
                // the lowering of a relation atom, which the node shows as its premise
                B::RelTail | B::RelTuple => continue,
                B::RelAbsent => self.negated(&b.args, vals)?,
                _ => {
                    let name = info.builtins.get(j).map_or("builtin", String::as_str);
                    self.builtin_call("", name, &[&b.args], ", ", vals)?
                }
            };
            builtins.push(call);
        }
        Ok((bindings, builtins))
    }

    /// A negated atom (item 839) as a call: `not name(args)` for a relation, whose lowered
    /// arguments are the first column, the key and the other columns, and `not (s p o)`
    /// for a triple.
    fn negated(&mut self, args: &[Slot], vals: &[u64]) -> anyhow::Result<String> {
        let key = match args.get(1) {
            Some(Slot::Const(k)) => crate::ir::lower::rel_of(*k),
            _ => None,
        };
        let rel = key.and_then(|r| self.s.rs.relations.and_then(|rs| rs.get(r)));
        let Some(rel) = rel else {
            return self.builtin_call("not ", "", &[args], " ", vals);
        };
        let cols: &[&[Slot]] = match rel.arity {
            0 => &[],
            1 => &[&args[..1]],
            2 => &[&args[..1], &args[2..3]],
            _ => &[&args[..1], &args[2..]],
        };
        self.builtin_call("not ", &rel.name, cols, ", ", vals)
    }

    fn builtin_call(
        &mut self,
        prefix: &str,
        name: &str,
        groups: &[&[Slot]],
        sep: &str,
        vals: &[u64],
    ) -> anyhow::Result<String> {
        let count = groups.iter().map(|group| group.len()).sum::<usize>();
        let mut temporary = Held::try_new_exact(
            self.memory.budget(),
            count * std::mem::size_of::<Option<Term>>(),
        )?;
        let mut args = Vec::with_capacity(count);
        for arg in groups.iter().flat_map(|group| group.iter()) {
            let id = match *arg {
                Slot::Const(c) => c,
                Slot::Var(v) => vals[v],
                Slot::Any => 0,
            };
            let (term, held) = self.s.terms.term_admitted(id, self.memory.budget())?;
            temporary.absorb(held);
            args.push(term);
        }
        let mut bytes = CallBytes(0);
        write_call(&mut bytes, prefix, name, &args, sep)?;
        self.charge(bytes.0 + std::mem::size_of::<String>())?;
        let mut call = String::with_capacity(bytes.0);
        write_call(&mut call, prefix, name, &args, sep)?;
        Ok(call)
    }

    fn generalized(&self, f: u32) -> bool {
        let t = &self.s.facts[f as usize];
        !crate::ir::lower::is_key(t[1]) && !self.s.terms.valid(t)
    }

    fn leaf_node(&mut self, f: u32, id: u32, shared: bool, l: Leaf) -> anyhow::Result<Derivation> {
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
            read: None,
            relation: self.relation(f)?,
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
            d.relation = self.relation(f)?;
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
            read: None,
            relation: self.relation(f)?,
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
        if e.downcast_ref::<crate::budget::MemoryExceeded>().is_some() {
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
    let budget = Budget::with_parent(opts.max_memory, opts.memory_parent.clone());
    let mut graphs: Vec<Id> = input.inputs.to_vec();
    graphs.extend(input.inferred);
    let base = Arc::new(Base::new(snap.clone(), &graphs)?);
    let mut g = Graph::with_base_in(base, &budget)?;
    let terms = Terms::new_in_controlled(snap.clone(), &budget, || check_controls(opts))?;
    let mut warnings = WorkingWarnings::new(&budget);
    let parsed = crate::profile_rules(input.profile, input.extras)?;
    let sources = crate::rule_sources(input.profile, input.extras)?;
    if crate::parser::uses_program(&parsed) {
        drop(g);
        drop(terms);
        let source =
            |t: &Terms, w: &mut WorkingWarnings, check: &mut dyn FnMut() -> anyhow::Result<()>| {
                crate::engine::frontend::compile_controlled(&parsed, t, w, false, check)
            };
        // the generalized facts are not part of the dataset's graphs, so a derivation
        // through one is not found, as for Jena rules
        return program_search(
            snap,
            &source,
            &sources,
            input.extras.geo_default_geometry,
            input.inputs,
            input.inferred,
            &[],
            triple,
            opts,
        );
    }
    let compiled =
        engine::compile_info_controlled(&parsed, &terms, &mut warnings, false, &mut || {
            check_controls(opts)
        })?;
    let (rules, infos) = (compiled.rules(), compiled.infos());
    let rs = RuleSet {
        rules,
        infos,
        sources: &sources,
        geo: input
            .extras
            .geo_default_geometry
            .then(|| crate::geo_ids_controlled(&terms, || check_controls(opts)))
            .transpose()?,
        relations: None,
    };
    let mut root = [0u64; 3];
    for (k, t) in triple.iter().enumerate() {
        let id = terms.try_id_for_controlled(t, || check_controls(opts))?;
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
        &[],
    )
}

/// [`explain`] for a rule program with relations (item 839's plan, §12.3 and §13), whose
/// closure was materialized into `inferred`, with `generalized` the derived facts that
/// are no RDF, which the run saved beside it. The relation facts are not written, so they
/// are rebuilt on the graph of the inputs, the inferred graph and the generalized facts,
/// which is the complete closure of triples, with the explanation's budget and
/// cancellation, and the answer does not depend on a kept closure. [`explain`] reaches the
/// same search when a profile is a rule program. `program`'s constants must be terms of
/// the store, since the explanation compiles it again over its own terms.
#[cfg(test)]
pub(crate) fn explain_program(
    snap: Arc<Snapshot>,
    program: &crate::ir::Program,
    inputs: &[Id],
    inferred: Option<Id>,
    generalized: &[[Vec<u8>; 3]],
    triple: &[Term; 3],
    opts: &ExplainOptions,
) -> anyhow::Result<Explained> {
    let source =
        |t: &Terms, _: &mut WorkingWarnings, check: &mut dyn FnMut() -> anyhow::Result<()>| {
            let vocab = crate::ir::Vocab::try_of_controlled(t, check)?;
            let p = crate::ir::Program::new(
                program.relations.clone(),
                program.rules.clone(),
                &vocab,
                &t.budget(),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(crate::engine::frontend::ProgramInfo::bare(p, &t.budget()))
        };
    let sources = vec!["rules".to_string()];
    budget_errors(program_search(
        snap,
        &source,
        &sources,
        false,
        inputs,
        inferred,
        generalized,
        triple,
        opts,
    ))
}

/// What a program explanation compiles over its own terms.
type ExplainSource<'a> = dyn Fn(
        &Terms,
        &mut WorkingWarnings,
        &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<crate::engine::frontend::ProgramInfo>
    + 'a;

/// The lowered rules of every stratum of `program`, in stratum order, with the infos of
/// their IR rules: the names of the IR rules' variables, where the variables and builtins
/// a lowering adds have none. `held` is charged for the rules, their infos and the
/// origins, and the strata's buffers, which `lowered` charges, are given back to it as
/// the rules move out (item 839's plan, §12.3 and §20.2).
pub(crate) fn flat_rules(
    program: &crate::ir::Program,
    lowered: &mut crate::ir::lower::Lowered,
    held: &mut Held,
    lines: &[(usize, usize)],
) -> Result<(Vec<CRule>, Vec<RuleInfo>), BudgetFailure> {
    // the lowered rules of every stratum, with the names of their IR rules' variables;
    // the variables and builtins a lowering adds have none
    // the origins (exact vectors, with the scratch counts that size them), the flat
    // rules and their infos; the strata's own buffers, which `lowered` charges, are freed
    // as the rules move out and their bytes are given back below
    let n: usize = lowered.strata.iter().map(Vec::len).sum();
    let count = lowered.strata.len();
    held.try_grow(
        count * (size_of::<Vec<usize>>() + size_of::<usize>())
            + n * (size_of::<usize>() + size_of::<CRule>() + size_of::<RuleInfo>()),
    )?;
    let origins = program.lowered_origins();
    held.shrink(count * size_of::<usize>());
    let mut rules: Vec<CRule> = Vec::with_capacity(n);
    let mut infos: Vec<RuleInfo> = Vec::with_capacity(n);
    for (stratum, ris) in std::mem::take(&mut lowered.strata)
        .into_iter()
        .zip(&origins)
    {
        for (r, &ri) in stratum.into_iter().zip(ris) {
            let ir = &program.rules[ri];
            // the names of the variables and builtins, charged before they are made
            let names = (0..r.nvars.min(ir.nvars))
                .map(|v| {
                    ir.vars
                        .get(v)
                        .map_or(2 + v.checked_ilog10().unwrap_or(0) as usize, String::len)
                })
                .sum::<usize>()
                + r.builtins
                    .iter()
                    .map(|b| b.kind.rule_name().len())
                    .sum::<usize>();
            held.try_grow(names + (r.nvars + r.builtins.len()) * size_of::<String>())?;
            let vars = (0..r.nvars)
                .map(|v| {
                    if v >= ir.nvars {
                        String::new()
                    } else {
                        ir.vars.get(v).cloned().unwrap_or_else(|| format!("v{v}"))
                    }
                })
                .collect();
            let builtins = r
                .builtins
                .iter()
                .map(|b| b.kind.rule_name().to_string())
                .collect();
            let (line, parsed) = lines.get(ri).copied().unwrap_or((0, 0));
            infos.push(RuleInfo {
                vars,
                builtins,
                line,
                parsed,
            });
            rules.push(r);
        }
    }
    // the strata's buffers, exact in `lower`, are freed
    held.shrink(count * size_of::<Vec<CRule>>() + n * size_of::<CRule>());
    Ok((rules, infos))
}

#[allow(clippy::too_many_arguments)]
fn program_search(
    snap: Arc<Snapshot>,
    source: &ExplainSource<'_>,
    sources: &[String],
    geo: bool,
    inputs: &[Id],
    inferred: Option<Id>,
    generalized: &[[Vec<u8>; 3]],
    triple: &[Term; 3],
    opts: &ExplainOptions,
) -> anyhow::Result<Explained> {
    let budget = Budget::with_parent(opts.max_memory, opts.memory_parent.clone());
    let mut graphs: Vec<Id> = inputs.to_vec();
    graphs.extend(inferred);
    let base = Arc::new(Base::new(snap.clone(), &graphs)?);
    let mut g = Graph::with_base_in(base, &budget)?;
    let terms = Terms::new_in_controlled(snap.clone(), &budget, || check_controls(opts))?;
    // the generalized facts, saved as term keys, in this explanation's terms; the credit
    // also holds the metadata of the rules below while the search runs
    let mut held = Held::new(&budget);
    {
        held.try_grow(generalized.len() * size_of::<Triple>())?;
        let id = |k: &[u8]| terms.try_id_for_key_controlled(k, || check_controls(opts));
        let mut facts: Vec<Triple> = Vec::with_capacity(generalized.len());
        for k in generalized {
            facts.push([id(&k[0])?, id(&k[1])?, id(&k[2])?]);
        }
        g.try_add_batch(facts)?;
        held.shrink(generalized.len() * size_of::<Triple>());
    }
    // the program is compiled over this explanation's terms, which hold the local terms
    // its constants need
    let mut warnings = WorkingWarnings::new(&budget);
    let info = source(&terms, &mut warnings, &mut || check_controls(opts))?;
    let program = &info.program;
    let vocab = crate::ir::Vocab::try_of_controlled(&terms, || check_controls(opts))?;
    let part = program
        .relation_part(&vocab, &budget)
        .map_err(|e| match e {
            crate::ir::ProgramError::Memory(m) => anyhow::Error::from(m),
            e => anyhow::anyhow!("{e}"),
        })?;
    let mut lowered = program
        .lower(&budget)
        .map_err(crate::ir::lower::LowerError::into_anyhow)?;
    let (rules, infos) = flat_rules(program, &mut lowered, &mut held, &info.lines)?;
    let mut makes_terms = false;
    if let Some(part) = &part {
        let lp = part
            .lower(&budget)
            .map_err(crate::ir::lower::LowerError::into_anyhow)?;
        makes_terms = lp
            .strata
            .iter()
            .flatten()
            .any(|r| crate::ir::rebuild::makes_terms(r, &terms));
        drop(lp);
        let limits = engine::Limits {
            max_iterations: usize::MAX,
            max_inferred: usize::MAX,
            budget: budget.clone(),
            closures: false,
            magic: None,
            sampled_planning: false,
            cancel: opts.cancel.clone(),
            deadline: opts.deadline,
            progress: None,
        };
        if let Err(e) = crate::ir::rebuild::rebuild_relations(&mut g, part, &terms, &limits) {
            if opts.deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(ExplainBudgetExceeded(
                    "the explanation passed its timeout".to_string(),
                )
                .into());
            }
            return Err(e);
        }
    }
    let rs = RuleSet {
        rules: &rules,
        infos: &infos,
        sources,
        geo: geo
            .then(|| crate::geo_ids_controlled(&terms, || check_controls(opts)))
            .transpose()?,
        relations: Some(&program.relations),
    };
    let mut root = [0u64; 3];
    for (k, t) in triple.iter().enumerate() {
        let id = terms.try_id_for_controlled(t, || check_controls(opts))?;
        if Id(id).tag() == Tag::Local {
            return Ok(Explained::NotFound);
        }
        root[k] = id;
    }
    if !g.contains(&root) {
        return Ok(Explained::NotFound);
    }
    let extra = FxHashSet::default();
    let found = search(
        &snap,
        &mut g,
        &terms,
        &rs,
        inputs,
        &extra,
        &budget,
        root,
        opts,
        &[],
    )?;
    if makes_terms && matches!(found, Explained::NotFound) {
        anyhow::bail!(
            "the derivation may pass relation facts that make new terms, which an explanation cannot rebuild"
        );
    }
    Ok(found)
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
    rounds: &[u32],
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
        rounds: if max_trees == 1 { rounds } else { &[] },
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
        full_schedule: None,
        _full_schedule_allocation: None,
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
        // The prefix proves one minimum-depth tree, not completeness under
        // root exclusion. Discover alternatives with the original unpruned BFS:
        // a later independent support can be hidden by an earlier root-dependent
        // witness. Keep all consumed budgets and already proven instances.
        if found_at.is_some()
            && !s.rounds.is_empty()
            && s.leaf[r as usize].is_none()
            && s.root_tops(r).map_err(stop)?.2.len() <= max_trees
        {
            let allocation =
                Held::try_new(budget, s.facts.len().saturating_mul(4).saturating_add(64))
                    .map_err(|error| stop(Stop::Error(error.into())))?;
            let mut scheduled = vec![false; s.facts.len()];
            scheduled[r as usize] = true;
            s.full_schedule = Some(scheduled);
            s._full_schedule_allocation = Some(allocation);
            s.rounds = &[];
            frontier = vec![r];
            level = 0;
            continue;
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
                memory: Held::new(budget),
                max_depth: opts.max_depth.min(MAX_DEPTH_LIMIT),
                ids: FxHashMap::default(),
                next: 0,
            };
            let tree = b.partial(r, 0, &mut Vec::new())?;
            b.memory.into_bytes();
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
        memory: Held::new(budget),
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
    b.memory.into_bytes();
    Ok(Explained::Found { trees, more })
}

/// Fail when an explanation's cancellation flag or deadline has tripped.
pub(crate) fn check_controls(opts: &ExplainOptions) -> anyhow::Result<()> {
    if opts
        .cancel
        .as_ref()
        .is_some_and(|c| c.load(Ordering::Relaxed))
    {
        return Err(anyhow::Error::new(sparkles_core::Error::Cancelled));
    }
    if opts.deadline.is_some_and(|d| Instant::now() >= d) {
        return Err(ExplainBudgetExceeded("the explanation passed its timeout".to_string()).into());
    }
    Ok(())
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
#[cfg(test)]
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
                memory_parent: opts.memory_parent.clone(),
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
        geo: p
            .geo_default
            .then(|| crate::geo_ids_controlled(p.terms, || check_controls(opts)))
            .transpose()?,
        relations: p.relations,
    };
    search(
        p.snap,
        p.graph,
        p.terms,
        &rs,
        p.inputs,
        extra,
        p.budget,
        root,
        opts,
        p.compact_rounds,
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
        let root = {
            let terms = session.terms();
            [
                terms.try_id_for_controlled(&triple[0], || check_controls(opts))?,
                terms.try_id_for_controlled(&triple[1], || check_controls(opts))?,
                terms.try_id_for_controlled(&triple[2], || check_controls(opts))?,
            ]
        };
        session
            .derive_ids(&[[Some(root[0]), Some(root[1]), Some(root[2])]])
            .map_err(session_error)?;
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
            .expect_err("control interrupts geometry construction");
            assert_eq!(calls, stop_at);
            assert!(
                error
                    .downcast_ref::<sparkles_core::Error>()
                    .is_some_and(|e| matches!(e, sparkles_core::Error::Cancelled))
            );
        }
    }

    #[test]
    fn proof_triple_admits_the_second_literal_while_the_first_is_live() {
        let store = sparkles_core::store::Store::in_memory(Default::default());
        let mut tx = store.write();
        let literal = tx
            .intern(&oxrdf::Literal::new_simple_literal("z".repeat(65_536)).into())
            .unwrap();
        let predicate = tx
            .intern(&oxrdf::NamedNode::new_unchecked("urn:p").into())
            .unwrap();
        tx.insert([predicate, predicate, literal, Id::DEFAULT_GRAPH])
            .unwrap();
        tx.commit().unwrap();
        assert_eq!(literal.tag(), Tag::Delta);
        let snap = store.snapshot();
        let terms = Terms::new(snap.clone());
        let rs = RuleSet {
            rules: &[],
            infos: &[],
            sources: &[],
            geo: None,
            relations: None,
        };
        let mut graph = Graph::with_capacity(0);
        let plans = Plans::new(&mut graph, &[]).unwrap();
        const LIMIT: usize = 96 * 1024;
        let budget = Budget::new(Some(LIMIT));
        let search = Search {
            snap: &snap,
            g: &graph,
            terms: &terms,
            rs: &rs,
            plans: &plans,
            inputs: &[],
            extra: &FxHashSet::default(),
            budget: &budget,
            opts: &ExplainOptions::default(),
            rounds: &[],
            abort: AtomicBool::new(false),
            facts: vec![[literal.0, predicate.0, literal.0]],
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
            full_schedule: None,
            _full_schedule_allocation: None,
        };
        let mut builder = Builder {
            s: &search,
            memory: Held::new(&budget),
            max_depth: 1,
            ids: FxHashMap::default(),
            next: 0,
        };
        let (result, peak) = crate::alloc_probe::peak_of(|| builder.triple(0));
        assert!(
            peak <= LIMIT,
            "proof allocated both 64 KiB literals before refusing (peak={peak})"
        );
        assert!(BudgetFailure::from_anyhow(&result.unwrap_err()).is_some());
        drop(builder);
        assert_eq!(budget.used(), 0);

        let mut builder = Builder {
            s: &search,
            memory: Held::new(&budget),
            max_depth: 1,
            ids: FxHashMap::default(),
            next: 0,
        };
        let (result, peak) = crate::alloc_probe::peak_of(|| {
            builder.builtin_call("", "print", &[&[Slot::Const(literal.0)]], ", ", &[])
        });
        assert!(
            peak <= LIMIT,
            "builtin allocated both the 64 KiB term and call before refusing (peak={peak})"
        );
        assert!(BudgetFailure::from_anyhow(&result.unwrap_err()).is_some());
    }

    #[test]
    fn proof_leaf_classification_keeps_large_delta_object_within_budget() {
        let store = sparkles_core::store::Store::in_memory(Default::default());
        let mut tx = store.write();
        let subject = tx
            .intern(&oxrdf::NamedNode::new_unchecked("urn:s").into())
            .unwrap();
        let predicate = tx
            .intern(&oxrdf::NamedNode::new_unchecked("urn:p").into())
            .unwrap();
        let value = "z".repeat(65_536);
        let literal = tx
            .intern(&oxrdf::Literal::new_simple_literal(value.clone()).into())
            .unwrap();
        tx.insert([subject, predicate, literal, Id::DEFAULT_GRAPH])
            .unwrap();
        tx.commit().unwrap();
        assert_eq!(literal.tag(), Tag::Delta);
        let snap = store.snapshot();
        let terms = Terms::new(snap.clone());
        let rs = RuleSet {
            rules: &[],
            infos: &[],
            sources: &[],
            geo: None,
            relations: None,
        };
        let mut graph = Graph::with_capacity(0);
        let plans = Plans::new(&mut graph, &[]).unwrap();
        const LIMIT: usize = 96 * 1024;
        let budget = Budget::new(Some(LIMIT));
        let search = Search {
            snap: &snap,
            g: &graph,
            terms: &terms,
            rs: &rs,
            plans: &plans,
            inputs: &[Id::DEFAULT_GRAPH],
            extra: &FxHashSet::default(),
            budget: &budget,
            opts: &ExplainOptions::default(),
            rounds: &[],
            abort: AtomicBool::new(false),
            facts: vec![[subject.0, predicate.0, literal.0]],
            index: FxHashMap::default(),
            leaf: vec![Some(Leaf::Graph(Id::DEFAULT_GRAPH))],
            by_head: vec![Vec::new()],
            insts: Vec::new(),
            premises: Vec::new(),
            binds: Vec::new(),
            visited: 0,
            depth: vec![0],
            via: vec![NONE],
            kept: 0,
            full_schedule: None,
            _full_schedule_allocation: None,
        };
        let mut builder = Builder {
            s: &search,
            memory: Held::new(&budget),
            max_depth: 1,
            ids: FxHashMap::default(),
            next: 0,
        };
        let (result, peak) = crate::alloc_probe::peak_of(|| builder.node(0, 0, None));
        let proof = result.expect("valid leaf with one 64 KiB delta literal fits in 96 KiB");
        assert!(
            peak <= LIMIT,
            "leaf classification copied the 64 KiB delta key while its literal was live (peak={peak})"
        );
        assert!(!proof.generalized);
        assert_eq!(proof.kind, DerivationKind::Asserted);
        assert_eq!(
            proof.triple[2],
            Term::Literal(oxrdf::Literal::new_simple_literal(value))
        );
        drop(proof);
        drop(builder);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn explanation_root_lookup_admits_fsst_scratch_and_polls_controls() {
        use sparkles_core::{
            builder::{BuildOptions, VocabCodec},
            io::{RdfFormat, Source},
            store::{Store, StoreOptions},
        };

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            &dir.path().join("db"),
            StoreOptions {
                build: BuildOptions {
                    vocab_codec: Some(VocabCodec::Fsst),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        let long = "z".repeat(65_536);
        let mut data = format!(r#"@prefix ex: <http://ex.org/> . ex:s ex:p "a", "{long}""#);
        for i in 2..sparkles_core::vocab::FC_BLOCK {
            data.push_str(&format!(r#", "~filler{i}""#));
        }
        data.push_str(" .");
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        store.compact().unwrap();
        let snap = store.snapshot();
        assert!(snap.generation.vocab.is_fsst());
        assert_eq!(
            snap.generation.vocab.find(&sparkles_core::id::term_key(
                &oxrdf::Literal::new_simple_literal(long).into()
            )),
            Ok(1)
        );

        let ex = |local: &str| {
            Term::NamedNode(oxrdf::NamedNode::new(format!("http://ex.org/{local}")).unwrap())
        };
        let lit = |s: &str| Term::Literal(oxrdf::Literal::new_simple_literal(s));
        // Use "0fresh" (sorts before uncompressed entry 0 "a") so compiling the rule
        // itself does not scan entry 1; only the tested root lookups do.
        let profile = Profile::Rules(
            "@prefix ex: <http://ex.org/> . \
             [derive: (?x ex:p ?y) -> (?x ex:q ?y)] \
             [fresh: (?x ex:p \"a\") -> (?x ex:derived \"0fresh\")]"
                .into(),
        );
        let extras = Extras::default();

        // Measure the budget used by materialized setup before root lookup, then leave 8 KiB.
        let setup_budget = Budget::unlimited();
        let base = Arc::new(Base::new(snap.clone(), &[Id::DEFAULT_GRAPH]).unwrap());
        let mut g = Graph::with_base(base);
        g.set_budget(&setup_budget).unwrap();
        let setup_terms = Terms::new_in(snap.clone(), &setup_budget).unwrap();
        let mut warnings = WorkingWarnings::new(&setup_budget);
        let parsed = crate::profile_rules(&profile, &extras).unwrap();
        let _compiled = engine::compile_info(&parsed, &setup_terms, &mut warnings, false).unwrap();
        let mat_limit = setup_budget.used() + 8192;

        let mat_input = ExplainInput {
            snap: snap.clone(),
            profile: &profile,
            extras: &extras,
            inputs: &[Id::DEFAULT_GRAPH],
            inferred: None,
        };
        // Entry 0 ("a") is uncompressed; measure baseline peak without scanning entry 1.
        let (_, mat_baseline) = crate::alloc_probe::peak_of(|| {
            explain(
                &mat_input,
                &[ex("s"), ex("p"), lit("a")],
                &ExplainOptions::default(),
            )
            .unwrap()
        });
        let mat_opts = ExplainOptions {
            max_memory: Some(mat_limit),
            ..Default::default()
        };
        // 1. Materialized strict reasoner route on absent short literal "b".
        let (res, peak) = crate::alloc_probe::peak_of(|| {
            explain(&mat_input, &[ex("s"), ex("p"), lit("b")], &mat_opts)
        });
        assert!(
            peak < mat_baseline + 16_384,
            "materialized root miss allocated 64 KiB FSST suffix (peak={peak}, baseline={mat_baseline})"
        );
        assert!(
            res.unwrap_err()
                .downcast_ref::<ExplainBudgetExceeded>()
                .is_some()
        );
        // 2. Materialized strict reasoner route on present short filler sorting after 64 KiB literal.
        let (res, peak) = crate::alloc_probe::peak_of(|| {
            explain(&mat_input, &[ex("s"), ex("p"), lit("~filler2")], &mat_opts)
        });
        assert!(
            peak < mat_baseline + 16_384,
            "materialized root hit allocated 64 KiB FSST suffix (peak={peak}, baseline={mat_baseline})"
        );
        assert!(
            res.unwrap_err()
                .downcast_ref::<ExplainBudgetExceeded>()
                .is_some()
        );

        // 3. Demand and why-not routes with 8 KiB left after demand session setup.
        let reasoner =
            QueryReasoner::new(profile.clone(), &extras, crate::Inputs::default()).unwrap();
        let demand_input = DemandInput {
            reasoner: &reasoner,
            snap: snap.clone(),
            readable: None,
        };
        let probe_session =
            demand_session(&demand_input, &ExplainOptions::default(), true).unwrap();
        let demand_limit = probe_session.budget().used() + 8192;
        drop(probe_session);
        let (_, demand_baseline) = crate::alloc_probe::peak_of(|| {
            explain_demand(
                &demand_input,
                &[ex("s"), ex("absent"), lit("a")],
                &ExplainOptions::default(),
            )
            .unwrap()
        });
        let demand_opts = ExplainOptions {
            max_memory: Some(demand_limit),
            ..Default::default()
        };
        let (res, peak) = crate::alloc_probe::peak_of(|| {
            explain_demand(&demand_input, &[ex("s"), ex("q"), lit("b")], &demand_opts)
        });
        assert!(
            peak < demand_baseline + 16_384,
            "demand root miss allocated 64 KiB FSST suffix (peak={peak}, baseline={demand_baseline})"
        );
        assert!(
            res.unwrap_err()
                .downcast_ref::<ExplainBudgetExceeded>()
                .is_some()
        );
        let (res, peak) = crate::alloc_probe::peak_of(|| {
            explain_demand(
                &demand_input,
                &[ex("s"), ex("q"), lit("~filler2")],
                &demand_opts,
            )
        });
        assert!(
            peak < demand_baseline + 16_384,
            "demand root hit allocated 64 KiB FSST suffix (peak={peak}, baseline={demand_baseline})"
        );
        assert!(
            res.unwrap_err()
                .downcast_ref::<ExplainBudgetExceeded>()
                .is_some()
        );

        let whynot_opts = crate::whynot::WhyNotOptions {
            explain: demand_opts,
            ..Default::default()
        };
        let (res, peak) = crate::alloc_probe::peak_of(|| {
            crate::whynot::why_not(&demand_input, &[ex("s"), ex("q"), lit("b")], &whynot_opts)
        });
        assert!(
            peak < demand_baseline + 16_384,
            "why-not root miss allocated 64 KiB FSST suffix (peak={peak}, baseline={demand_baseline})"
        );
        assert!(
            res.unwrap_err()
                .downcast_ref::<ExplainBudgetExceeded>()
                .is_some()
        );

        // Local-interning behavior on misses is preserved when budget allows.
        let found = explain_demand(
            &demand_input,
            &[ex("s"), ex("derived"), lit("0fresh")],
            &ExplainOptions::default(),
        )
        .unwrap();
        assert!(matches!(found, Explained::Found { .. }));
        let wn = crate::whynot::why_not(
            &demand_input,
            &[ex("s"), ex("q"), lit("b")],
            &crate::whynot::WhyNotOptions::default(),
        )
        .unwrap();
        assert!(!matches!(wn, crate::whynot::WhyNot::Entailed));

        // Direct strict term lookup with 8 KiB left after Terms::new_in refuses before
        // allocating the 64 KiB FSST suffix, and polls live cancellation/deadline controls.
        let direct_probe = Budget::unlimited();
        let _direct_setup = Terms::new_in(snap.clone(), &direct_probe).unwrap();
        let direct_limit = direct_probe.used() + 8192;
        let direct_budget = Budget::new(Some(direct_limit));
        let direct_terms = Terms::new_in(snap.clone(), &direct_budget).unwrap();
        for term in [lit("b"), lit("~filler2")] {
            let (res, peak) = crate::alloc_probe::peak_of(|| {
                direct_terms.try_id_for_controlled(&term, || Ok(()))
            });
            assert!(
                peak <= 8192,
                "direct strict lookup for {term} allocated oversized FSST scratch (peak={peak})"
            );
            assert!(BudgetFailure::from_anyhow(&res.unwrap_err()).is_some());
        }
        let mid_budget = Budget::unlimited();
        let mid_terms = Terms::new_in(snap.clone(), &mid_budget).unwrap();
        let polls = Cell::new(0usize);
        let (stopped, peak) = crate::alloc_probe::peak_of(|| {
            mid_terms.try_id_for_controlled(&lit("b"), || {
                let n = polls.get() + 1;
                polls.set(n);
                if n >= 8 {
                    Err(anyhow::Error::new(sparkles_core::Error::Cancelled))
                } else {
                    Ok(())
                }
            })
        });
        assert!(
            peak <= 8192,
            "mid-scan cancellation should stop before 64 KiB allocation (peak={peak})"
        );
        assert!(
            stopped
                .unwrap_err()
                .downcast_ref::<sparkles_core::Error>()
                .is_some_and(|e| matches!(e, sparkles_core::Error::Cancelled))
        );
        let expired_opts = ExplainOptions {
            deadline: Some(Instant::now() - std::time::Duration::from_secs(1)),
            ..Default::default()
        };
        assert!(
            explain(&mat_input, &[ex("s"), ex("p"), lit("b")], &expired_opts)
                .unwrap_err()
                .downcast_ref::<ExplainBudgetExceeded>()
                .is_some()
        );
        let cancel_opts = ExplainOptions {
            cancel: Some(Arc::new(AtomicBool::new(true))),
            ..Default::default()
        };
        assert!(
            explain(&mat_input, &[ex("s"), ex("p"), lit("b")], &cancel_opts)
                .unwrap_err()
                .downcast_ref::<sparkles_core::Error>()
                .is_some_and(|e| matches!(e, sparkles_core::Error::Cancelled))
        );
    }

    #[test]
    fn explanation_rule_constant_lookup_admits_fsst_scratch_and_polls_controls() {
        use sparkles_core::{
            builder::{BuildOptions, VocabCodec},
            io::{RdfFormat, Source},
            store::{Store, StoreOptions},
        };

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            &dir.path().join("db"),
            StoreOptions {
                build: BuildOptions {
                    vocab_codec: Some(VocabCodec::Fsst),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        let long = "z".repeat(65_536);
        let mut data = format!(r#"@prefix ex: <http://ex.org/> . ex:s ex:p "a", "{long}""#);
        for i in 2..sparkles_core::vocab::FC_BLOCK {
            data.push_str(&format!(r#", "~filler{i}""#));
        }
        data.push_str(" .");
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        store.compact().unwrap();
        let snap = store.snapshot();
        assert!(snap.generation.vocab.is_fsst());

        let ex = |local: &str| {
            Term::NamedNode(oxrdf::NamedNode::new(format!("http://ex.org/{local}")).unwrap())
        };
        let lit = |s: &str| Term::Literal(oxrdf::Literal::new_simple_literal(s));
        let extras = Extras::default();

        // Baseline rules use "a" (uncompressed entry 0) and "0fresh" (sorts before entry 0),
        // so baseline compilation never scans entry 1; target rules use "b", whose
        // three-byte literal probe shares the tag with "a" and reaches the 64 KiB entry 1.
        let jena_a = Profile::Rules(
            "@prefix ex: <http://ex.org/> . \
             [derive: (?x ex:p ?y) -> (?x ex:q ?y)] \
             [fresh: (?x ex:p \"a\") -> (?x ex:derived \"0fresh\")]"
                .into(),
        );
        let jena_b = Profile::Rules(
            "@prefix ex: <http://ex.org/> . \
             [derive: (?x ex:p ?y) -> (?x ex:q ?y)] \
             [fresh: (?x ex:p \"b\") -> (?x ex:derived \"0fresh\")]"
                .into(),
        );
        let prog_a = Profile::Rules(
            "@prefix ex: <http://ex.org/> . \
             [rel: (?x ex:p \"a\") -> r(?x)] \
             [out: r(?x) -> (?x ex:q \"a\")]"
                .into(),
        );
        let prog_b = Profile::Rules(
            "@prefix ex: <http://ex.org/> . \
             [rel: (?x ex:p \"b\") -> r(?x)] \
             [out: r(?x) -> (?x ex:q \"b\")]"
                .into(),
        );

        let setup_budget = Budget::unlimited();
        let base = Arc::new(Base::new(snap.clone(), &[Id::DEFAULT_GRAPH]).unwrap());
        let mut g = Graph::with_base(base);
        g.set_budget(&setup_budget).unwrap();
        let setup_terms = Terms::new_in(snap.clone(), &setup_budget).unwrap();
        let mut warnings = WorkingWarnings::new(&setup_budget);
        let parsed_a = crate::profile_rules(&jena_a, &extras).unwrap();
        let _compiled_a =
            engine::compile_info(&parsed_a, &setup_terms, &mut warnings, false).unwrap();
        let mat_limit = setup_budget.used() + 8192;

        let mat_a_input = ExplainInput {
            snap: snap.clone(),
            profile: &jena_a,
            extras: &extras,
            inputs: &[Id::DEFAULT_GRAPH],
            inferred: None,
        };
        let mat_b_input = ExplainInput {
            snap: snap.clone(),
            profile: &jena_b,
            extras: &extras,
            inputs: &[Id::DEFAULT_GRAPH],
            inferred: None,
        };
        let (_, mat_baseline) = crate::alloc_probe::peak_of(|| {
            explain(
                &mat_a_input,
                &[ex("s"), ex("p"), lit("a")],
                &ExplainOptions::default(),
            )
            .unwrap()
        });
        let mat_opts = ExplainOptions {
            max_memory: Some(mat_limit),
            ..Default::default()
        };
        for root_obj in [lit("b"), lit("a")] {
            let (res, peak, largest) = crate::alloc_probe::peak_and_largest_of(|| {
                explain(
                    &mat_b_input,
                    &[ex("s"), ex("p"), root_obj.clone()],
                    &mat_opts,
                )
            });
            assert!(
                peak < mat_baseline + 16_384 && largest < 65_536,
                "materialized rule constant \"b\" with root {root_obj} allocated 64 KiB FSST suffix (peak={peak}, baseline={mat_baseline}, largest={largest})"
            );
            assert!(
                res.unwrap_err()
                    .downcast_ref::<ExplainBudgetExceeded>()
                    .is_some()
            );
        }

        // Program rules (uses_program == true) in materialized program_search.
        let prog_a_input = ExplainInput {
            snap: snap.clone(),
            profile: &prog_a,
            extras: &extras,
            inputs: &[Id::DEFAULT_GRAPH],
            inferred: None,
        };
        let prog_b_input = ExplainInput {
            snap: snap.clone(),
            profile: &prog_b,
            extras: &extras,
            inputs: &[Id::DEFAULT_GRAPH],
            inferred: None,
        };
        let (_, prog_baseline) = crate::alloc_probe::peak_of(|| {
            explain(
                &prog_a_input,
                &[ex("s"), ex("p"), lit("a")],
                &ExplainOptions::default(),
            )
            .unwrap()
        });
        let prog_opts = ExplainOptions {
            max_memory: Some(mat_limit + 4096),
            ..Default::default()
        };
        for root_obj in [lit("b"), lit("a")] {
            let (res, peak, largest) = crate::alloc_probe::peak_and_largest_of(|| {
                explain(
                    &prog_b_input,
                    &[ex("s"), ex("p"), root_obj.clone()],
                    &prog_opts,
                )
            });
            assert!(
                peak < prog_baseline + 16_384 && largest < 65_536,
                "program rule constant \"b\" with root {root_obj} allocated 64 KiB FSST suffix (peak={peak}, baseline={prog_baseline}, largest={largest})"
            );
            assert!(
                res.unwrap_err()
                    .downcast_ref::<ExplainBudgetExceeded>()
                    .is_some()
            );
        }

        // Demand and why-not sessions compile rules in Session::new before root lookup.
        let reasoner_a =
            QueryReasoner::new(jena_a.clone(), &extras, crate::Inputs::default()).unwrap();
        let demand_a_input = DemandInput {
            reasoner: &reasoner_a,
            snap: snap.clone(),
            readable: None,
        };
        let probe_session =
            demand_session(&demand_a_input, &ExplainOptions::default(), true).unwrap();
        let demand_limit = probe_session.budget().used() + 8192;
        drop(probe_session);
        let (_, demand_baseline) = crate::alloc_probe::peak_of(|| {
            explain_demand(
                &demand_a_input,
                &[ex("s"), ex("absent"), lit("a")],
                &ExplainOptions::default(),
            )
            .unwrap()
        });
        let reasoner_b =
            QueryReasoner::new(jena_b.clone(), &extras, crate::Inputs::default()).unwrap();
        let demand_b_input = DemandInput {
            reasoner: &reasoner_b,
            snap: snap.clone(),
            readable: None,
        };
        let demand_opts = ExplainOptions {
            max_memory: Some(demand_limit),
            ..Default::default()
        };
        for root_obj in [lit("b"), lit("a")] {
            let (res, peak, largest) = crate::alloc_probe::peak_and_largest_of(|| {
                explain_demand(
                    &demand_b_input,
                    &[ex("s"), ex("q"), root_obj.clone()],
                    &demand_opts,
                )
            });
            assert!(
                peak < demand_baseline + 16_384 && largest < 65_536,
                "demand rule constant \"b\" with root {root_obj} allocated 64 KiB FSST suffix (peak={peak}, baseline={demand_baseline}, largest={largest})"
            );
            assert!(
                res.unwrap_err()
                    .downcast_ref::<ExplainBudgetExceeded>()
                    .is_some()
            );
        }

        let whynot_opts = crate::whynot::WhyNotOptions {
            explain: demand_opts.clone(),
            ..Default::default()
        };
        let (res, peak, largest) = crate::alloc_probe::peak_and_largest_of(|| {
            crate::whynot::why_not(&demand_b_input, &[ex("s"), ex("q"), lit("b")], &whynot_opts)
        });
        assert!(
            peak < demand_baseline + 16_384 && largest < 65_536,
            "why-not rule constant \"b\" allocated 64 KiB FSST suffix (peak={peak}, baseline={demand_baseline}, largest={largest})"
        );
        assert!(
            res.unwrap_err()
                .downcast_ref::<ExplainBudgetExceeded>()
                .is_some()
        );

        // Demand program rules (uses_program == true) in Session::new -> program_rules.
        let reasoner_prog_a =
            QueryReasoner::new(prog_a, &extras, crate::Inputs::default()).unwrap();
        let demand_prog_a = DemandInput {
            reasoner: &reasoner_prog_a,
            snap: snap.clone(),
            readable: None,
        };
        let probe_prog = demand_session(&demand_prog_a, &ExplainOptions::default(), true).unwrap();
        let demand_prog_limit = probe_prog.budget().used() + 8192;
        drop(probe_prog);
        let (_, demand_prog_baseline) = crate::alloc_probe::peak_of(|| {
            explain_demand(
                &demand_prog_a,
                &[ex("s"), ex("absent"), lit("a")],
                &ExplainOptions::default(),
            )
            .unwrap()
        });
        let reasoner_prog_b =
            QueryReasoner::new(prog_b.clone(), &extras, crate::Inputs::default()).unwrap();
        let demand_prog_b = DemandInput {
            reasoner: &reasoner_prog_b,
            snap: snap.clone(),
            readable: None,
        };
        let demand_prog_opts = ExplainOptions {
            max_memory: Some(demand_prog_limit),
            ..Default::default()
        };
        let (res, peak, largest) = crate::alloc_probe::peak_and_largest_of(|| {
            explain_demand(
                &demand_prog_b,
                &[ex("s"), ex("q"), lit("b")],
                &demand_prog_opts,
            )
        });
        assert!(
            peak < demand_prog_baseline + 16_384 && largest < 65_536,
            "demand program constant \"b\" allocated 64 KiB FSST suffix (peak={peak}, baseline={demand_prog_baseline}, largest={largest})"
        );
        assert!(
            res.unwrap_err()
                .downcast_ref::<ExplainBudgetExceeded>()
                .is_some()
        );

        // Cancellation and expired deadline stop before rule/program compilation allocates
        // the 64 KiB FSST suffix.
        let cancel_opts = ExplainOptions {
            cancel: Some(Arc::new(AtomicBool::new(true))),
            ..Default::default()
        };
        let expired_opts = ExplainOptions {
            deadline: Some(Instant::now() - std::time::Duration::from_secs(1)),
            ..Default::default()
        };
        for (label, input, baseline) in [
            ("materialized", &mat_b_input, mat_baseline),
            ("program", &prog_b_input, prog_baseline),
        ] {
            let (res, peak) = crate::alloc_probe::peak_of(|| {
                explain(input, &[ex("s"), ex("p"), lit("a")], &cancel_opts)
            });
            assert!(
                peak < baseline + 16_384,
                "{label} cancel allocated 64 KiB FSST suffix (peak={peak}, baseline={baseline})"
            );
            assert!(
                res.unwrap_err()
                    .downcast_ref::<sparkles_core::Error>()
                    .is_some_and(|e| matches!(e, sparkles_core::Error::Cancelled))
            );

            let (res, peak) = crate::alloc_probe::peak_of(|| {
                explain(input, &[ex("s"), ex("p"), lit("a")], &expired_opts)
            });
            assert!(
                peak < baseline + 16_384,
                "{label} expired deadline allocated 64 KiB FSST suffix (peak={peak}, baseline={baseline})"
            );
            assert!(
                res.unwrap_err()
                    .downcast_ref::<ExplainBudgetExceeded>()
                    .is_some()
            );
        }

        // Mid-scan cancellation stops inside controlled Jena and program compilation
        // before allocating the 64 KiB FSST suffix.
        let parsed_b = crate::profile_rules(&jena_b, &extras).unwrap();
        let parsed_prog_b = crate::profile_rules(&prog_b, &extras).unwrap();
        let mid_budget = Budget::unlimited();
        let mid_terms = Terms::new_in(snap.clone(), &mid_budget).unwrap();
        let mut mid_warnings = WorkingWarnings::new(&mid_budget);
        let polls = Cell::new(0usize);
        let (res, peak) = crate::alloc_probe::peak_of(|| {
            engine::compile_info_controlled(
                &parsed_b,
                &mid_terms,
                &mut mid_warnings,
                false,
                &mut || {
                    let n = polls.get() + 1;
                    polls.set(n);
                    if n >= 16 {
                        Err(anyhow::Error::new(sparkles_core::Error::Cancelled))
                    } else {
                        Ok(())
                    }
                },
            )
        });
        assert!(
            peak <= 16_384,
            "mid-compile Jena cancellation allocated 64 KiB FSST suffix (peak={peak})"
        );
        assert!(
            res.err()
                .expect("mid-compile cancellation")
                .downcast_ref::<sparkles_core::Error>()
                .is_some_and(|e| matches!(e, sparkles_core::Error::Cancelled))
        );
        let polls = Cell::new(0usize);
        let (res, peak) = crate::alloc_probe::peak_of(|| {
            crate::engine::frontend::compile_controlled(
                &parsed_prog_b,
                &mid_terms,
                &mut mid_warnings,
                false,
                &mut || {
                    let n = polls.get() + 1;
                    polls.set(n);
                    if n >= 16 {
                        Err(anyhow::Error::new(sparkles_core::Error::Cancelled))
                    } else {
                        Ok(())
                    }
                },
            )
        });
        assert!(
            peak <= 16_384,
            "mid-compile program cancellation allocated 64 KiB FSST suffix (peak={peak})"
        );
        assert!(
            res.err()
                .expect("mid-compile program cancellation")
                .downcast_ref::<sparkles_core::Error>()
                .is_some_and(|e| matches!(e, sparkles_core::Error::Cancelled))
        );

        // Controlled compilation preserves rule blank-node handling without interpreting
        // encoded response labels such as `spkrule_62` (hex for "b").
        let bnode_terms = Terms::new_in(snap.clone(), &Budget::unlimited()).unwrap();
        let plain_bnode = bnode_terms
            .try_id_for_rule_controlled(
                &Term::BlankNode(oxrdf::BlankNode::new_unchecked("b")),
                || Ok(()),
            )
            .unwrap();
        let encoded_like_bnode = bnode_terms
            .try_id_for_rule_controlled(
                &Term::BlankNode(oxrdf::BlankNode::new_unchecked("spkrule_62")),
                || Ok(()),
            )
            .unwrap();
        assert!(bnode_terms.is_rule_bnode(plain_bnode));
        assert!(bnode_terms.is_rule_bnode(encoded_like_bnode));
        assert_ne!(plain_bnode, encoded_like_bnode);
        // By contrast, general response-side lookup decodes `spkrule_62` back to `_:b`.
        let decoded_bnode = bnode_terms
            .try_id_for_controlled(
                &Term::BlankNode(oxrdf::BlankNode::new_unchecked("spkrule_62")),
                || Ok(()),
            )
            .unwrap();
        assert_eq!(decoded_bnode, plain_bnode);
    }

    #[test]
    fn explanation_terms_init_avoids_fsst_literal_boundary_decode() {
        use sparkles_core::{
            builder::{BuildOptions, VocabCodec},
            io::{RdfFormat, Source},
            store::{Store, StoreOptions},
        };

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            &dir.path().join("db"),
            StoreOptions {
                build: BuildOptions {
                    vocab_codec: Some(VocabCodec::Fsst),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        // Block 0 holds one short literal "a" at entry 0, immediately followed by a
        // compressible 64 KiB first IRI at entry 1 and short <http://b.org/...> fillers
        // filling the rest of block 0 so <http://ex.org/*> sits in block 1+.
        // Add enough <http://m.org/...> IRIs to cross vocabmerge::SAMPLE_EVERY (4096) so
        // the FSST IRI table (kind 1) is trained and compresses the 64 KiB IRI at entry 1.
        let long = "z".repeat(65_536);
        let mut data =
            format!(r#"@prefix ex: <http://ex.org/> . ex:s ex:p "a", <http://a.org/{long}>"#);
        for i in 2..sparkles_core::vocab::FC_BLOCK {
            data.push_str(&format!(", <http://b.org/f{i:02}>"));
        }
        let sample_z = "z".repeat(256);
        for i in 0..4112 {
            data.push_str(&format!(", <http://m.org/k{i:04}_{sample_z}>"));
        }
        data.push_str(" .");
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        store.compact().unwrap();
        let snap = store.snapshot();
        assert!(snap.generation.vocab.is_fsst());
        assert_eq!(snap.generation.vocab.first_triple(), 1);

        let ex = |local: &str| {
            Term::NamedNode(oxrdf::NamedNode::new(format!("http://ex.org/{local}")).unwrap())
        };
        let lit = |s: &str| Term::Literal(oxrdf::Literal::new_simple_literal(s));
        let extras = Extras::default();
        let jena = Profile::Rules(
            "@prefix ex: <http://ex.org/> . [derive: (?x ex:p ?y) -> (?x ex:q ?y)]".into(),
        );
        let prog = Profile::Rules(
            "@prefix ex: <http://ex.org/> . \
             [rel: (?x ex:p \"a\") -> r(?x)] \
             [out: r(?x) -> (?x ex:q \"a\")]"
                .into(),
        );

        let measure_fresh = |f: Box<dyn FnOnce() -> anyhow::Result<bool> + Send + '_>| {
            std::thread::scope(|scope| {
                scope
                    .spawn(move || crate::alloc_probe::peak_and_largest_of(f))
                    .join()
                    .unwrap()
            })
        };

        let mat_jena_input = ExplainInput {
            snap: snap.clone(),
            profile: &jena,
            extras: &extras,
            inputs: &[Id::DEFAULT_GRAPH],
            inferred: None,
        };
        let (res, peak, largest) = measure_fresh(Box::new(|| {
            explain(
                &mat_jena_input,
                &[ex("s"), ex("p"), lit("a")],
                &ExplainOptions::default(),
            )
            .map(|_| true)
        }));
        assert!(res.is_ok(), "materialized Jena failed: {res:?}");
        assert!(
            largest < 65_536,
            "materialized Jena Terms init decoded 64 KiB boundary IRI (peak={peak}, largest={largest})"
        );

        let mat_prog_input = ExplainInput {
            snap: snap.clone(),
            profile: &prog,
            extras: &extras,
            inputs: &[Id::DEFAULT_GRAPH],
            inferred: None,
        };
        let (res, peak, largest) = measure_fresh(Box::new(|| {
            explain(
                &mat_prog_input,
                &[ex("s"), ex("p"), lit("a")],
                &ExplainOptions::default(),
            )
            .map(|_| true)
        }));
        assert!(res.is_ok(), "materialized program failed: {res:?}");
        assert!(
            largest < 65_536,
            "materialized program Terms init decoded 64 KiB boundary IRI (peak={peak}, largest={largest})"
        );

        let reasoner = QueryReasoner::new(jena, &extras, crate::Inputs::default()).unwrap();
        let demand_input = DemandInput {
            reasoner: &reasoner,
            snap: snap.clone(),
            readable: None,
        };
        let (res, peak, largest) = measure_fresh(Box::new(|| {
            explain_demand(
                &demand_input,
                &[ex("s"), ex("q"), lit("a")],
                &ExplainOptions::default(),
            )
            .map(|_| true)
        }));
        assert!(res.is_ok(), "demand explain failed: {res:?}");
        assert!(
            largest < 65_536,
            "demand Terms init decoded 64 KiB boundary IRI (peak={peak}, largest={largest})"
        );

        let (res, peak, largest) = measure_fresh(Box::new(|| {
            crate::whynot::why_not(
                &demand_input,
                &[ex("s"), ex("zabsent"), lit("a")],
                &crate::whynot::WhyNotOptions::default(),
            )
            .map(|_| true)
        }));
        assert!(res.is_ok(), "why-not failed: {res:?}");
        assert!(
            largest < 65_536,
            "why-not Terms init decoded 64 KiB boundary IRI (peak={peak}, largest={largest})"
        );
    }

    #[test]
    fn explanation_imports_and_named_graph_admit_fsst_scratch() {
        use sparkles_core::{
            builder::{BuildOptions, VocabCodec},
            io::{RdfFormat, Source},
            store::{Store, StoreOptions},
        };

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            &dir.path().join("db"),
            StoreOptions {
                build: BuildOptions {
                    vocab_codec: Some(VocabCodec::Fsst),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        let long = "z".repeat(65_536);
        let fc = sparkles_core::vocab::FC_BLOCK;
        // Zero literals so rank 0 is an IRI and the FSST IRI table trains directly.
        // Block 0: rdf:first/nil/rest/type + fillers so Terms::new_in_controlled succeeds
        //          without touching any 64 KiB block.
        // Block 1: owl:a, 64 KiB owl:b{long}, owl:imports + owl:j* fillers.
        // Block 2: short owl:r* fillers so owl:sameAs probes stay out of Block 1.
        // Block 3: x:a, 64 KiB x:b{long}, x:g (named input graph), x:imp (imported graph) + fillers.
        // Block 4+: z:* used by rules and target triple.
        let mut data = String::from(
            "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
             @prefix owl: <http://www.w3.org/2002/07/owl#> .\n\
             @prefix x: <http://x.org/> .\n\
             @prefix z: <http://z.org/> .\n\
             {\n  z:s z:p z:o, rdf:first, rdf:nil, rdf:rest, rdf:type",
        );
        for i in 0..(fc - 4) {
            data.push_str(&format!(
                ", <http://www.w3.org/1999/02/22-rdf-syntax-ns#z{i:02}>"
            ));
        }
        data.push_str(&format!(", owl:a, <http://www.w3.org/2002/07/owl#b{long}>"));
        for i in 0..(fc - 3) {
            data.push_str(&format!(", <http://www.w3.org/2002/07/owl#j{i:02}>"));
        }
        for i in 0..fc {
            data.push_str(&format!(", <http://www.w3.org/2002/07/owl#r{i:02}>"));
        }
        data.push_str(&format!(", x:a, <http://x.org/b{long}>"));
        for i in 0..(fc - 4) {
            data.push_str(&format!(", <http://x.org/z{i:02}>"));
        }
        data.push_str(" .\n  z:s owl:imports x:imp .\n}\n");
        data.push_str("x:g { z:s z:p z:o . }\n");
        data.push_str("x:imp { z:s z:p z:o . }\n");
        store
            .load(&[Source::from_bytes(data.into_bytes(), RdfFormat::TriG, None)])
            .unwrap();
        store.compact().unwrap();
        let snap = store.snapshot();
        assert!(snap.generation.vocab.is_fsst());

        let z = |local: &str| {
            Term::NamedNode(oxrdf::NamedNode::new(format!("http://z.org/{local}")).unwrap())
        };
        let extras = Extras::default();
        let rules = Profile::Rules(
            "@prefix z: <http://z.org/> . [derive: (?x z:p ?y) -> (?x z:q ?y)]".into(),
        );
        let named_inputs = crate::Inputs {
            data_graphs: vec![crate::inputs::GraphRef::Named("http://x.org/g".into())],
            imports: crate::inputs::ImportMode::None,
            ..Default::default()
        };
        let import_inputs = crate::Inputs {
            data_graphs: vec![crate::inputs::GraphRef::Default],
            imports: crate::inputs::ImportMode::Dataset,
            ..Default::default()
        };

        for (label, inputs) in [
            ("named-graph", named_inputs),
            ("owl-imports", import_inputs),
        ] {
            let reasoner = QueryReasoner::new(rules.clone(), &extras, inputs).unwrap();
            let input = DemandInput {
                reasoner: &reasoner,
                snap: snap.clone(),
                readable: None,
            };
            let opts = ExplainOptions {
                max_memory: Some(48 * 1024),
                ..Default::default()
            };
            let (res, peak, largest) = std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        crate::alloc_probe::peak_and_largest_of(|| {
                            explain_demand(&input, &[z("s"), z("q"), z("o")], &opts)
                        })
                    })
                    .join()
                    .unwrap()
            });
            assert!(
                largest < 65_536,
                "{label} demand decoded 64 KiB FSST suffix (peak={peak}, largest={largest})"
            );
            assert!(
                res.unwrap_err()
                    .downcast_ref::<ExplainBudgetExceeded>()
                    .is_some(),
                "{label} demand expected ExplainBudgetExceeded"
            );

            let wn_opts = crate::whynot::WhyNotOptions {
                explain: opts,
                ..Default::default()
            };
            let (res, peak, largest) = std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        crate::alloc_probe::peak_and_largest_of(|| {
                            crate::whynot::why_not(&input, &[z("s"), z("q"), z("o")], &wn_opts)
                        })
                    })
                    .join()
                    .unwrap()
            });
            assert!(
                largest < 65_536,
                "{label} why-not decoded 64 KiB FSST suffix (peak={peak}, largest={largest})"
            );
            assert!(
                res.unwrap_err()
                    .downcast_ref::<ExplainBudgetExceeded>()
                    .is_some(),
                "{label} why-not expected ExplainBudgetExceeded"
            );

            let cancel_opts = ExplainOptions {
                cancel: Some(Arc::new(AtomicBool::new(true))),
                ..Default::default()
            };
            let (res, peak, largest) = std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        crate::alloc_probe::peak_and_largest_of(|| {
                            explain_demand(&input, &[z("s"), z("q"), z("o")], &cancel_opts)
                        })
                    })
                    .join()
                    .unwrap()
            });
            assert!(
                largest < 65_536,
                "{label} cancel decoded 64 KiB FSST suffix (peak={peak}, largest={largest})"
            );
            assert!(
                res.unwrap_err()
                    .downcast_ref::<sparkles_core::Error>()
                    .is_some_and(|e| matches!(e, sparkles_core::Error::Cancelled)),
                "{label} expected Cancelled"
            );
        }

        // Empty readable input set: when access denies the sole configured default graph,
        // the session must skip the owl:imports vocabulary probe rather than triggering
        // the 64 KiB FSST block decode and refusing under the 48 KiB cap.
        let import_inputs = crate::Inputs {
            data_graphs: vec![crate::inputs::GraphRef::Default],
            imports: crate::inputs::ImportMode::Dataset,
            ..Default::default()
        };
        let reasoner = QueryReasoner::new(rules, &extras, import_inputs).unwrap();
        let denied = Arc::new(sparkles_core::access::GraphAccess::graphs(
            sparkles_core::access::Graphs::none(),
            sparkles_core::access::Graphs::none(),
        ));
        let denied_input = DemandInput {
            reasoner: &reasoner,
            snap: snap.clone(),
            readable: Some(denied),
        };
        let opts_48k = ExplainOptions {
            max_memory: Some(48 * 1024),
            ..Default::default()
        };
        let (session_res, peak, largest) = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    crate::alloc_probe::peak_and_largest_of(|| {
                        demand_session(&denied_input, &opts_48k, true).map(|_| ())
                    })
                })
                .join()
                .unwrap()
        });
        assert!(
            largest < 65_536,
            "denied default graph session probed 64 KiB owl:imports FSST block (peak={peak}, largest={largest})"
        );
        assert!(
            session_res.is_ok(),
            "denied default graph session should succeed under 48 KiB cap without probing owl:imports, got {session_res:?}"
        );

        let opts_60k = ExplainOptions {
            max_memory: Some(60 * 1024),
            ..Default::default()
        };
        let (res, peak, largest) = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    crate::alloc_probe::peak_and_largest_of(|| {
                        explain_demand(&denied_input, &[z("s"), z("q"), z("o")], &opts_60k)
                    })
                })
                .join()
                .unwrap()
        });
        assert!(
            largest < 65_536,
            "denied default graph demand probed 64 KiB owl:imports FSST block (peak={peak}, largest={largest})"
        );
        assert!(
            matches!(res, Ok(Explained::NotFound)),
            "denied default graph demand should return NotFound without probing owl:imports, got {res:?}"
        );

        let wn_opts = crate::whynot::WhyNotOptions {
            explain: opts_48k,
            ..Default::default()
        };
        let (res, peak, largest) = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    crate::alloc_probe::peak_and_largest_of(|| {
                        crate::whynot::why_not(
                            &denied_input,
                            &[z("s"), z("unrelated"), z("o")],
                            &wn_opts,
                        )
                    })
                })
                .join()
                .unwrap()
        });
        assert!(
            largest < 65_536,
            "denied default graph why-not probed 64 KiB owl:imports FSST block (peak={peak}, largest={largest})"
        );
        assert!(
            matches!(res, Ok(crate::whynot::WhyNot::NotEntailed { .. })),
            "denied default graph why-not should return NotEntailed without probing owl:imports, got {res:?}"
        );
    }

    #[test]
    fn demand_vocab_classification_avoids_delta_literal_copies() {
        let store = sparkles_core::store::Store::in_memory(Default::default());
        let long = "z".repeat(65_536);
        let update = format!(
            "INSERT DATA {{ <http://ex.org/other> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \"{long}\" }}"
        );
        sparkles_core::sparql::update::update(&store, &update, &Default::default()).unwrap();
        let snap = store.snapshot();
        let long_term = Term::Literal(oxrdf::Literal::new_simple_literal(long.clone()));
        assert_eq!(
            snap.lookup_term(&long_term).unwrap().tag(),
            Tag::Delta,
            "fixture requires long literal in delta vocabulary"
        );

        let ex = |local: &str| {
            Term::NamedNode(oxrdf::NamedNode::new(format!("http://ex.org/{local}")).unwrap())
        };
        let extras = Extras::default();
        let base_rules = Profile::Rules(
            "@prefix ex: <http://ex.org/> . \
             @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> . \
             [r: (?x rdf:type \"a\") -> (?x ex:q ex:o)]"
                .into(),
        );
        let target_rules = Profile::Rules(format!(
            "@prefix ex: <http://ex.org/> . \
             @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> . \
             [r: (?x rdf:type \"{long}\") -> (?x ex:q ex:o)]"
        ));

        let base_reasoner =
            QueryReasoner::new(base_rules, &extras, crate::Inputs::default()).unwrap();
        let base_input = DemandInput {
            reasoner: &base_reasoner,
            snap: snap.clone(),
            readable: None,
        };
        let probe = demand_session(&base_input, &ExplainOptions::default(), true).unwrap();
        let base_used = probe.budget().used();
        drop(probe);

        // Cap fits the single 64 KiB encoded compilation probe plus session overhead,
        // while excluding two simultaneous 64 KiB owned copies (delta key + lexical String).
        let cap = base_used + 80 * 1024;
        assert!(cap < base_used + 2 * 65_536);

        let target_reasoner =
            QueryReasoner::new(target_rules, &extras, crate::Inputs::default()).unwrap();
        let target_input = DemandInput {
            reasoner: &target_reasoner,
            snap: snap.clone(),
            readable: None,
        };
        let opts = ExplainOptions {
            max_memory: Some(cap),
            ..Default::default()
        };

        // Direct derive_ids check after Session::new drops its compilation probe:
        // demand::Vocab::of must allocate zero 64 KiB copies of the delta literal.
        let mut session = demand_session(&target_input, &opts, true).unwrap();
        let target_ids = [
            session.terms().try_id_for(&ex("s")).unwrap(),
            session.terms().try_id_for(&ex("q")).unwrap(),
            session.terms().try_id_for(&ex("o")).unwrap(),
        ];
        let (res, peak, largest) = crate::alloc_probe::peak_and_largest_of(|| {
            session.derive_ids(&[[
                Some(target_ids[0]),
                Some(target_ids[1]),
                Some(target_ids[2]),
            ]])
        });
        assert!(res.is_ok(), "derive_ids failed: {res:?}");
        assert!(
            largest < 32_768 && peak < 65_536,
            "derive_ids copied delta literal during Vocab::of (peak={peak}, largest={largest})"
        );
        drop(session);

        let (res, peak, largest) = crate::alloc_probe::peak_and_largest_of(|| {
            explain_demand(&target_input, &[ex("s"), ex("q"), ex("o")], &opts)
        });
        assert!(res.is_ok(), "explain_demand failed: {res:?}");
        assert!(
            peak < base_used + 112 * 1024,
            "explain_demand spiked to two live copies of delta literal (peak={peak}, cap={cap}, largest={largest})"
        );

        let wn_opts = crate::whynot::WhyNotOptions {
            explain: opts,
            ..Default::default()
        };
        let (res, peak, largest) = crate::alloc_probe::peak_and_largest_of(|| {
            crate::whynot::why_not(
                &target_input,
                &[ex("s"), ex("unrelated"), ex("o")],
                &wn_opts,
            )
        });
        assert!(res.is_ok(), "why_not failed: {res:?}");
        assert!(
            peak < base_used + 112 * 1024,
            "why_not spiked to two live copies of delta literal (peak={peak}, cap={cap}, largest={largest})"
        );

        // Verify demand::Vocab::of_controlled polls live controls without copying delta keys.
        let parsed = crate::profile_rules(
            &Profile::Rules(format!(
                "@prefix ex: <http://ex.org/> . \
             @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> . \
             [r: (?x rdf:type \"{long}\") -> (?x ex:q ex:o)]"
            )),
            &extras,
        )
        .unwrap();
        let ctrl_budget = Budget::unlimited();
        let ctrl_terms = Terms::new_in(snap.clone(), &ctrl_budget).unwrap();
        let mut ctrl_warnings = WorkingWarnings::new(&ctrl_budget);
        let compiled =
            engine::compile_info(&parsed, &ctrl_terms, &mut ctrl_warnings, false).unwrap();
        let polls = Cell::new(0usize);
        let (cancelled, peak, largest) = crate::alloc_probe::peak_and_largest_of(|| {
            crate::demand::Vocab::of_controlled(compiled.rules(), &ctrl_terms, || {
                let n = polls.get() + 1;
                polls.set(n);
                if n >= 2 {
                    Err(anyhow::Error::new(sparkles_core::Error::Cancelled))
                } else {
                    Ok(())
                }
            })
        });
        assert!(
            largest < 16_384 && peak < 16_384,
            "Vocab::of_controlled allocated on delta literal before cancel (peak={peak}, largest={largest})"
        );
        assert!(
            cancelled
                .err()
                .expect("expected cancellation")
                .downcast_ref::<sparkles_core::Error>()
                .is_some_and(|e| matches!(e, sparkles_core::Error::Cancelled))
        );
    }

    #[test]
    fn explanation_owl_imports_retains_decode_credit_across_long_delta_iris() {
        let ex = |local: &str| {
            Term::NamedNode(oxrdf::NamedNode::new(format!("http://ex.org/{local}")).unwrap())
        };
        let long = "z".repeat(65_536);
        let imp1 = format!("http://imp.org/one_{long}");
        let imp2 = format!("http://imp.org/two_{long}");
        let imp3 = format!("http://imp.org/three_{long}");
        let owl_imports = crate::inputs::OWL_IMPORTS;

        let extras = Extras::default();
        let rules = Profile::Rules(
            "@prefix ex: <http://ex.org/> . [derive: (?x ex:p ?y) -> (?x ex:q ?y)]".into(),
        );
        let inputs = crate::Inputs {
            data_graphs: vec![crate::inputs::GraphRef::Default],
            imports: crate::inputs::ImportMode::Dataset,
            ..Default::default()
        };
        let reasoner = QueryReasoner::new(rules, &extras, inputs).unwrap();

        // Baseline session memory without long owl:imports IRIs.
        let base_store = sparkles_core::store::Store::in_memory(Default::default());
        sparkles_core::sparql::update::update(
            &base_store,
            "INSERT DATA { <http://ex.org/s> <http://ex.org/p> <http://ex.org/o> }",
            &Default::default(),
        )
        .unwrap();
        let base_input = DemandInput {
            reasoner: &reasoner,
            snap: base_store.snapshot(),
            readable: None,
        };
        let probe = demand_session(&base_input, &ExplainOptions::default(), true).unwrap();
        let base_used = probe.budget().used();
        drop(probe);

        // Headroom fits one 64 KiB decoded import name, strictly below two simultaneous 64 KiB names.
        let cap = base_used + 96 * 1024;
        assert!(cap < base_used + 2 * 65_536);
        let opts = ExplainOptions {
            max_memory: Some(cap),
            ..Default::default()
        };

        // 1. Three distinct long delta owl:imports IRIs that do not resolve to graphs:
        //    credit must stay with each live decoded name and its retained warning so the
        //    cap refuses before accumulating multiple 64 KiB strings in memory.
        let unresolved_store = sparkles_core::store::Store::in_memory(Default::default());
        sparkles_core::sparql::update::update(
            &unresolved_store,
            &format!(
                "INSERT DATA {{ \
                    <http://ex.org/ont> <{owl_imports}> <{imp1}>, <{imp2}>, <{imp3}> . \
                    <http://ex.org/s> <http://ex.org/p> <http://ex.org/o> . \
                }}"
            ),
            &Default::default(),
        )
        .unwrap();
        let unresolved_snap = unresolved_store.snapshot();
        for iri in [&imp1, &imp2, &imp3] {
            assert_eq!(unresolved_snap.lookup_iri(iri).unwrap().tag(), Tag::Delta);
        }
        let unresolved_input = DemandInput {
            reasoner: &reasoner,
            snap: unresolved_snap,
            readable: None,
        };

        let (res, peak, largest) = crate::alloc_probe::peak_and_largest_of(|| {
            explain_demand(&unresolved_input, &[ex("s"), ex("q"), ex("o")], &opts)
        });
        assert!(
            res.unwrap_err()
                .downcast_ref::<ExplainBudgetExceeded>()
                .is_some(),
            "unresolved long delta imports expected ExplainBudgetExceeded"
        );
        assert!(
            peak < base_used + 112 * 1024,
            "unresolved long delta imports exceeded single-name headroom (peak={peak}, cap={cap}, largest={largest})"
        );

        let wn_opts = crate::whynot::WhyNotOptions {
            explain: opts.clone(),
            ..Default::default()
        };
        let (res, peak, largest) = crate::alloc_probe::peak_and_largest_of(|| {
            crate::whynot::why_not(&unresolved_input, &[ex("s"), ex("q"), ex("o")], &wn_opts)
        });
        assert!(
            res.unwrap_err()
                .downcast_ref::<ExplainBudgetExceeded>()
                .is_some(),
            "unresolved long delta imports why-not expected ExplainBudgetExceeded"
        );
        assert!(
            peak < base_used + 112 * 1024,
            "unresolved long delta imports why-not exceeded single-name headroom (peak={peak}, cap={cap}, largest={largest})"
        );

        // 2. Three distinct long delta owl:imports IRIs that DO resolve to existing graphs:
        //    processing each decoded name incrementally while holding its credit keeps peak
        //    within single-name headroom and derives the triple from the 3rd imported graph.
        let resolved_store = sparkles_core::store::Store::in_memory(Default::default());
        sparkles_core::sparql::update::update(
            &resolved_store,
            &format!(
                "INSERT DATA {{ \
                    <http://ex.org/ont> <{owl_imports}> <{imp1}>, <{imp2}>, <{imp3}> . \
                    GRAPH <{imp1}> {{ <http://ex.org/a1> <http://ex.org/r> <http://ex.org/b1> }} \
                    GRAPH <{imp2}> {{ <http://ex.org/a2> <http://ex.org/r> <http://ex.org/b2> }} \
                    GRAPH <{imp3}> {{ <http://ex.org/s> <http://ex.org/p> <http://ex.org/o> }} \
                }}"
            ),
            &Default::default(),
        )
        .unwrap();
        let resolved_input = DemandInput {
            reasoner: &reasoner,
            snap: resolved_store.snapshot(),
            readable: None,
        };
        let (res, peak, largest) = crate::alloc_probe::peak_and_largest_of(|| {
            explain_demand(&resolved_input, &[ex("s"), ex("q"), ex("o")], &opts)
        });
        assert!(
            matches!(res, Ok(Explained::Found { ref trees, .. }) if !trees.is_empty()),
            "resolved long delta imports should derive from 3rd graph, got {res:?}"
        );
        assert!(
            peak < base_used + 112 * 1024,
            "resolved long delta imports held multiple 64 KiB names live at once (peak={peak}, cap={cap}, largest={largest})"
        );
    }

    #[test]
    fn demand_debug_trace_avoids_unchecked_predicate_decode() {
        struct DemandDebugSubscriber;
        impl tracing::Subscriber for DemandDebugSubscriber {
            fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
                meta.target() == "sparkles_reasoner::demand"
                    && *meta.level() <= tracing::Level::DEBUG
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, _: &tracing::Event<'_>) {}
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }

        let store = sparkles_core::store::Store::in_memory(Default::default());
        let long = "z".repeat(65_536);
        let long_p = format!("http://ex.org/p_{long}");
        sparkles_core::sparql::update::update(
            &store,
            &format!(
                "INSERT DATA {{ \
                    <http://ex.org/s> <http://ex.org/p> <http://ex.org/o> . \
                    <http://ex.org/a> <{long_p}> <http://ex.org/b> . \
                }}"
            ),
            &Default::default(),
        )
        .unwrap();
        let snap = store.snapshot();
        assert_eq!(snap.lookup_iri(&long_p).unwrap().tag(), Tag::Delta);

        let ex = |local: &str| {
            Term::NamedNode(oxrdf::NamedNode::new(format!("http://ex.org/{local}")).unwrap())
        };
        let extras = Extras::default();
        let rules = Profile::Rules(format!(
            "@prefix ex: <http://ex.org/> . \
             [r1: (?x ex:p ?y) -> (?x <{long_p}> ?y)] \
             [r2: (?x <{long_p}> ?y) -> (?x ex:q ?y)]"
        ));
        let reasoner = QueryReasoner::new(rules, &extras, crate::Inputs::default()).unwrap();
        let input = DemandInput {
            reasoner: &reasoner,
            snap,
            readable: None,
        };
        let mut session = demand_session(&input, &ExplainOptions::default(), true).unwrap();
        let target_ids = [
            session.terms().try_id_for(&ex("s")).unwrap(),
            session.terms().try_id_for(&ex("q")).unwrap(),
            session.terms().try_id_for(&ex("o")).unwrap(),
        ];

        let (res, peak, largest) = tracing::subscriber::with_default(DemandDebugSubscriber, || {
            tracing::callsite::rebuild_interest_cache();
            crate::alloc_probe::peak_and_largest_of(|| {
                session.derive_ids(&[[
                    Some(target_ids[0]),
                    Some(target_ids[1]),
                    Some(target_ids[2]),
                ]])
            })
        });
        assert!(res.is_ok(), "derive_ids with debug tracing failed: {res:?}");
        assert!(
            largest < 32_768 && peak < 65_536,
            "demand debug trace decoded 64 KiB delta predicate (peak={peak}, largest={largest})"
        );
    }

    #[test]
    fn proof_decode_admits_nested_triple_allocations() {
        let store = sparkles_core::store::Store::in_memory(Default::default());
        let mut term: Term = oxrdf::NamedNode::new_unchecked("urn:o").into();
        let subject = oxrdf::NamedNode::new_unchecked("urn:subject-0123");
        let predicate = oxrdf::NamedNode::new_unchecked("urn:predica-0123");
        for _ in 0..sparkles_core::nesting::MAX_TRIPLE_TERMS {
            term = Term::Triple(Box::new(oxrdf::Triple::new(
                subject.clone(),
                predicate.clone(),
                term,
            )));
        }
        let key = sparkles_core::id::term_key(&term);
        let limit = key.len() * 2;
        let decoded_bytes = term_bytes(&term);
        // Keep the cap well above `anyhow::Error` backtrace capture under `RUST_BACKTRACE=1`
        // while still far below the decoded nested `Term` size.
        assert!(
            limit >= 16_384 && limit < decoded_bytes,
            "nested triple limit ({limit}) must exceed error backtrace overhead and stay below decoded size ({decoded_bytes})"
        );
        let terms = Terms::new(store.snapshot());
        let id = terms.id_for(&term).unwrap();
        let budget = Budget::new(Some(limit));
        let (result, peak) = crate::alloc_probe::peak_of(|| terms.term_admitted(id, &budget));
        assert!(
            peak <= limit,
            "decoded a nested triple over budget (peak={peak}, limit={limit})"
        );
        assert!(BudgetFailure::from_anyhow(&result.unwrap_err()).is_some());
        assert_eq!(budget.used(), 0);
        let (decoded, held) = terms.term_admitted(id, &Budget::unlimited()).unwrap();
        assert_eq!(decoded.as_ref(), Some(&term));
        assert_eq!(held.bytes(), decoded_bytes);
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
            terms.id_for(&named("a")).unwrap(),
            terms.id_for(&named("q")).unwrap(),
            terms.id_for(&named("b")).unwrap(),
        ];
        let premise = [root[0], terms.id_for(&named("p")).unwrap(), root[2]];
        let profile = Profile::Rules("[step: (<http://ex.org/a> <http://ex.org/p> <http://ex.org/b>) -> (<http://ex.org/a> <http://ex.org/q> <http://ex.org/b>)]".into());
        let compiled = engine::compile_info(
            &profile.rules().unwrap(),
            &terms,
            &mut WorkingWarnings::new(&budget),
            false,
        )
        .unwrap();
        let (rules, infos) = (compiled.rules(), compiled.infos());
        let sources = vec!["rules".into()];
        let rs = RuleSet {
            rules,
            infos,
            sources: &sources,
            geo: None,
            relations: None,
        };
        let mut g = Graph::with_capacity(0);
        let plans = Plans::new(&mut g, rules).unwrap();
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
            rounds: &[],
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
            full_schedule: None,
            _full_schedule_allocation: None,
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
            memory: Held::new(&budget),
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
