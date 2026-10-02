//! Rule compilation and semi-naive forward chaining over id-level triples.

use crate::builtins::{BuiltinKind, HeadAction};
use crate::graph::{Cands, Graph, Triple};
use crate::parser::{BuiltinCall, Clause, Direction, Node, Rule};
use crate::terms::{Terms, anchored_regex};
use oxrdf::Term;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::collections::HashSet;
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

/// Receives the body of a rule instance: the rows its atoms matched and the facts its
/// builtins read (`listForAll`). Returns false to stop the evaluation.
pub(crate) type Sink<'s> = dyn FnMut(&[u32], &[Triple]) -> bool + 's;

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
    /// rows matched by the atoms of the current partial instance ([`Mode::Instances`])
    trail: Vec<u32>,
    /// facts read by builtins of the current partial instance ([`Mode::Instances`])
    pub implicit: Vec<Triple>,
    stop: bool,
}

impl<'a> Eval<'a> {
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
    ) -> Eval<'a> {
        Eval {
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
            trail: Vec::new(),
            implicit: Vec::new(),
            stop: false,
        }
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
    /// candidate list (used to split work across threads).
    pub fn run(&mut self, k: usize, b: &mut [u64], first: Option<Cands<'_>>) {
        if k == self.steps.len() {
            self.emit(b);
            return;
        }
        match self.steps[k] {
            Step::Atom { atom, lo, hi } => {
                let a = &self.rule.atoms[atom];
                let cands = match first {
                    Some(c) => c,
                    None => {
                        self.g
                            .cands(self.opt(a.s, b), self.opt(a.p, b), self.opt(a.o, b), lo, hi)
                    }
                };
                let (as_, ap, ao) = (a.s, a.p, a.o);
                let collecting = self.collecting();
                for j in 0..cands.len() {
                    if self.aborted() {
                        return;
                    }
                    let row = cands.get(j);
                    if !self.g.alive(row) {
                        continue;
                    }
                    let t = self.g.triples[row as usize];
                    let mut newly = [usize::MAX; 3];
                    let mut n = 0;
                    let ok = unify(as_, t[0], b, &mut newly, &mut n)
                        && unify(ap, t[1], b, &mut newly, &mut n)
                        && unify(ao, t[2], b, &mut newly, &mut n);
                    if ok {
                        if collecting {
                            self.trail.push(row);
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
            }
            Step::Builtin(j) => crate::builtins::eval(self, j, k, b),
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
                    if self.g.position(&t).is_some() == existing {
                        self.out.push(t);
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
}

pub(crate) struct Outcome {
    pub iterations: usize,
}

const CHUNK: usize = 2048;

/// Run the rules to a fixpoint. Derived triples are appended to `g`.
///
/// With `start`, the rows before it are already closed under the rules and the rows from
/// it on are new: evaluation is semi-naive from the first iteration.
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
                g.ensure_s_index();
            }
            if o {
                g.ensure_o_index();
            }
        }
        // ---- split into parallel work items
        let gr: &Graph = g;
        let mut work: Vec<(usize, &[Step], Option<Cands<'_>>)> = Vec::new();
        for (ri, p) in &tasks {
            let r = &rules[*ri];
            match p.first() {
                Some(Step::Atom { atom, lo, hi }) => {
                    let a = &r.atoms[*atom];
                    let c = |s: Slot| match s {
                        Slot::Const(c) => Some(c),
                        _ => None,
                    };
                    let cands = gr.cands(c(a.s), c(a.p), c(a.o), *lo, *hi);
                    if cands.len() == 0 {
                        continue;
                    }
                    for ch in cands.chunks(CHUNK) {
                        work.push((*ri, p.as_slice(), Some(ch)));
                    }
                }
                _ => work.push((*ri, p.as_slice(), None)),
            }
        }
        let produced = AtomicUsize::new(0);
        let budget = limits
            .max_inferred
            .saturating_sub((gr.len() - base_len) as usize);
        let max_out = budget.saturating_mul(4).saturating_add(1 << 20);
        let results: Vec<(usize, Vec<Triple>)> = work
            .par_iter()
            .map(|(ri, steps, first)| {
                let r = &rules[*ri];
                let mut ev = Eval::new(
                    gr,
                    terms,
                    r,
                    steps,
                    de,
                    &abort,
                    limits.cancel.as_deref(),
                    max_out,
                    Mode::New,
                );
                let mut b = vec![0u64; r.nvars];
                ev.run(0, &mut b, *first);
                let total = produced.fetch_add(ev.out.len(), Ordering::Relaxed) + ev.out.len();
                if total > max_out {
                    abort.store(true, Ordering::Relaxed);
                }
                (*ri, ev.out)
            })
            .collect();
        let top_rule = || {
            let mut per: FxHashMap<usize, usize> = FxHashMap::default();
            for (ri, out) in &results {
                *per.entry(*ri).or_default() += out.len();
            }
            per.into_iter()
                .max_by_key(|x| x.1)
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
        if produced.load(Ordering::Relaxed) > max_out {
            anyhow::bail!(
                "reasoning exceeded the limit of {} inferred triples{}",
                limits.max_inferred,
                top_rule()
            );
        }
        let overflow = if (g.len() - base_len) as usize + produced.load(Ordering::Relaxed)
            > limits.max_inferred
        {
            top_rule()
        } else {
            String::new()
        };
        // ---- merge
        let before = g.len();
        g.add_batch(results.into_iter().flat_map(|(_, out)| out));
        iteration += 1;
        let new = g.len() - before;
        tracing::debug!(iteration, new, "reasoner iteration");
        if (g.len() - base_len) as usize > limits.max_inferred {
            anyhow::bail!(
                "reasoning exceeded the limit of {} inferred triples{overflow}",
                limits.max_inferred
            );
        }
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
                        sparkles::id::Id(*c).tag() == sparkles::id::Tag::Local
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
