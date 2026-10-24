//! Explanations of inferred triples over a materialized closure (spec U10 §3.3).
//!
//! The search reads the closure as the input graphs and the inferred graph of a snapshot,
//! through the reasoner's base tier, so nothing is loaded up front. It walks backward from
//! the triple a level at a time. Each level enumerates, for the facts it reached, the rule
//! instances whose head is the fact and whose premises are in the closure, with the same
//! backward plans as the incremental search. After each level it works out, for every fact
//! reached, the smallest depth at which it is supported: an asserted fact has depth 0, and
//! a fact has depth `d + 1` when one of its instances has premises of depth `d` or less. A
//! fact that only facts depending on it support never gets a depth, so every tree the
//! search returns is well founded, whatever order incremental runs left the closure's rows
//! in. The search stops at the first level where the triple has a depth, and the depths of
//! the facts it reached then describe a tree of the smallest depth.
//!
//! The leaves are the facts of the input graphs the caller passes, which are the graphs
//! the caller can read, in the snapshot of the caller's view. A derivation is returned only
//! when it reaches such facts, and when none exists the answer is the same as for a triple
//! the closure does not hold.
//!
//! The search keeps every instance it enumerates until it answers. Its memory grows with
//! the instance budget, about 100 bytes per instance, so a request at the default budget of
//! 100,000 instances holds about 10 MB at most, and all of it is charged to the request's
//! memory budget. Reading the premises' depths from those instances again after each level
//! is what makes the trees minimal. The deadline is read between rules and inside each
//! rule's evaluation, so a rule whose candidates all fail stops at it too.

use crate::budget::Budget;
use crate::engine::{self, CRule, Eval, Mode};
use crate::facts::Base;
use crate::graph::{Graph, Triple};
use crate::incremental::{Plans, bind_head};
use crate::terms::Terms;
use crate::{Extras, Profile};
use oxrdf::Term;
use rustc_hash::FxHashMap;
pub use sparkles_core::explain::{
    Derivation, DerivationKind, ExplainBudgetExceeded, Explained, RuleRef,
};
use sparkles_core::id::{Id, Tag};
use sparkles_core::index::Perm;
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

/// What the search reads.
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
}

const NONE: u32 = u32::MAX;

/// Per fact, its depth and the instance that gives it, and the root's top instances.
type RootTops = (Vec<u32>, Vec<u32>, Vec<u32>);

struct Search<'a> {
    snap: &'a Snapshot,
    g: &'a Graph,
    terms: &'a Terms,
    rules: &'a [CRule],
    plans: &'a Plans,
    inputs: &'a [Id],
    budget: &'a Arc<Budget>,
    opts: &'a ExplainOptions,
    abort: AtomicBool,
    facts: Vec<Triple>,
    index: FxHashMap<Triple, u32>,
    /// per fact: the input graph that asserts it, or `None`
    leaf: Vec<Option<Id>>,
    /// per fact: its instances
    by_head: Vec<Vec<u32>>,
    insts: Vec<Inst>,
    premises: Vec<u32>,
    visited: usize,
    /// per fact, after [`settle`](Self::settle): its depth (`NONE` when it has none) and
    /// the instance that gives it (`NONE` for a leaf)
    depth: Vec<u32>,
    via: Vec<u32>,
}

/// Why a search stopped before its answer.
enum Stop {
    Budget(String),
    Error(anyhow::Error),
}

