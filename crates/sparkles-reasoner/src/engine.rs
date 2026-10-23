//! Rule compilation and semi-naive forward chaining over id-level triples.

use crate::budget::{Budget, Held, MemoryExceeded, table_bytes, table_grown};
use crate::builtins::{BuiltinKind, HeadAction};
use crate::facts::{BaseRange, Memo};
use crate::graph::{Cands, Graph, Triple};
use crate::parser::{BuiltinCall, Clause, Direction, Node, Rule};
use crate::terms::{Terms, anchored_regex};
use oxrdf::Term;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::HashSet;
use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// ================================================================== compile ====

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Var(usize),
    Const(u64),
    Any,
}

#[derive(Clone, Debug)]
pub(crate) struct Atom {
    pub s: Slot,
    pub p: Slot,
    pub o: Slot,
}

#[derive(Debug)]
pub(crate) struct CBuiltin {
    pub kind: BuiltinKind,
    pub args: Vec<Slot>,
    /// variables that must be bound before the call
    pub inputs: Vec<usize>,
    /// variables the call binds
    pub outputs: Vec<usize>,
    /// pre-compiled pattern for `regex` with a constant pattern
    pub regex: Option<regex::Regex>,
}

#[derive(Debug)]
pub(crate) enum CHead {
    Triple(Slot, Slot, Slot),
    Action(HeadAction, Vec<Slot>),
}

#[derive(Debug)]
pub(crate) struct CRule {
    pub name: String,
    pub nvars: usize,
    pub atoms: Vec<Atom>,
    pub builtins: Vec<CBuiltin>,
    pub head: Vec<CHead>,
}

struct FlatRule {
    name: String,
    body: Vec<Clause>,
    head: Vec<Clause>,
}

/// Flatten nested rules in heads: `B1 -> [H <- B2]` materializes exactly like
/// `B1, B2 -> H` (and the same for nested forward rules).
fn flatten(r: &Rule, prefix: &[Clause], name: String, out: &mut Vec<FlatRule>) {
    let mut body = prefix.to_vec();
    body.extend(
        r.body
            .iter()
            .filter(|c| !matches!(c, Clause::Rule(_)))
            .cloned(),
    );
    let mut head = Vec::new();
    let mut nested = 0;
    for c in &r.head {
        match c {
            Clause::Rule(inner) => {
                nested += 1;
                let n = inner
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("{name}/{nested}"));
                flatten(inner, &body, n, out);
            }
            c => head.push(c.clone()),
        }
    }
    if !head.is_empty() || nested == 0 {
        out.push(FlatRule { name, body, head });
    }
}

fn vars_of(n: &Node, out: &mut Vec<String>) {
    match n {
        Node::Var(v) => out.push(v.clone()),
        Node::Functor(_, a) => a.iter().for_each(|x| vars_of(x, out)),
        _ => {}
    }
}

/// Compile parsed rules. Unsupported rules are skipped with a warning.
pub(crate) fn compile(rules: &[Rule], terms: &Terms, warnings: &mut Vec<String>) -> Vec<CRule> {
    let mut flat = Vec::new();
    for r in rules {
        if r.direction == Direction::Backward {
            warnings.push(format!(
                "rule '{}' (line {}): backward rules ('<-') are not supported by forward materialization; skipped",
                r.label(),
                r.line
            ));
            continue;
        }
        if r.body.iter().any(|c| matches!(c, Clause::Rule(_))) {
            warnings.push(format!(
                "rule '{}': nested rule in a body; skipped",
                r.label()
            ));
            continue;
        }
        flatten(r, &[], r.label(), &mut flat);
    }
    let mut out = Vec::new();
    for f in flat {
        match compile_one(&f, terms, warnings) {
            Ok(Some(c)) => out.push(c),
            Ok(None) => {}
            Err(msg) => warnings.push(format!("rule '{}': {msg}; skipped", f.name)),
        }
    }
    out
}

fn compile_one(
    f: &FlatRule,
    terms: &Terms,
    warnings: &mut Vec<String>,
) -> Result<Option<CRule>, String> {
    let mut vars: FxHashMap<String, usize> = FxHashMap::default();
    let mut var = |v: &str| -> usize {
        let n = vars.len();
        *vars.entry(v.to_string()).or_insert(n)
    };
    let slot = |n: &Node, var: &mut dyn FnMut(&str) -> usize| -> Result<Slot, String> {
        Ok(match n {
            Node::Var(v) => Slot::Var(var(v)),
            Node::Const(t) => Slot::Const(terms.id_for(t)),
            Node::Any => Slot::Any,
            Node::Functor(name, _) => {
                return Err(format!(
                    "functor term '{name}(…)' is not supported by forward materialization"
                ));
            }
        })
    };
    let mut atoms = Vec::new();
    let mut builtins = Vec::new();
    // variables bound at the current textual position (Jena evaluates bodies left to right)
    let mut bound: HashSet<String> = HashSet::new();
    for c in &f.body {
        match c {
            Clause::Triple(t) => {
                let a = Atom {
                    s: slot(&t.subject, &mut var)?,
                    p: slot(&t.predicate, &mut var)?,
                    o: slot(&t.object, &mut var)?,
                };
                let mut vs = Vec::new();
                for n in [&t.subject, &t.predicate, &t.object] {
                    vars_of(n, &mut vs);
                }
                bound.extend(vs);
                atoms.push(a);
            }
            Clause::Builtin(b) => {
                let Some(kind) = BuiltinKind::from_name(&b.name) else {
                    if let Some(always) = static_bound_test(b, &bound)? {
                        if !always {
                            // e.g. `unbound(?x)` after ?x was bound: the rule can never fire
                            return Ok(None);
                        }
                        continue;
                    }
                    return Err(format!("unknown or unsupported builtin '{}'", b.name));
                };
                kind.check_arity(b.args.len())
                    .map_err(|e| format!("{}: {e}", b.name))?;
                let mut args = Vec::new();
                for (i, a) in b.args.iter().enumerate() {
                    // noValue: variables not bound at this position act as wildcards
                    if kind == BuiltinKind::NoValue
                        && let Node::Var(v) = a
                        && !bound.contains(v)
                    {
                        args.push(Slot::Any);
                        continue;
                    }
                    let s = slot(a, &mut var)?;
                    if s == Slot::Any
                        && kind.input_positions(b.args.len()).contains(&i)
                        && kind != BuiltinKind::NoValue
                    {
                        return Err(format!("wildcard as input of builtin '{}'", b.name));
                    }
                    args.push(s);
                }
                let inputs: Vec<usize> = kind
                    .input_positions(args.len())
                    .into_iter()
                    .filter_map(|i| match args[i] {
                        Slot::Var(v) => Some(v),
                        _ => None,
                    })
                    .collect();
                let outputs: Vec<usize> = kind
                    .output_positions(args.len())
                    .into_iter()
                    .filter_map(|i| match args[i] {
                        Slot::Var(v) => Some(v),
                        _ => None,
                    })
                    .collect();
                for (i, a) in b.args.iter().enumerate() {
                    if kind.output_positions(b.args.len()).contains(&i)
                        && let Node::Var(v) = a
                    {
                        bound.insert(v.clone());
                    }
                }
                let regex = match (kind, b.args.get(1)) {
                    (BuiltinKind::Regex, Some(Node::Const(Term::Literal(l)))) => Some(
                        anchored_regex(l.value())
                            .ok_or_else(|| format!("invalid regex '{}'", l.value()))?,
                    ),
                    _ => None,
                };
                builtins.push(CBuiltin {
                    kind,
                    args,
                    inputs,
                    outputs,
                    regex,
                });
            }
            Clause::Rule(_) => unreachable!("flattened"),
        }
    }
    let mut head = Vec::new();
    for c in &f.head {
        match c {
            Clause::Triple(t) => {
                let s = slot(&t.subject, &mut var)?;
                let p = slot(&t.predicate, &mut var)?;
                let o = slot(&t.object, &mut var)?;
                if [s, p, o].contains(&Slot::Any) {
                    return Err("wildcard in a head triple".into());
                }
                head.push(CHead::Triple(s, p, o));
            }
            Clause::Builtin(b) => match HeadAction::from_name(&b.name) {
                Some(Ok(action)) => {
                    let args = b
                        .args
                        .iter()
                        .map(|a| slot(a, &mut var))
                        .collect::<Result<Vec<_>, _>>()?;
                    if args.len() != 3 {
                        return Err(format!("{} expects 3 arguments", b.name));
                    }
                    head.push(CHead::Action(action, args));
                }
                Some(Err(())) => {
                    warnings.push(format!(
                        "rule '{}': head builtin '{}' ignored",
                        f.name, b.name
                    ));
                }
                None => {
                    warnings.push(format!(
                        "rule '{}': unsupported head builtin '{}' ignored",
                        f.name, b.name
                    ));
                }
            },
            Clause::Rule(_) => unreachable!("flattened"),
        }
    }
    if head.is_empty() {
        return Ok(None);
    }
    let nvars = vars.len();
    let rule = CRule {
        name: f.name.clone(),
        nvars,
        atoms,
        builtins,
        head,
    };
    // every head variable must be bound by the body
    let mut bound_vars = vec![false; nvars];
    for a in &rule.atoms {
        for s in [a.s, a.p, a.o] {
            if let Slot::Var(v) = s {
                bound_vars[v] = true;
            }
        }
    }
    for b in &rule.builtins {
        for &v in &b.outputs {
            bound_vars[v] = true;
        }
    }
    for h in &rule.head {
        let slots: Vec<Slot> = match h {
            CHead::Triple(s, p, o) => vec![*s, *p, *o],
            CHead::Action(_, a) => a.clone(),
        };
        for s in slots {
            if let Slot::Var(v) = s
                && !bound_vars[v]
            {
                let name = vars
                    .iter()
                    .find(|(_, i)| **i == v)
                    .map(|(n, _)| n.clone())
                    .unwrap_or_default();
                return Err(format!("head variable ?{name} is not bound by the body"));
            }
        }
    }
    // every builtin must become evaluable
    let ranges = vec![(0u32, 0u32); rule.atoms.len()];
    if plan(&rule, &ranges, None).is_none() {
        return Err("a builtin's input variables are never bound by the body".into());
    }
    Ok(Some(rule))
}

