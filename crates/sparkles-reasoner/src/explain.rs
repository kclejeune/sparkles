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
//! 100,000 instances holds about 10 MB at most. Reading the premises' depths from those
//! instances again after each level is what makes the trees minimal.

use crate::budget::Budget;
use crate::engine::{self, CRule, Eval, Mode};
use crate::facts::Base;
use crate::graph::{Graph, Triple};
use crate::incremental::{Plans, bind_head};
use crate::inputs::GraphRef;
use crate::terms::Terms;
use crate::{Extras, Profile};
use oxrdf::Term;
use rustc_hash::FxHashMap;
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
    /// the memory the rule evaluations may hold, unbounded when `None`
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

/// What a node of a derivation is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DerivationKind {
    /// a fact of an input graph
    Asserted,
    /// a fact from a rule with an empty body
    Axiom,
    /// a fact a rule instance derived from the node's premises
    Derived,
    /// a fact the tree does not expand because it passed the depth limit; another request
    /// on its triple explains it
    Ref,
}

impl DerivationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DerivationKind::Asserted => "asserted",
            DerivationKind::Axiom => "axiom",
            DerivationKind::Derived => "derived",
            DerivationKind::Ref => "ref",
        }
    }
}

/// The rule of a derived node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleRef {
    /// the rule's name in its rule text, or `rule@line N` for an unnamed rule
    pub name: String,
    /// the profile the rule belongs to, or the vocabulary that added it
    pub profile: String,
    /// the rule's id in the OWL 2 RL tables, for the rules of the `owl-rl` profile that
    /// carry one
    pub owl2rl: Option<String>,
}

/// One node of a derivation tree.
#[derive(Clone, Debug)]
pub struct Derivation {
    /// the node's number in the answer; a fact has one number in all the answer's trees
    pub id: u32,
    pub triple: [Term; 3],
    pub kind: DerivationKind,
    /// for an asserted node, the input graph that holds the fact
    pub graph: Option<GraphRef>,
    /// for a derived node or an axiom, its rule
    pub rule: Option<RuleRef>,
    /// the premises, in the order of the rule's body
    pub premises: Vec<Derivation>,
    /// whether the node repeats one given earlier in the answer with the same `id`, whose
    /// premises it leaves out
    pub shared: bool,
    /// the depth of the fact's derivation: 0 for an asserted fact, 1 for an axiom
    pub depth: u32,
}

/// The answer of an explanation.
#[derive(Clone, Debug)]
pub enum Explained {
    /// derivations from the leaves, the first of the smallest depth, then others whose top
    /// rule differs, and whether derivations by further rules exist
    Found { trees: Vec<Derivation>, more: bool },
    /// no derivation from the leaves exists, or the closure does not hold the triple
    NotFound,
}

/// An explanation that passed its limits.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct ExplainBudgetExceeded(pub String);

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