/// The heap one fact takes in the search: its triple, its index entry, its leaf and its
/// list of instances.
const FACT_BYTES: usize = 3 * 8 + 48 + 16 + 24 + 8;
/// The heap one instance takes, without its premises.
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
        self.charge(FACT_BYTES)?;
        let i = self.facts.len() as u32;
        let leaf = self.asserted(&t);
        self.facts.push(t);
        self.index.insert(t, i);
        self.leaf.push(leaf);
        self.by_head.push(Vec::new());
        Ok((i, true))
    }

    /// The first input graph that holds `t`.
    fn asserted(&self, t: &Triple) -> Option<Id> {
        if t.iter().any(|&x| Id(x).tag() == Tag::Local) {
            return None;
        }
        self.inputs.iter().copied().find(|g| {
            self.snap
                .count(Perm::Gspo, &[g.0, t[0], t[1], t[2]])
                .is_ok_and(|n| n > 0)
        })
    }

    fn check_time(&self) -> Result<(), Stop> {
        if self
            .opts
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            return Err(Stop::Error(anyhow::anyhow!("cancelled")));
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
        let heads: Vec<(usize, usize)> = self.plans.heads(t[1]).collect();
        for (ri, hi) in heads {
            self.check_time()?;
            let Some((steps, pre)) = &self.plans.back[ri][hi] else {
                continue;
            };
            let rule = &self.rules[ri];
            let Some(mut b) = bind_head(rule, hi, pre, t) else {
                continue;
            };
            let mut found: Vec<Vec<Triple>> = Vec::new();
            let over = Cell::new(false);
            let no_room = Cell::new(None);
            let held = Cell::new(0usize);
            let room = self.opts.max_instances.saturating_sub(self.visited);
            let deadline = self.opts.deadline;
            let budget = self.budget;
            let failed = {
                let mut sink = |matched: &[Triple], implicit: &[Triple]| -> bool {
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
                    // the buffer until the instances are kept, given back below
                    let n = 24 + body.capacity() * std::mem::size_of::<Triple>();
                    if let Err(e) = budget.try_charge(n) {
                        no_room.set(Some(e));
                        return false;
                    }
                    held.set(held.get() + n);
                    found.push(body);
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
            let kept = self.keep(f, ri, t, found, next);
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

    /// Keep the instances `found` of rule `ri` for fact `f`, whose triple is `t`.
    fn keep(
        &mut self,
        f: u32,
        ri: usize,
        t: Triple,
        found: Vec<Vec<Triple>>,
        next: &mut Vec<u32>,
    ) -> Result<(), Stop> {
        for body in found {
            self.visited += 1;
            // an instance among whose premises is its own head supports nothing
            if body.contains(&t) {
                continue;
            }
            self.charge(INST_BYTES + 4 * body.len())?;
            let from = self.premises.len() as u32;
            for u in body {
                let (m, new) = self.fact(u)?;
                self.premises.push(m);
                if new && self.leaf[m as usize].is_none() {
                    next.push(m);
                }
            }
            let i = self.insts.len() as u32;
            self.insts.push(Inst {
                rule: ri as u32,
                head: f,
                from,
                to: self.premises.len() as u32,
            });
            self.by_head[f as usize].push(i);
        }
        Ok(())
    }

    /// The depth of every fact reached, and the instance that gives it.
    fn settle(&mut self) -> Result<(), Stop> {
        let (depth, via) = self.settle_with(None)?;
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
        // the users and open counts are freed here, the depths are kept by the caller
        self.budget
            .release(n * 24 + self.premises.len() * 4 + self.insts.len() * 4);
        Ok((depth, via))
    }

    /// The instances of fact `f` whose premises all have a depth in `depth`, one per rule,
    /// `first` first and the others by depth.
    fn tops(&self, f: u32, depth: &[u32], first: u32) -> Vec<u32> {
        let mut best: FxHashMap<u32, (u32, u32)> = FxHashMap::default();
        for &i in &self.by_head[f as usize] {
            let inst = &self.insts[i as usize];
            let ps = &self.premises[inst.from as usize..inst.to as usize];
            if ps.iter().any(|&p| depth[p as usize] == NONE) {
                continue;
            }
            let d = 1 + ps.iter().map(|&p| depth[p as usize]).max().unwrap_or(0);
            let e = best.entry(inst.rule).or_insert((d, i));
            if d < e.0 {
                *e = (d, i);
            }
        }
        let mut v: Vec<(u32, u32)> = best.into_values().collect();
        v.sort_by_key(|&(d, i)| (i != first, d, i));
        v.into_iter().map(|(_, i)| i).collect()
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

/// Builds the answer's trees, numbering facts once across them.
struct Builder<'s, 'a> {
    s: &'s Search<'a>,
    sources: &'s [String],
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
        Ok([self.term(t[0])?, self.term(t[1])?, self.term(t[2])?])
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
        let ri = self.s.insts[inst as usize].rule as usize;
        let name = self.s.rules[ri].name.clone();
        let profile = self.sources[ri].clone();
        RuleRef {
            owl2rl: owl2rl_id(&profile, &name),
            name,
            profile,
        }
    }

    /// The node of fact `f` at `level` of a tree, by instance `inst` (or the fact's own).
    fn node(&mut self, f: u32, level: u32, inst: Option<u32>) -> anyhow::Result<Derivation> {
        let s = self.s;
        let triple = self.triple(f)?;
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
                self.ids.insert(f, self.next);
                (self.next, false)
            }
        };
        let depth = s.depth[f as usize];
        if let Some(g) = s.leaf[f as usize] {
            return Ok(Derivation {
                id,
                triple,
                kind: DerivationKind::Asserted,
                graph: Some(self.graph(g)?),
                rule: None,
                premises: Vec::new(),
                shared,
                depth: 0,
            });
        }
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
        })
    }
}

/// Explain why the closure holds `triple`: up to `opts.max_trees` derivations from the
/// facts of `input.inputs`, whose top rules differ. Fails with [`ExplainBudgetExceeded`]
/// when the search passes its limits before it finds the first one.
pub fn explain(
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
    // the rules, each with the profile or vocabulary it came from
    let mut warnings = Vec::new();
    let mut rules: Vec<CRule> = Vec::new();
    let mut sources: Vec<String> = Vec::new();
    let group = engine::compile(&input.profile.rules()?, &terms, &mut warnings);
    sources.extend(std::iter::repeat_n(
        input.profile.name().to_string(),
        group.len(),
    ));
    rules.extend(group);
    for v in &input.extras.vocabularies {
        let group = engine::compile(&v.rules()?, &terms, &mut warnings);
        sources.extend(std::iter::repeat_n(v.name().to_string(), group.len()));
        rules.extend(group);
    }
    let plans = Plans::new(&mut g, &rules)?;
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
    let max_trees = opts.max_trees.clamp(1, MAX_TREES_LIMIT);
    let mut s = Search {
        snap: &snap,
        g: &g,
        terms: &terms,
        rules: &rules,
        plans: &plans,
        inputs: input.inputs,
        budget: &budget,
        opts,
        abort: AtomicBool::new(false),
        facts: Vec::new(),
        index: FxHashMap::default(),
        leaf: Vec::new(),
        by_head: Vec::new(),
        insts: Vec::new(),
        premises: Vec::new(),
        visited: 0,
        depth: Vec::new(),
        via: Vec::new(),
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
    loop {
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
            return Err(ExplainBudgetExceeded(format!(
                "the derivation is deeper than the search allows ({SEARCH_DEPTH} levels)"
            ))
            .into());
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
            Some(e) => return Err(stop(e)),
        }
        frontier = next;
        level += 1;
    }
    if found_at.is_none() {
        return Ok(Explained::NotFound);
    }
    // every tree reads the depths with the root excluded, which equal the plain depths
    // under the root's first instance
    let (depth, via, tops) = s.root_tops(r).map_err(stop)?;
    s.depth = depth;
    s.via = via;
    let mut b = Builder {
        s: &s,
        sources: &sources,
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