/// `bound(?x…)` / `unbound(?x…)` are resolved statically from the textual position.
fn static_bound_test(b: &BuiltinCall, bound: &HashSet<String>) -> Result<Option<bool>, String> {
    let want = match b.name.as_str() {
        "bound" => true,
        "unbound" => false,
        _ => return Ok(None),
    };
    let mut all = true;
    for a in &b.args {
        let is_bound = match a {
            Node::Var(v) => bound.contains(v),
            Node::Any => false,
            _ => true,
        };
        all &= is_bound == want;
    }
    Ok(Some(all))
}

// ================================================================== planning ===

#[derive(Clone, Copy, Debug)]
pub(crate) enum Step {
    Atom { atom: usize, lo: u32, hi: u32 },
    Builtin(usize),
}

fn slot_bound(s: Slot, bound: &[bool]) -> bool {
    match s {
        Slot::Const(_) => true,
        Slot::Var(v) => bound[v],
        Slot::Any => false,
    }
}

/// Greedy join order: builtins as soon as their inputs are bound, then the atom with
/// the smallest estimated number of matches. `g = None` plans without statistics.
pub(crate) fn plan(rule: &CRule, ranges: &[(u32, u32)], g: Option<&Graph>) -> Option<Vec<Step>> {
    plan_with(rule, ranges, g, None, &[])
}