impl<'a> Search<'a> {
    fn fact(&mut self, t: Triple) -> (u32, bool) {
        if let Some(&i) = self.index.get(&t) {
            return (i, false);
        }
        let i = self.facts.len() as u32;
        let leaf = self.asserted(&t);
        self.facts.push(t);
        self.index.insert(t, i);
        self.leaf.push(leaf);
        self.by_head.push(Vec::new());
        (i, true)
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
            let Some((steps, pre)) = &self.plans.back[ri][hi] else {
                continue;
            };
            let rule = &self.rules[ri];
            let Some(mut b) = bind_head(rule, hi, pre, t) else {
                continue;
            };
            let mut found: Vec<Vec<Triple>> = Vec::new();
            let over = Cell::new(false);
            let room = self.opts.max_instances.saturating_sub(self.visited);
            let deadline = self.opts.deadline;
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
                ev.run(0, &mut b, None);
                ev.err.take()
            };
            if let Some(e) = failed {
                return Err(Stop::Error(e));
            }
            for body in found {
                self.visited += 1;
                // an instance among whose premises is its own head supports nothing
                if body.contains(&t) {
                    continue;
                }
                let from = self.premises.len() as u32;
                for u in body {
                    let (m, new) = self.fact(u);
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

    /// The depth of every fact reached, and the instance that gives it.
    fn settle(&mut self) {
        let n = self.facts.len();
        self.depth = vec![NONE; n];
        self.via = vec![NONE; n];
        let mut users: Vec<Vec<u32>> = vec![Vec::new(); n];
        let mut open: Vec<u32> = Vec::with_capacity(self.insts.len());
        let mut bucket: Vec<u32> = Vec::new();
        let mut later: Vec<u32> = Vec::new();
        for (f, l) in self.leaf.iter().enumerate() {
            if l.is_some() {
                self.depth[f] = 0;
                bucket.push(f as u32);
            }
        }
        for (i, inst) in self.insts.iter().enumerate() {
            let ps = &self.premises[inst.from as usize..inst.to as usize];
            open.push(ps.len() as u32);
            for &p in ps {
                users[p as usize].push(i as u32);
            }
            if ps.is_empty() && self.depth[inst.head as usize] == NONE {
                self.depth[inst.head as usize] = 1;
                self.via[inst.head as usize] = i as u32;
                later.push(inst.head);
            }
        }
        let mut d = 0u32;
        while !bucket.is_empty() || !later.is_empty() {
            for f in std::mem::take(&mut bucket) {
                for &i in &users[f as usize] {
                    open[i as usize] -= 1;
                    if open[i as usize] == 0 {
                        let h = self.insts[i as usize].head as usize;
                        if self.depth[h] == NONE {
                            self.depth[h] = d + 1;
                            self.via[h] = i;
                            later.push(h as u32);
                        }
                    }
                }
            }
            d += 1;
            bucket = std::mem::take(&mut later)
                .into_iter()
                .filter(|&f| self.depth[f as usize] == d)
                .collect();
        }
    }

    /// The instances of fact `f` whose premises all have a depth, one per rule, the one of
    /// `via[f]` first and the others by depth.
    fn tops(&self, f: u32) -> Vec<u32> {
        let mut best: FxHashMap<u32, (u32, u32)> = FxHashMap::default();
        for &i in &self.by_head[f as usize] {
            let inst = &self.insts[i as usize];
            let ps = &self.premises[inst.from as usize..inst.to as usize];
            if ps.iter().any(|&p| self.depth[p as usize] == NONE) {
                continue;
            }
            let d = 1 + ps
                .iter()
                .map(|&p| self.depth[p as usize])
                .max()
                .unwrap_or(0);
            let e = best.entry(inst.rule).or_insert((d, i));
            if d < e.0 {
                *e = (d, i);
            }
        }
        let first = self.via[f as usize];
        let mut v: Vec<(u32, u32)> = best.into_values().collect();
        v.sort_by_key(|&(d, i)| (i != first, d, i));
        v.into_iter().map(|(_, i)| i).collect()
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

    fn graph(&self, g: Id) -> anyhow::Result<GraphRef> {
        if g == Id::DEFAULT_GRAPH {
            return Ok(GraphRef::Default);
        }
        match self.term(g.0)? {
            Term::NamedNode(n) => Ok(GraphRef::Named(n.into_string())),
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
    let (r, _) = s.fact(root);
    let mut frontier = if s.leaf[r as usize].is_some() {
        Vec::new()
    } else {
        vec![r]
    };
    let mut found_at: Option<u32> = None;
    let mut level = 0u32;
    loop {
        s.settle();
        if s.depth[r as usize] != NONE && found_at.is_none() {
            found_at = Some(level);
        }
        let enough = match found_at {
            None => false,
            Some(at) => s.tops(r).len() > max_trees || level >= at + EXTRA_LEVELS,
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
                s.settle();
                break;
            }
            Some(Stop::Budget(m)) => return Err(ExplainBudgetExceeded(m).into()),
            Some(Stop::Error(e)) => return Err(e),
        }
        frontier = next;
        level += 1;
    }
    if found_at.is_none() {
        return Ok(Explained::NotFound);
    }
    let mut b = Builder {
        s: &s,
        sources: &sources,
        max_depth: opts.max_depth.min(MAX_DEPTH_LIMIT),
        ids: FxHashMap::default(),
        next: 0,
    };
    let mut trees = Vec::new();
    let tops = s.tops(r);
    if s.leaf[r as usize].is_some() {
        trees.push(b.node(r, 0, None)?);
    }
    for &i in tops.iter().take(max_trees.saturating_sub(trees.len())) {
        trees.push(b.node(r, 0, Some(i))?);
    }
    let more = tops.len() + usize::from(s.leaf[r as usize].is_some()) > trees.len();
    Ok(Explained::Found { trees, more })
}