/// [`plan`] with the variables `prebound` bound before the first step, and atom `first`
/// (if any) as the first step.
pub(crate) fn plan_with(
    rule: &CRule,
    ranges: &[(u32, u32)],
    g: Option<&Graph>,
    first: Option<usize>,
    prebound: &[usize],
) -> Option<Vec<Step>> {
    let mut bound = vec![false; rule.nvars];
    for &v in prebound {
        bound[v] = true;
    }
    let mut steps = Vec::with_capacity(rule.atoms.len() + rule.builtins.len());
    let mut atoms_left: Vec<usize> = (0..rule.atoms.len()).collect();
    let mut bi_left: Vec<usize> = (0..rule.builtins.len()).collect();
    if let Some(d) = first {
        atoms_left.retain(|&i| i != d);
        let a = &rule.atoms[d];
        for s in [a.s, a.p, a.o] {
            if let Slot::Var(v) = s {
                bound[v] = true;
            }
        }
        steps.push(Step::Atom {
            atom: d,
            lo: ranges[d].0,
            hi: ranges[d].1,
        });
    }
    loop {
        // place builtins whose inputs are bound
        let mut progress = true;
        while progress {
            progress = false;
            bi_left.retain(|&j| {
                let b = &rule.builtins[j];
                if b.inputs.iter().all(|&v| bound[v]) {
                    steps.push(Step::Builtin(j));
                    for &v in &b.outputs {
                        bound[v] = true;
                    }
                    progress = true;
                    false
                } else {
                    true
                }
            });
        }
        if atoms_left.is_empty() {
            break;
        }
        let best = atoms_left
            .iter()
            .enumerate()
            .map(|(k, &i)| (k, estimate(&rule.atoms[i], &bound, ranges[i], g)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(k, _)| k)
            .unwrap();
        let i = atoms_left.remove(best);
        let a = &rule.atoms[i];
        for s in [a.s, a.p, a.o] {
            if let Slot::Var(v) = s {
                bound[v] = true;
            }
        }
        steps.push(Step::Atom {
            atom: i,
            lo: ranges[i].0,
            hi: ranges[i].1,
        });
    }
    bi_left.is_empty().then_some(steps)
}

fn estimate(a: &Atom, bound: &[bool], (lo, hi): (u32, u32), g: Option<&Graph>) -> f64 {
    let (sb, pb, ob) = (
        slot_bound(a.s, bound),
        slot_bound(a.p, bound),
        slot_bound(a.o, bound),
    );
    let Some(g) = g else {
        // no statistics: prefer bound positions
        return 3.0 - (sb as u8 + pb as u8 + ob as u8) as f64;
    };
    if hi <= lo {
        return 0.0;
    }
    let span = (hi - lo) as f64;
    let n = g.len().max(1) as f64;
    let frac = span / n;
    match a.p {
        Slot::Const(p) => {
            let cnt = g.count_p(p, lo, hi) as f64;
            let (ds, dos) = g.pstats(p);
            match (sb, ob) {
                (true, true) => cnt.min(1.0),
                (true, false) => cnt / ds.max(1) as f64,
                (false, true) => cnt / dos.max(1) as f64,
                (false, false) => cnt,
            }
        }
        _ if pb => {
            let per_p = span / g.distinct_predicates() as f64;
            match (sb, ob) {
                (true, true) => 1.0,
                (true, false) => (per_p / 4.0).max(1.0),
                (false, true) => (per_p / 4.0).max(1.0),
                (false, false) => per_p,
            }
        }
        _ => match (sb, ob) {
            (true, true) => (n / g.distinct_subjects() as f64 * frac).max(1.0),
            (true, false) => (n / g.distinct_subjects() as f64 * frac).max(1.0),
            (false, true) => (n / g.distinct_objects() as f64 * frac).max(1.0),
            (false, false) => span * 2.0,
        },
    }
}

/// Which optional graph indexes a plan uses: (by subject, by object).
fn index_needs(rule: &CRule, steps: &[Step]) -> (bool, bool) {
    index_needs_with(rule, steps, &[])
}

/// [`index_needs`] of a plan that starts with the variables `prebound` bound.
pub(crate) fn index_needs_with(rule: &CRule, steps: &[Step], prebound: &[usize]) -> (bool, bool) {
    let mut bound = vec![false; rule.nvars];
    for &v in prebound {
        bound[v] = true;
    }
    let (mut need_s, mut need_o) = (false, false);
    for st in steps {
        match *st {
            Step::Atom { atom, .. } => {
                let a = &rule.atoms[atom];
                if !slot_bound(a.p, &bound) {
                    if slot_bound(a.s, &bound) {
                        need_s = true;
                    } else if slot_bound(a.o, &bound) {
                        need_o = true;
                    }
                }
                for s in [a.s, a.p, a.o] {
                    if let Slot::Var(v) = s {
                        bound[v] = true;
                    }
                }
            }
            Step::Builtin(j) => {
                for &v in &rule.builtins[j].outputs {
                    bound[v] = true;
                }
            }
        }
    }
    (need_s, need_o)
}

// ================================================================ evaluation ===

/// Receives the body of a rule instance: the facts its atoms matched and the facts its
/// builtins read (`listForAll`). Returns false to stop the evaluation.
pub(crate) type Sink<'s> = dyn FnMut(&[Triple], &[Triple]) -> bool + 's;

/// Where the first step of a plan reads its candidates when the work is split.
#[derive(Clone, Copy)]
pub(crate) enum First<'a> {
    /// in-memory rows
    Rows(Cands<'a>),
    /// a share of a base-tier scan
    Base(BaseRange),
}

/// What an evaluation emits.
pub(crate) enum Mode<'s> {
    /// heads not in the graph yet (forward chaining)
    New,
    /// heads in the graph (the consequences of deleted facts)
    Existing,
    /// the instances whose head number `head` is `target`
    Instances {
        head: usize,
        target: Triple,
        sink: &'s mut Sink<'s>,
    },
}

pub(crate) struct Eval<'a> {
    pub g: &'a Graph,
    pub terms: &'a Terms,
    pub rule: &'a CRule,
    pub steps: &'a [Step],
    /// current end of the graph (visible state for noValue / list access)
    pub end: u32,
    pub abort: &'a AtomicBool,
    pub cancel: Option<&'a AtomicBool>,
    pub out: Vec<Triple>,
    pub max_out: usize,
    ticks: u32,
    pub mode: Mode<'a>,
    /// facts matched by the atoms of the current partial instance ([`Mode::Instances`])
    trail: smallvec::SmallVec<[Triple; 4]>,
    /// facts read by builtins of the current partial instance ([`Mode::Instances`])
    pub implicit: smallvec::SmallVec<[Triple; 2]>,
    stop: bool,
    /// heads emitted so far ([`Mode::New`]), so that each goes out once
    seen: FxHashSet<Triple>,
    /// the first error of a base-tier scan
    pub err: Option<anyhow::Error>,
    /// per plan step: the base-tier blocks it read last, made at the first read of the
    /// base tier (most evaluations of a backward search read none)
    memos: Vec<Memo>,
    /// per plan step: the base-tier facts of its recent lookups with few results, made at
    /// the first such lookup; unused when the base tier remembers lookups itself
    looked: Vec<FxHashMap<Triple, std::rc::Rc<[Triple]>>>,
    /// per plan step: a buffer for the facts of a lookup
    scratch: Vec<Vec<Triple>>,
    /// the base-tier blocks builtins read last
    bmemo: Memo,
    /// the members of the RDF lists builtins read
    lists: FxHashMap<u64, Option<std::rc::Rc<[u64]>>>,
    /// stop at this plan step and keep the bindings instead, up to this many
    pub collect_at: Option<(usize, usize)>,
    pub collected: Vec<Vec<u64>>,
    /// the heap of the buffers and caches above, charged to the run's budget
    held: Option<Held>,
    /// the run's budget, which `held` charges once the evaluation charges anything
    budget: &'a Arc<Budget>,
    /// of `held`: the remembered lookups and lists
    cached: usize,
}

/// Lookups in the base tier with at most this many facts are remembered by the step that
/// made them, so that later candidates with the same bindings do not read the index
/// again.
const REMEMBER_FACTS: usize = 64;
/// Lookups a step remembers before it forgets them all.
const REMEMBER_LOOKUPS: usize = 4096;
/// The bytes of one remembered lookup or list besides its facts: its map entry, with
/// the table's load, and the shared slice's counts.
const REMEMBERED_ENTRY: usize = 64;
const TRIPLE: usize = std::mem::size_of::<Triple>();

impl Drop for Eval<'_> {
    fn drop(&mut self) {
        if let Some(b) = self.g.base() {
            b.add_scans(self.memos.iter().map(|m| m.scans).sum::<u64>() + self.bmemo.scans);
        }
    }
}

impl<'a> Eval<'a> {
    /// The charges of the evaluation's buffers, made at the first one: most evaluations
    /// of a backward search charge nothing, and a holder shares the budget's count.
    #[inline]
    fn held(&mut self) -> &mut Held {
        let budget = self.budget;
        self.held.get_or_insert_with(|| Held::new(budget))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        g: &'a Graph,
        terms: &'a Terms,
        rule: &'a CRule,
        steps: &'a [Step],
        end: u32,
        abort: &'a AtomicBool,
        cancel: Option<&'a AtomicBool>,
        max_out: usize,
        mode: Mode<'a>,
        budget: &'a Arc<Budget>,
    ) -> Eval<'a> {
        Eval {
            held: None,
            budget,
            cached: 0,
            g,
            terms,
            rule,
            steps,
            end,
            abort,
            cancel,
            out: Vec::new(),
            max_out,
            ticks: 0,
            mode,
            trail: smallvec::SmallVec::new(),
            implicit: smallvec::SmallVec::new(),
            stop: false,
            seen: FxHashSet::default(),
            err: None,
            memos: Vec::new(),
            looked: Vec::new(),
            scratch: Vec::new(),
            bmemo: Memo::default(),
            lists: FxHashMap::default(),
            collect_at: None,
            collected: Vec::new(),
        }
    }

    /// The members of the RDF list at `head` among the visible rows, or `None` when it
    /// is not a well-formed list.
    pub fn list(&mut self, head: u64) -> Option<std::rc::Rc<[u64]>> {
        if let Some(l) = self.lists.get(&head) {
            return l.clone();
        }
        let (g, t) = (self.g, self.terms);
        let l = g
            .list(
                &mut self.bmemo,
                head,
                t.rdf_first,
                t.rdf_rest,
                t.rdf_nil,
                self.end,
            )
            .map(std::rc::Rc::<[u64]>::from);
        if self.lists.len() >= REMEMBER_LOOKUPS {
            self.forget();
        }
        let bytes = l.as_ref().map_or(0, |m| m.len() * 8 + 16) + REMEMBERED_ENTRY;
        if let Err(e) = self.held().try_grow(bytes) {
            self.fail(e);
            return None;
        }
        self.cached += bytes;
        self.lists.insert(head, l.clone());
        l
    }

    /// Forget the remembered lookups and lists, releasing their charge.
    fn forget(&mut self) {
        for l in &mut self.looked {
            l.clear();
        }
        self.lists.clear();
        let cached = self.cached;
        self.held().shrink(cached);
        self.cached = 0;
    }

    /// Stop the evaluation with a memory error (the first one is kept), and the other
    /// threads of the iteration with it.
    pub fn fail(&mut self, e: MemoryExceeded) {
        let e = e.with_context(format!(" (rule '{}')", self.rule.name));
        self.err.get_or_insert(anyhow::Error::new(e));
        self.stop = true;
        self.abort.store(true, Ordering::Relaxed);
    }

    /// Add a candidate to the output, charging the buffer's growth first (the new
    /// allocation while the old one is alive); false when it does not fit, and the
    /// evaluation stops.
    pub fn push_out(&mut self, t: Triple) -> bool {
        let cap = self.out.capacity();
        if self.out.len() == cap {
            let next = (cap * 2).max(64);
            if let Err(e) = self.held().try_grow(next * TRIPLE) {
                self.fail(e);
                return false;
            }
            self.out.reserve_exact(next - self.out.len());
            // the old buffer is freed
            self.held().shrink(cap * TRIPLE);
            let spare = self.out.capacity() - next;
            self.held().grow(spare * TRIPLE);
        }
        self.out.push(t);
        true
    }

    /// Whether `t` was not emitted yet, remembering it; the set's growth is charged
    /// first. `None` when it does not fit, and the evaluation stops.
    fn see(&mut self, t: Triple) -> Option<bool> {
        let (cap, len) = (self.seen.capacity(), self.seen.len());
        if len == cap {
            let next = table_bytes(table_grown(cap, len, 1), TRIPLE);
            if let Err(e) = self.held().try_grow(next) {
                self.fail(e);
                return None;
            }
            self.seen.reserve(1);
            self.held().shrink(table_bytes(cap, TRIPLE) + next);
            let cap = self.seen.capacity();
            self.held().grow(table_bytes(cap, TRIPLE));
        }
        Some(self.seen.insert(t))
    }

    /// The candidates, with the charge of their buffer.
    pub fn take_out(&mut self) -> (Vec<Triple>, Held) {
        let out = std::mem::take(&mut self.out);
        let held = self.held().split(out.capacity() * TRIPLE);
        (out, held)
    }

    /// Whether `t` is among the visible facts.
    pub fn holds(&mut self, t: &Triple) -> bool {
        let g = self.g;
        g.holds_before(&mut self.bmemo, t, self.end)
    }

    /// Whether a visible fact matches the pattern.
    pub fn exists(&mut self, s: Option<u64>, p: Option<u64>, o: Option<u64>) -> bool {
        let g = self.g;
        g.exists(&mut self.bmemo, s, p, o, self.end)
    }

    /// Whether the evaluation reports rule instances (and so tracks their bodies).
    #[inline]
    pub fn collecting(&self) -> bool {
        matches!(self.mode, Mode::Instances { .. })
    }

    #[inline]
    pub fn val(&self, s: Slot, b: &[u64]) -> u64 {
        match s {
            Slot::Var(v) => b[v],
            Slot::Const(c) => c,
            Slot::Any => 0,
        }
    }

    #[inline]
    fn opt(&self, s: Slot, b: &[u64]) -> Option<u64> {
        match self.val(s, b) {
            0 => None,
            x => Some(x),
        }
    }

    fn aborted(&mut self) -> bool {
        if self.stop {
            return true;
        }
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks & 0x3FF == 0
            && (self.abort.load(Ordering::Relaxed)
                || self.cancel.is_some_and(|c| c.load(Ordering::Relaxed)))
        {
            return true;
        }
        self.out.len() > self.max_out
    }

    /// Run the plan from step `k`; for `k == 0` with an atom, `first` overrides its
    /// candidates (used to split work across threads).
    pub fn run(&mut self, k: usize, b: &mut [u64], first: Option<First<'_>>) {
        if let Some((d, cap)) = self.collect_at
            && d == k
        {
            if self.collected.len() < cap {
                // the bindings and their slot, with room for the vector's doubling
                if let Err(e) = self
                    .held()
                    .try_grow(b.len() * 8 + 2 * size_of::<Vec<u64>>())
                {
                    self.fail(e);
                    return;
                }
                self.collected.push(b.to_vec());
            } else {
                self.stop = true;
            }
            return;
        }
        if k == self.steps.len() {
            self.emit(b);
            return;
        }
        match self.steps[k] {
            Step::Atom { atom, lo, hi } => {
                let a = &self.rule.atoms[atom];
                let (s, p, o) = (self.opt(a.s, b), self.opt(a.p, b), self.opt(a.o, b));
                match first {
                    Some(First::Rows(c)) => self.rows(k, atom, b, c),
                    Some(First::Base(r)) => self.base_facts(k, atom, b, Some(&r), [s, p, o]),
                    None => {
                        let c = self.g.cands(s, p, o, lo, hi);
                        let base = self.g.reads_base(lo, hi);
                        if base && self.rows_first(c.len(), [s, p, o]) {
                            self.rows(k, atom, b, c);
                            if self.stop {
                                return;
                            }
                            self.base_facts(k, atom, b, None, [s, p, o]);
                            return;
                        }
                        if base {
                            self.base_facts(k, atom, b, None, [s, p, o]);
                            if self.stop {
                                return;
                            }
                        }
                        self.rows(k, atom, b, c);
                    }
                }
            }
            Step::Builtin(j) => crate::builtins::eval(self, j, k, b),
        }
    }

    /// Whether a backward search reads an atom's in-memory candidates before its facts in
    /// the base tier: when there are some, and the base tier's lookup is long. The facts of
    /// the inferred graph share the input graphs' permutations, so a lookup without a
    /// bound subject can read many blocks of the store's own inferences to find few input
    /// facts, or none, while the derived tier often holds an instance that proves the head
    /// at once. A search explores every instance before it calls a fact unprovable, so the
    /// order changes how soon it stops and never its answer. Rules evaluated forward keep
    /// the base tier first, the order of their output.
    #[inline]
    fn rows_first(&self, rows: usize, [s, p, o]: [Option<u64>; 3]) -> bool {
        rows > 0
            && self.collecting()
            && self
                .g
                .base()
                .is_some_and(|b| b.lookups().is_some() && b.blocks(s, p, o) > 1)
    }

    /// Match the atom of step `k` against in-memory rows.
    fn rows(&mut self, k: usize, atom: usize, b: &mut [u64], cands: Cands<'_>) {
        for j in 0..cands.len() {
            if self.aborted() {
                return;
            }
            let row = cands.get(j);
            if !self.g.alive(row) {
                continue;
            }
            let t = self.g.triple(row);
            self.fact(k, atom, b, t);
        }
    }

    /// Match the atom of step `k` against the base tier, or a share of its scan.
    fn base_facts(
        &mut self,
        k: usize,
        atom: usize,
        b: &mut [u64],
        r: Option<&BaseRange>,
        [s, p, o]: [Option<u64>; 3],
    ) {
        let g: &'a Graph = self.g;
        let Some(base) = g.base() else { return };
        if r.is_none() && base.lookups().is_some() && self.collecting() {
            // the tier remembers its lookups, and the evaluations of a backward search
            // are short: they read the tier's lookups directly, with one memo for every
            // step
            let mut memo = std::mem::take(&mut self.bmemo);
            let res = base.for_each_with(&mut memo, s, p, o, |t| {
                if self.aborted() {
                    return false;
                }
                self.fact(k, atom, b, t);
                !self.stop
            });
            // a nested lookup may have put its memo back meanwhile: its scans count
            memo.scans += self.bmemo.scans;
            self.bmemo = memo;
            if let Err(e) = res {
                self.err.get_or_insert(e);
                self.stop = true;
            }
            return;
        }
        if self.memos.is_empty() {
            self.memos.resize_with(self.steps.len(), Memo::default);
        }
        if self.looked.is_empty() {
            self.looked
                .resize_with(self.steps.len(), FxHashMap::default);
            self.scratch.resize_with(self.steps.len(), Vec::new);
        }
        let key = [s.unwrap_or(0), p.unwrap_or(0), o.unwrap_or(0)];
        if r.is_none()
            && let Some(facts) = self.looked[k].get(&key).cloned()
        {
            for &t in facts.iter() {
                if self.aborted() {
                    return;
                }
                self.fact(k, atom, b, t);
            }
            return;
        }
        let mut memo = std::mem::take(&mut self.memos[k]);
        // the facts of a lookup, while they are few
        let mut few = std::mem::take(&mut self.scratch[k]);
        few.clear();
        // whether the lookup has too many facts to remember (a share of a scan is not
        // remembered)
        let mut many = r.is_some();
        let mut each = |t: Triple| {
            if self.aborted() {
                return false;
            }
            if !many {
                if few.len() < REMEMBER_FACTS {
                    few.push(t);
                } else {
                    many = true;
                }
            }
            self.fact(k, atom, b, t);
            !self.stop
        };
        let res = match r {
            Some(r) => base.for_range(&mut memo, r, s, p, o, &mut each),
            None => base.for_each_with(&mut memo, s, p, o, &mut each),
        };
        self.memos[k] = memo;
        match res {
            Ok(true) if !many => {
                if self.looked[k].len() >= REMEMBER_LOOKUPS {
                    self.forget();
                }
                let bytes = few.len() * TRIPLE + 16 + REMEMBERED_ENTRY;
                match self.held().try_grow(bytes) {
                    Ok(()) => {
                        self.cached += bytes;
                        self.looked[k].insert(key, few.as_slice().into());
                    }
                    Err(e) => self.fail(e),
                }
            }
            Ok(_) => {}
            Err(e) => {
                self.err.get_or_insert(e);
                self.stop = true;
            }
        }
        self.scratch[k] = few;
    }

    /// Unify the atom of step `k` with fact `t` and continue.
    #[inline]
    fn fact(&mut self, k: usize, atom: usize, b: &mut [u64], t: Triple) {
        let a = &self.rule.atoms[atom];
        let mut newly = [usize::MAX; 3];
        let mut n = 0;
        let ok = unify(a.s, t[0], b, &mut newly, &mut n)
            && unify(a.p, t[1], b, &mut newly, &mut n)
            && unify(a.o, t[2], b, &mut newly, &mut n);
        if ok {
            if self.collecting() {
                self.trail.push(t);
                self.run(k + 1, b, None);
                self.trail.pop();
            } else {
                self.run(k + 1, b, None);
            }
        }
        for &v in &newly[..n] {
            b[v] = 0;
        }
    }

    /// Continue with a variable bound to `value` (or check it if already bound).
    pub fn bind_and_continue(&mut self, slot: Slot, value: u64, k: usize, b: &mut [u64]) {
        match slot {
            Slot::Var(v) if b[v] == 0 => {
                b[v] = value;
                self.run(k + 1, b, None);
                b[v] = 0;
            }
            s => {
                let cur = self.val(s, b);
                if s == Slot::Any
                    || cur == value
                    || crate::builtins::same_value(self.terms, cur, value)
                {
                    self.run(k + 1, b, None);
                }
            }
        }
    }

    fn emit(&mut self, b: &[u64]) {
        let rule = self.rule;
        if let Mode::Instances { head, target, .. } = &self.mode {
            if let Some(CHead::Triple(s, p, o)) = rule.head.get(*head) {
                let t = [self.val(*s, b), self.val(*p, b), self.val(*o, b)];
                if t == *target {
                    let Eval {
                        mode,
                        trail,
                        implicit,
                        stop,
                        ..
                    } = self;
                    if let Mode::Instances { sink, .. } = mode
                        && !sink(trail, implicit)
                    {
                        *stop = true;
                    }
                }
            }
            return;
        }
        let existing = matches!(self.mode, Mode::Existing);
        for h in &rule.head {
            match h {
                CHead::Triple(s, p, o) => {
                    let t = [self.val(*s, b), self.val(*p, b), self.val(*o, b)];
                    let emit = if existing {
                        self.g.contains(&t)
                    } else if self.g.known(&t) {
                        false
                    } else {
                        match self.see(t) {
                            Some(new) => new,
                            None => return,
                        }
                    };
                    if emit && !self.push_out(t) {
                        return;
                    }
                }
                CHead::Action(action, args) => {
                    let vals: Vec<u64> = args.iter().map(|a| self.val(*a, b)).collect();
                    crate::builtins::head_action(self, *action, &vals);
                }
            }
        }
    }
}

#[inline]
fn unify(s: Slot, v: u64, b: &mut [u64], newly: &mut [usize; 3], n: &mut usize) -> bool {
    match s {
        Slot::Const(c) => c == v,
        Slot::Any => true,
        Slot::Var(x) => {
            if b[x] == 0 {
                b[x] = v;
                newly[*n] = x;
                *n += 1;
                true
            } else {
                b[x] == v
            }
        }
    }
}

// ================================================================= fixpoint ====

pub(crate) struct Limits {
    pub max_iterations: usize,
    pub max_inferred: usize,
    pub cancel: Option<Arc<AtomicBool>>,
    pub progress: Option<crate::ProgressFn>,
    /// the run's memory, against [`crate::ReasonOptions::max_memory`]
    pub budget: Arc<Budget>,
}

pub(crate) struct Outcome {
    pub iterations: usize,
}

/// In-memory rows per work item.
const CHUNK: usize = 2048;
/// Base-tier rows per work item.
const BASE_CHUNK: usize = 4096;

/// A share of an iteration's evaluation: rule `ri`'s plan from step `k`, with each of
/// the bindings `b` in turn (none bound when there are none), and the candidates of step
/// `k` restricted to `first`.
struct Item<'a> {
    ri: usize,
    steps: &'a [Step],
    k: usize,
    b: Vec<Vec<u64>>,
    first: Option<First<'a>>,
}

/// The shares of the candidates of plan step `k` (an atom) with the bindings `b`: base
/// tier ranges, then in-memory row chunks. A step that is not an atom is one share.
fn shares<'g>(
    g: &'g Graph,
    r: &CRule,
    steps: &[Step],
    k: usize,
    b: &[u64],
) -> anyhow::Result<Vec<Option<First<'g>>>> {
    let Some(&Step::Atom { atom, lo, hi }) = steps.get(k) else {
        return Ok(vec![None]);
    };
    let a = &r.atoms[atom];
    let v = |s: Slot| match s {
        Slot::Const(c) => Some(c),
        Slot::Var(x) if b[x] != 0 => Some(b[x]),
        _ => None,
    };
    let (s, p, o) = (v(a.s), v(a.p), v(a.o));
    let mut out = Vec::new();
    if let Some(base) = g.base()
        && g.reads_base(lo, hi)
    {
        out.extend(
            base.ranges(s, p, o, BASE_CHUNK)?
                .into_iter()
                .map(|x| Some(First::Base(x))),
        );
    }
    let cands = g.cands(s, p, o, lo, hi);
    if cands.len() > 0 {
        out.extend(
            cands
                .chunks(CHUNK)
                .into_iter()
                .map(|c| Some(First::Rows(c))),
        );
    }
    Ok(out)
}

/// Bindings worked out up to plan step `d` before the work is split there.
const MAX_SPLIT_BINDINGS: usize = 1 << 16;

/// The fewest bindings a work item of a split evaluates in turn.
const MIN_BATCH: usize = 8;

/// The work of a plan split at step `d` (an atom): the bindings of the steps before it,
/// from the shares `first` of step 0, in the order a sequential evaluation visits them,
/// and the charge of the bindings. `None` when there are too many bindings.
///
/// A binding whose candidates at step `d` are many gets an item per share of them. The
/// others are evaluated in turn by items of [`MIN_BATCH`] or more consecutive bindings,
/// about `per_item` each, which read step `d` as any later step does: through the
/// remembered lookups and the blocks the item read last. Cutting each binding's
/// candidates into shares would cost a read of the index for each, and an evaluation of
/// its own for each share, which is most of the time of a small incremental run.
#[allow(clippy::too_many_arguments)]
fn split_at<'g>(
    g: &'g Graph,
    terms: &Terms,
    r: &CRule,
    steps: &'g [Step],
    ri: usize,
    d: usize,
    first: &[Option<First<'g>>],
    abort: &AtomicBool,
    limits: &Limits,
    items_wanted: usize,
) -> anyhow::Result<Option<(Vec<Item<'g>>, Held)>> {
    let per = r.nvars * 8 + 2 * size_of::<Vec<u64>>();
    type Part = anyhow::Result<Option<(Vec<Vec<u64>>, Held)>>;
    let parts: Vec<Part> = first
        .par_iter()
        .map(|f| {
            let mut ev = Eval::new(
                g,
                terms,
                r,
                steps,
                g.len(),
                abort,
                limits.cancel.as_deref(),
                usize::MAX,
                Mode::New,
                &limits.budget,
            );
            ev.collect_at = Some((d, MAX_SPLIT_BINDINGS));
            let mut b = vec![0u64; r.nvars];
            ev.run(0, &mut b, *f);
            if let Some(e) = ev.err.take() {
                return Err(e);
            }
            let collected = std::mem::take(&mut ev.collected);
            // the bindings keep their charge
            let held = ev.held().split(collected.len() * per);
            Ok((collected.len() < MAX_SPLIT_BINDINGS).then_some((collected, held)))
        })
        .collect();
    let mut bindings = Vec::new();
    let mut held = Held::new(&limits.budget);
    for p in parts {
        match p? {
            Some((v, h)) => {
                bindings.extend(v);
                held.absorb(h);
            }
            None => return Ok(None),
        }
        if bindings.len() >= MAX_SPLIT_BINDINGS {
            return Ok(None);
        }
    }
    let per_item = bindings.len().div_ceil(items_wanted.max(1)).max(MIN_BATCH);
    let mut items = Vec::new();
    let mut batch: Vec<Vec<u64>> = Vec::new();
    let flush = |batch: &mut Vec<Vec<u64>>, items: &mut Vec<Item<'g>>, held: &mut Held| {
        if batch.is_empty() {
            return Ok::<(), MemoryExceeded>(());
        }
        held.try_grow(size_of::<Item<'_>>())?;
        items.push(Item {
            ri,
            steps,
            k: d,
            b: std::mem::take(batch),
            first: None,
        });
        Ok(())
    };
    for b in bindings {
        if !many_at(g, r, steps, d, &b) {
            batch.push(b);
            if batch.len() >= per_item {
                flush(&mut batch, &mut items, &mut held)?;
            }
            continue;
        }
        flush(&mut batch, &mut items, &mut held)?;
        for f in shares(g, r, steps, d, &b)? {
            held.try_grow(size_of::<Item<'_>>() + per)?;
            items.push(Item {
                ri,
                steps,
                k: d,
                b: vec![b.clone()],
                first: f,
            });
        }
    }
    flush(&mut batch, &mut items, &mut held)?;
    Ok(Some((items, held)))
}

/// Whether the candidates of plan step `k` (an atom) with the bindings `b` may be many:
/// the base tier's key range spans more than one block, or the in-memory rows are more
/// than a share of them.
fn many_at(g: &Graph, r: &CRule, steps: &[Step], k: usize, b: &[u64]) -> bool {
    let Some(&Step::Atom { atom, lo, hi }) = steps.get(k) else {
        return false;
    };
    let a = &r.atoms[atom];
    let v = |s: Slot| match s {
        Slot::Const(c) => Some(c),
        Slot::Var(x) if b[x] != 0 => Some(b[x]),
        _ => None,
    };
    let (s, p, o) = (v(a.s), v(a.p), v(a.o));
    if let Some(base) = g.base()
        && g.reads_base(lo, hi)
        && base.blocks(s, p, o) > 1
    {
        return true;
    }
    g.cands(s, p, o, lo, hi).len() > CHUNK
}

/// What a work item gave: its rule, its candidates with their charge, and its error.
type Done = (usize, Vec<Triple>, Held, Option<anyhow::Error>);

/// Run the rules to a fixpoint. Derived triples are appended to `g`.
///
/// With `start`, the rows before it are already closed under the rules and the rows from
/// it on are new: evaluation is semi-naive from the first iteration.
///
/// Every buffer and table the evaluation and the merges allocate is charged to
/// `limits.budget` before it grows. The first charge past the limit stops the
/// iteration's other work items at their next check and fails the run with the rule
/// that was being evaluated.
pub(crate) fn run(
    g: &mut Graph,
    rules: &[CRule],
    terms: &Terms,
    limits: &Limits,
    start: Option<u32>,
) -> anyhow::Result<Outcome> {
    let base_len = g.len();
    let abort = AtomicBool::new(false);
    let cancelled = || {
        limits
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    };
    let mut iteration = 0usize;
    let (mut ds, mut de) = (start.unwrap_or(0).min(base_len), base_len);
    let memory = |e: MemoryExceeded, iteration: usize| -> anyhow::Error {
        anyhow::Error::new(e.with_context(format!(" (in iteration {})", iteration + 1)))
    };
    loop {
        if cancelled() {
            anyhow::bail!("reasoning cancelled");
        }
        if iteration >= limits.max_iterations {
            anyhow::bail!(
                "reasoning did not reach a fixpoint within {} iterations ({} triples derived so far)",
                limits.max_iterations,
                g.len() - base_len
            );
        }
        limits.budget.check().map_err(|e| memory(e, iteration))?;
        if let Some(p) = &limits.progress {
            let f = 0.1 + 0.7 * (1.0 - 1.0 / (iteration as f32 + 1.0));
            p(
                f,
                &format!(
                    "iteration {} ({} derived)",
                    iteration + 1,
                    g.len() - base_len
                ),
            );
        }
        // ---- plans for this iteration
        let t_iter = std::time::Instant::now();
        let mut tasks: Vec<(usize, Vec<Step>)> = Vec::new();
        // a join with an atom whose constant predicate has no rows in its range is empty
        let empty = |r: &CRule, ranges: &[(u32, u32)], g: &Graph| {
            r.atoms.iter().zip(ranges).any(|(a, &(lo, hi))| match a.p {
                Slot::Const(p) => g.count_p(p, lo, hi) == 0,
                _ => lo >= hi,
            })
        };
        for (ri, r) in rules.iter().enumerate() {
            if iteration == 0 && start.is_none() {
                // naive evaluation over the base data (old = ∅)
                let ranges = vec![(0, de); r.atoms.len()];
                if empty(r, &ranges, g) {
                    continue;
                }
                if let Some(p) = plan(r, &ranges, Some(g)) {
                    tasks.push((ri, p));
                }
                continue;
            }
            // semi-naive: atom d reads the delta, atoms before it the old rows, atoms
            // after it everything
            for d in 0..r.atoms.len() {
                let a = &r.atoms[d];
                let has_delta = match a.p {
                    Slot::Const(p) => g.count_p(p, ds, de) > 0,
                    _ => de > ds,
                };
                if !has_delta {
                    continue;
                }
                let ranges: Vec<(u32, u32)> = (0..r.atoms.len())
                    .map(|i| match i.cmp(&d) {
                        std::cmp::Ordering::Less => (0, ds),
                        std::cmp::Ordering::Equal => (ds, de),
                        std::cmp::Ordering::Greater => (0, de),
                    })
                    .collect();
                if empty(r, &ranges, g) {
                    continue;
                }
                if let Some(p) = plan(r, &ranges, Some(g)) {
                    tasks.push((ri, p));
                }
            }
        }
        for (ri, p) in &tasks {
            let (s, o) = index_needs(&rules[*ri], p);
            if s {
                g.ensure_index(true, true)
                    .map_err(|e| memory(e, iteration))?;
            }
            if o {
                g.ensure_index(false, true)
                    .map_err(|e| memory(e, iteration))?;
            }
        }
        // ---- split into parallel work items
        let gr: &Graph = g;
        let mut work: Vec<Item<'_>> = Vec::new();
        // the charge of the split bindings, for the iteration
        let mut split_held = Held::new(&limits.budget);
        let split_min = rayon::current_num_threads().max(1) * 4;
        for (ri, p) in &tasks {
            let r = &rules[*ri];
            let first = shares(gr, r, p, 0, &vec![0; r.nvars])?;
            // a first step with few candidates followed by a large join runs on one
            // thread: then the bindings up to the next atom are worked out first, and the
            // work is split on that atom's candidates
            let deeper = (first.len() < split_min)
                .then(|| {
                    p.iter()
                        .skip(1)
                        .position(|s| matches!(s, Step::Atom { .. }))
                })
                .flatten()
                .map(|d| d + 1);
            if let Some(d) = deeper {
                let split = split_at(gr, terms, r, p, *ri, d, &first, &abort, limits, split_min)
                    .map_err(|e| match e.downcast::<MemoryExceeded>() {
                        Ok(m) => memory(m, iteration),
                        Err(e) => e,
                    })?;
                if let Some((items, held)) = split {
                    work.extend(items);
                    split_held.absorb(held);
                    continue;
                }
            }
            work.extend(first.into_iter().map(|f| Item {
                ri: *ri,
                steps: p.as_slice(),
                k: 0,
                b: Vec::new(),
                first: f,
            }));
        }
        let produced = AtomicUsize::new(0);
        let budget = limits
            .max_inferred
            .saturating_sub((gr.len() - base_len) as usize);
        // the candidates an evaluation may produce for the limit of inferred triples;
        // memory is checked by the charges as the candidates arrive
        let max_out = budget.saturating_mul(4).saturating_add(1 << 20);
        let timed = tracing::enabled!(tracing::Level::TRACE);
        let item_time: Vec<AtomicUsize> = if timed {
            rules.iter().map(|_| AtomicUsize::new(0)).collect()
        } else {
            Vec::new()
        };
        let t_eval = std::time::Instant::now();
        let results: Vec<Done> = work
            .par_iter()
            .map(|item| {
                let ri = &item.ri;
                // an item that starts after another failed or the run was cancelled
                // does nothing
                if abort.load(Ordering::Relaxed) || cancelled() {
                    return (*ri, Vec::new(), Held::new(&limits.budget), None);
                }
                let t_item = timed.then(std::time::Instant::now);
                let r = &rules[*ri];
                let mut ev = Eval::new(
                    gr,
                    terms,
                    r,
                    item.steps,
                    de,
                    &abort,
                    limits.cancel.as_deref(),
                    max_out,
                    Mode::New,
                    &limits.budget,
                );
                if item.b.is_empty() {
                    ev.run(item.k, &mut vec![0u64; r.nvars], item.first);
                }
                for b in &item.b {
                    if ev.err.is_some() || abort.load(Ordering::Relaxed) {
                        break;
                    }
                    ev.run(item.k, &mut b.clone(), item.first);
                }
                let total = produced.fetch_add(ev.out.len(), Ordering::Relaxed) + ev.out.len();
                if total > max_out || ev.err.is_some() {
                    abort.store(true, Ordering::Relaxed);
                }
                if let Some(t) = t_item {
                    item_time[*ri].fetch_add(t.elapsed().as_micros() as usize, Ordering::Relaxed);
                }
                let (out, held) = ev.take_out();
                (*ri, out, held, ev.err.take())
            })
            .collect();
        drop(split_held);
        if timed {
            // the threads' busy time against the wall time of the evaluation: the rest is
            // the wait at the iteration's barrier, and scheduling
            let busy: usize = item_time.iter().map(|t| t.load(Ordering::Relaxed)).sum();
            let wall = t_eval.elapsed().as_micros() as usize;
            tracing::trace!(
                iteration,
                busy_ms = busy / 1000,
                wall_ms = wall / 1000,
                threads = rayon::current_num_threads(),
                "evaluation busy time"
            );
            let mut by_rule: Vec<(usize, usize)> = item_time
                .iter()
                .enumerate()
                .map(|(i, t)| (t.load(Ordering::Relaxed), i))
                .filter(|x| x.0 > 0)
                .collect();
            by_rule.sort_unstable_by(|a, b| b.cmp(a));
            for (us, ri) in by_rule.into_iter().take(5) {
                let items = work.iter().filter(|w| w.ri == ri).count();
                tracing::trace!(iteration, rule = %rules[ri].name, ms = us / 1000, items, "rule time");
            }
        }
        // the first error in the order of the work items: a memory error as it is, with
        // the iteration, and an error of a base-tier read with what was being read
        let mut outs_held = Held::new(&limits.budget);
        let mut results: Vec<(usize, Vec<Triple>)> = {
            let mut ok = Vec::with_capacity(results.len());
            for (ri, out, held, err) in results {
                if let Some(e) = err {
                    return Err(match e.downcast::<MemoryExceeded>() {
                        Ok(m) => memory(m, iteration),
                        Err(e) => e.context("reading the input graphs"),
                    });
                }
                outs_held.absorb(held);
                ok.push((ri, out));
            }
            ok
        };
        let top_rule = |results: &[(usize, Vec<Triple>)]| {
            let mut per: FxHashMap<usize, usize> = FxHashMap::default();
            for (ri, out) in results {
                *per.entry(*ri).or_default() += out.len();
            }
            per.into_iter()
                .max_by_key(|x| (x.1, std::cmp::Reverse(x.0)))
                .map_or(String::new(), |(ri, n)| {
                    format!(
                        " (rule '{}' produced {n} candidate triples in iteration {})",
                        rules[ri].name,
                        iteration + 1
                    )
                })
        };
        if cancelled() {
            anyhow::bail!("reasoning cancelled");
        }
        let candidates = produced.load(Ordering::Relaxed);
        if candidates > max_out {
            return Err(crate::InferredLimitExceeded {
                limit: limits.max_inferred,
                context: top_rule(&results),
            }
            .into());
        }
        let overflow = if (g.len() - base_len) as usize + candidates > limits.max_inferred {
            top_rule(&results)
        } else {
            String::new()
        };
        // ---- merge
        let eval_ms = t_iter.elapsed().as_millis() as u64;
        let t_merge = std::time::Instant::now();
        let before = g.len();
        let outs: Vec<Vec<Triple>> = results.drain(..).map(|(_, out)| out).collect();
        g.merge(outs, outs_held)
            .map_err(|e| match e.downcast::<MemoryExceeded>() {
                Ok(m) => memory(m.with_context(overflow.clone()), iteration),
                Err(e) => e,
            })?;
        iteration += 1;
        let new = g.len() - before;
        tracing::debug!(
            iteration,
            new,
            candidates,
            eval_ms,
            merge_ms = t_merge.elapsed().as_millis() as u64,
            "reasoner iteration"
        );
        if (g.len() - base_len) as usize > limits.max_inferred {
            return Err(crate::InferredLimitExceeded {
                limit: limits.max_inferred,
                context: overflow,
            }
            .into());
        }
        limits
            .budget
            .check()
            .map_err(|e| memory(e.with_context(overflow.clone()), iteration - 1))?;
        if new == 0 {
            break;
        }
        ds = de;
        de = g.len();
    }
    Ok(Outcome {
        iterations: iteration,
    })
}
// ============================================================== incremental ====

/// Why a rule keeps a materialization from being maintained incrementally, if it does.
///
/// Incremental maintenance needs rules whose instances depend on the facts their atoms
/// match and on nothing else that can change: `noValue` is negation, `now` gives another
/// value in each run, `makeTemp` / `makeSkolem` and blank node constants create fresh
/// blank nodes in each run, head actions derive facts from lists their body never
/// matches, and a `listForAll` reads facts that no atom of the rule matches unless a
/// `listMember` and an atom over the same list cover them (as in `cls-int1`).
pub(crate) fn incremental_blocker(r: &CRule, terms: &Terms) -> Option<String> {
    use crate::builtins::BuiltinKind as B;
    for b in &r.builtins {
        let why = match b.kind {
            B::NoValue => "noValue is not monotonic",
            B::Now => "now gives a new value in each run",
            B::MakeTemp | B::MakeSkolem => "it creates blank nodes",
            B::ListForAll if !list_for_all_covered(r, b) => {
                "its listForAll reads facts that no atom of the rule matches"
            }
            _ => continue,
        };
        return Some(format!("rule '{}': {why}", r.name));
    }
    for h in &r.head {
        match h {
            CHead::Action(..) => {
                return Some(format!(
                    "rule '{}': head actions derive facts from lists",
                    r.name
                ));
            }
            CHead::Triple(s, p, o) => {
                let bnode = [s, p, o].into_iter().any(|x| match x {
                    Slot::Const(c) => {
                        sparkles_core::id::Id(*c).tag() == sparkles_core::id::Tag::Local
                            && terms.kind(*c) == crate::terms::Kind::BNode
                    }
                    _ => false,
                });
                if bnode {
                    return Some(format!(
                        "rule '{}': a blank node in the head is a new blank node in each run",
                        r.name
                    ));
                }
            }
        }
    }
    None
}

/// `listForAll(?l, ?y, P)` is covered when the rule also has `listMember(?l, ?m)` and
/// the atom `(?y P ?m)`: every fact it reads is then matched by that atom in some
/// instance.
fn list_for_all_covered(r: &CRule, b: &CBuiltin) -> bool {
    let &[l @ Slot::Var(_), y, p] = &b.args[..] else {
        return false;
    };
    r.builtins.iter().any(|m| {
        m.kind == crate::builtins::BuiltinKind::ListMember
            && m.args[0] == l
            && matches!(m.args[1], Slot::Var(_))
            && r.atoms
                .iter()
                .any(|a| a.s == y && a.p == p && a.o == m.args[1])
    })
}

/// Whether a rule reads RDF lists through builtins.
pub(crate) fn reads_lists(r: &CRule) -> bool {
    use crate::builtins::BuiltinKind as B;
    r.builtins.iter().any(|b| {
        matches!(
            b.kind,
            B::ListMember
                | B::ListContains
                | B::ListNotContains
                | B::ListLength
                | B::ListEntry
                | B::ListForAll
        )
    })
}
