//! Why not? Abduction over the rules (spec F18 §4).
//!
//! For a triple the rules do not derive, the search lists every rule whose head unifies
//! with the triple and unfolds each backward. Each body atom of an instance is either
//! matched against the closure, which F17's demand engine derives for the atom's pattern
//! the first time the search reads it, or kept as missing. A missing atom that is ground
//! and that rules can derive is unfolded further, up to the unfolding depth, or kept as a
//! fact itself. A missing atom with a variable that nothing binds keeps a placeholder,
//! shown as a blank node `_:y1`. A builtin that fails under the bindings blocks the
//! instance, and the candidate rule names it, with the fact a failing `noValue` found.
//!
//! The hypotheses, sets of missing facts, are kept when they have at most `max_missing`
//! facts and no smaller hypothesis is a subset of them. They are sorted by size, then by
//! the facts that state something about the RDF, RDFS and OWL vocabulary or state
//! `owl:sameAs` (such a hypothesis changes what the built-in terms mean or renames a
//! resource, rather than adding knowledge about the triple's resources), then by the rule
//! instances their derivation needs, then by their placeholders. Each is verified in that order: its facts, placeholders as fresh blank
//! nodes, are added to an overlay of the caller's view and the demand program runs for
//! the triple, uncached. Only hypotheses under which the triple is derived are returned,
//! each with its derivation tree under the overlay, whose hypothesis facts are leaves
//! marked `missing`.

use crate::builtins::BuiltinKind;
use crate::demand::{head_slots, intensional};
use crate::engine::{CHead, CRule, Eval, Mode, Slot};
use crate::explain::{
    DemandInput, ExplainOptions, demand_of, demand_session, search_session, session_error,
};
use crate::facts::Memo;
use crate::graph::{Graph, Triple};
use crate::query::Session;
use crate::terms::Terms;
use oxrdf::{BlankNode, Term};
use rustc_hash::FxHashSet;
pub use sparkles_core::explain::{Derivation, Explained, RuleRef};
use sparkles_core::sparql::reasoning::{Demand, DemandTerm};

/// Missing facts per hypothesis by default, and at most.
pub const DEFAULT_MAX_MISSING: usize = 2;
pub const MAX_MISSING_LIMIT: usize = 4;
/// Hypotheses returned by default.
pub const DEFAULT_MAX_HYPOTHESES: usize = 10;
/// Levels a missing atom is unfolded by default.
pub const DEFAULT_MAX_UNFOLD: u32 = 3;
/// Partial instances the unfolding visits for each rule that can derive the triple,
/// before it stops as incomplete.
const STEPS_PER_RULE: usize = 20_000;
/// Closure facts read per atom pattern; more make the answer incomplete.
const MAX_MATCHES: usize = 256;
/// Passes of the unfolding that ask the demand engine for the patterns the pass before
/// read, all at once.
const MAX_ROUNDS: usize = 3;
/// Hypotheses the search verifies at most.
const MAX_VERIFIED: usize = 64;
/// Hypotheses retained by one unfolding or Cartesian-product step.
const MAX_PARTIAL_HYPOTHESES: usize = 4096;
const GEO_CANDIDATE: usize = usize::MAX;

/// Limits of a why-not answer.
#[derive(Clone, Debug)]
pub struct WhyNotOptions {
    /// the limits of each derivation and tree
    pub explain: ExplainOptions,
    /// missing facts per hypothesis, at most [`MAX_MISSING_LIMIT`]
    pub max_missing: usize,
    /// hypotheses returned
    pub max_hypotheses: usize,
    /// levels a missing atom is unfolded
    pub max_unfold: u32,
}

impl Default for WhyNotOptions {
    fn default() -> WhyNotOptions {
        WhyNotOptions {
            explain: ExplainOptions::default(),
            max_missing: DEFAULT_MAX_MISSING,
            max_hypotheses: DEFAULT_MAX_HYPOTHESES,
            max_unfold: DEFAULT_MAX_UNFOLD,
        }
    }
}

/// A builtin that blocked an instance.
#[derive(Clone, Debug, PartialEq)]
pub struct Blocked {
    /// the call with its bound arguments, such as `noValue(<http://ex.org/bob>, <http://ex.org/died>)`
    pub call: String,
    /// for `noValue`, a fact that it found
    pub fact: Option<[Term; 3]>,
}

/// A rule whose head unifies with the triple.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub rule: RuleRef,
    /// the line of the rule text
    pub line: usize,
    /// the head's variables bound by the triple
    pub unifier: Vec<(String, Term)>,
    /// builtins that failed in the instances found, with no missing fact to undo them
    pub blocked_by: Vec<Blocked>,
}

/// Facts that, added to the data, make the triple entailed.
#[derive(Clone, Debug)]
pub struct Hypothesis {
    /// the missing facts, placeholders as blank nodes `_:y1`, `_:y2`, …
    pub missing: Vec<[Term; 3]>,
    /// the placeholders' labels
    pub placeholders: Vec<String>,
    /// the derivation of the triple under the hypothesis
    pub tree: Derivation,
}

/// A why-not answer.
#[derive(Clone, Debug)]
pub enum WhyNot {
    /// the rules derive the triple
    Entailed,
    NotEntailed {
        rules: Vec<Candidate>,
        hypotheses: Vec<Hypothesis>,
        /// every hypothesis of at most `max_missing` facts within the unfolding depth was
        /// considered
        complete: bool,
    },
}

/// A placeholder in a pattern.
const HOLE: u64 = u64::MAX - (1 << 20);

fn is_hole(x: u64) -> bool {
    x >= HOLE
}

type Pat = [u64; 3];

/// A hypothesis while the search makes it: its facts, and the rule instances its
/// derivation needs.
type Hyp = (Vec<Pat>, u32);
type Combo = (Vec<Pat>, u64, u32);
type CandidateInstance = (usize, Vec<u64>, Vec<(String, Option<Triple>)>);

/// One completion of an instance: its missing atoms as patterns (holes local to it).
struct Completion {
    missing: Vec<Pat>,
}

struct Unfold<'a> {
    session: Session,
    work: crate::budget::Held,
    opts: &'a WhyNotOptions,
    rdf_type: u64,
    geo: Option<(u64, u64)>,
    demanded: FxHashSet<[Option<u64>; 3]>,
    /// the patterns this pass read that no derivation asked for yet
    needed: Vec<[Option<u64>; 3]>,
    /// the facts being unfolded, from the triple down
    path: Vec<Pat>,
    steps: usize,
    /// the steps the current rule of the triple may reach
    cap: usize,
    complete: bool,
    /// a derivation the unfolding asked for passed its limits
    lossy: bool,
}

struct CallBytes(usize);
impl std::fmt::Write for CallBytes {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0 = self.0.checked_add(s.len()).ok_or(std::fmt::Error)?;
        Ok(())
    }
}

fn write_call(
    out: &mut impl std::fmt::Write,
    name: &str,
    args: &[Option<Term>],
) -> std::fmt::Result {
    write!(out, "{name}(")?;
    for (i, arg) in args.iter().enumerate() {
        if i != 0 {
            out.write_str(", ")?;
        }
        match arg {
            Some(term) => write!(out, "{term}")?,
            None => out.write_str("_")?,
        }
    }
    out.write_str(")")
}

/// Measure before rendering, and retain one charged string without joined copies.
fn blocker_call(
    terms: &Terms,
    name: &str,
    args: &[Option<u64>],
    retained: &mut crate::budget::Held,
) -> anyhow::Result<String> {
    let mut temporary = crate::budget::Held::new(retained.budget());
    temporary.try_grow(args.len() * std::mem::size_of::<Option<Term>>())?;
    let mut decoded = Vec::with_capacity(args.len());
    for arg in args {
        let term = arg.and_then(|id| terms.term(id));
        if let Some(t) = &term {
            temporary.try_grow(sparkles_core::explain::term_bytes(t))?;
        }
        decoded.push(term);
    }
    let mut bytes = CallBytes(0);
    write_call(&mut bytes, name, &decoded)?;
    retained.try_grow(bytes.0 + 2 * std::mem::size_of::<(String, Option<Triple>)>())?;
    let mut call = String::with_capacity(bytes.0);
    write_call(&mut call, name, &decoded)?;
    Ok(call)
}

/// A canonical hypothesis: its facts sorted, placeholders numbered by first use.
fn canonical(facts: &[Pat]) -> Vec<Pat> {
    let mut v: Vec<Pat> = facts.to_vec();
    v.sort_unstable();
    v.dedup();
    let mut map: Vec<(u64, u64)> = Vec::new();
    for f in &mut v {
        for x in f.iter_mut() {
            if is_hole(*x) {
                let n = match map.iter().find(|(a, _)| a == x) {
                    Some((_, b)) => *b,
                    None => {
                        let b = HOLE + map.len() as u64;
                        map.push((*x, b));
                        b
                    }
                };
                *x = n;
            }
        }
    }
    v
}

fn holes(facts: &[Pat]) -> usize {
    let mut seen: Vec<u64> = facts
        .iter()
        .flatten()
        .copied()
        .filter(|&x| is_hole(x))
        .collect();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

/// Shift the placeholders of `facts` past `by`.
fn shift(facts: &[Pat], by: u64) -> Vec<Pat> {
    facts
        .iter()
        .map(|f| f.map(|x| if is_hole(x) { x + by } else { x }))
        .collect()
}

/// The facts of `g` matching a pattern (`None` free), at most `cap`, and whether there
/// were more.
fn matches(g: &Graph, pat: [Option<u64>; 3], cap: usize) -> anyhow::Result<(Vec<Triple>, bool)> {
    let mut out: Vec<Triple> = Vec::new();
    let mut more = false;
    let fits = |t: &Triple| (0..3).all(|i| pat[i].is_none_or(|x| x == t[i]));
    if let Some(b) = g.base() {
        let mut memo = Memo::default();
        b.for_each_with(&mut memo, pat[0], pat[1], pat[2], |t| {
            if out.len() >= cap {
                more = true;
                return false;
            }
            if fits(&t) {
                out.push(t);
            }
            true
        })?;
    }
    let c = g.cands(pat[0], pat[1], pat[2], 0, g.len());
    for i in 0..c.len() {
        let r = c.get(i);
        if !g.alive(r) {
            continue;
        }
        let t = g.triple(r);
        if !fits(&t) || (pat[1].is_none() && g.is_hidden(t[1])) || out.contains(&t) {
            continue;
        }
        if out.len() >= cap {
            more = true;
            break;
        }
        out.push(t);
    }
    Ok((out, more))
}

impl Unfold<'_> {
    fn check_controls(&self) -> anyhow::Result<()> {
        if self
            .opts
            .explain
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        {
            return Err(session_error(sparkles_core::Error::Cancelled));
        }
        if self
            .opts
            .explain
            .deadline
            .is_some_and(|d| std::time::Instant::now() >= d)
        {
            return Err(session_error(sparkles_core::Error::Timeout));
        }
        Ok(())
    }

    fn push_combo(&mut self, out: &mut Vec<Combo>, value: Combo) -> anyhow::Result<bool> {
        self.check_controls()?;
        if out.len() >= MAX_PARTIAL_HYPOTHESES {
            self.complete = false;
            return Ok(false);
        }
        self.work.try_grow(
            2 * (std::mem::size_of::<Combo>() + value.0.len() * std::mem::size_of::<Pat>()),
        )?;
        out.push(value);
        Ok(true)
    }

    fn terms(&self) -> &Terms {
        self.session.terms()
    }

    fn rules(&self) -> &[CRule] {
        self.session.rules()
    }

    /// Whether rules can give facts of the pattern.
    fn derivable(&self, p: [Option<u64>; 3]) -> bool {
        let slot = |x: Option<u64>, v: usize| x.map_or(Slot::Var(v), Slot::Const);
        self.geo
            .is_some_and(|(_, default)| p[1].is_none_or(|p| p == default))
            || intensional(self.rules(), (slot(p[0], 0), slot(p[1], 1), slot(p[2], 2)))
    }

    /// Additions that make the implicit single-geometry rule hold.
    fn geometry_alternative(&mut self, t: Pat) -> anyhow::Result<Option<Hyp>> {
        let Some((has, default)) = self.geo else {
            return Ok(None);
        };
        if t[1] != default {
            return Ok(None);
        }
        self.check_controls()?;
        let base = self
            .session
            .graph()
            .and_then(Graph::base_arc)
            .expect("derived base");
        if base.any(&mut Memo::default(), Some(t[0]), Some(default), None)? {
            return Ok(None);
        }
        let mut exists = false;
        let mut conflict = false;
        let mut stopped = None;
        base.for_each(Some(t[0]), Some(has), None, |g| {
            if let Err(error) = self.check_controls() {
                stopped = Some(error);
                return false;
            }
            exists = true;
            conflict |= g[2] != t[2];
            !conflict
        })?;
        if let Some(error) = stopped {
            return Err(error);
        }
        self.check_controls()?;
        if exists || conflict {
            return Ok(None);
        }
        Ok(Some((vec![[t[0], has, t[2]]], 1)))
    }

    /// Note a pattern the pass reads whose facts no derivation asked for yet.
    fn ensure(&mut self, p: [Option<u64>; 3]) {
        if !self.derivable(p) || self.demanded.contains(&p) {
            return;
        }
        // a pattern whose facts a wider one asked for is complete
        let wider = |q: &[Option<u64>; 3]| (0..3).all(|i| q[i].is_none() || q[i] == p[i]);
        if self.demanded.iter().any(wider) {
            return;
        }
        // a free predicate asks for every fact the rules give about a value, whose
        // program can pass its limit: the facts derived so far and the stored ones stand
        if p[1].is_none() {
            self.lossy = true;
            return;
        }
        if !self.needed.contains(&p) {
            self.needed.push(p);
        }
    }

    /// Derive the facts of `pats`, in one program when it fits and one at a time when
    /// not.
    fn derive(&mut self, pats: Vec<[Option<u64>; 3]>) -> anyhow::Result<()> {
        let mut demands = Vec::with_capacity(pats.len());
        for p in &pats {
            self.demanded.insert(*p);
            let t = |x: Option<u64>| -> anyhow::Result<DemandTerm> {
                Ok(match x {
                    Some(id) => DemandTerm::Const(
                        self.terms()
                            .term(id)
                            .ok_or_else(|| anyhow::anyhow!("term {id} is unknown"))?,
                    ),
                    None => DemandTerm::Var,
                })
            };
            demands.push(Demand {
                s: t(p[0])?,
                p: t(p[1])?,
                o: t(p[2])?,
            });
        }
        let too_large = |e: &sparkles_core::Error| {
            matches!(e, sparkles_core::Error::Component(c)
                if c.code == crate::query::BUDGET_CODE && c.message.contains("rules"))
        };
        match self.session.derive_all(&demands) {
            Ok(()) => Ok(()),
            Err(e) if too_large(&e) && demands.len() > 1 => {
                for d in demands {
                    match self.session.derive_all(&[d]) {
                        Ok(()) => {}
                        Err(e) if too_large(&e) => self.lossy = true,
                        Err(e) => return Err(session_error(e)),
                    }
                }
                Ok(())
            }
            // a program past its size stands for facts the search reads without it
            Err(e) if too_large(&e) => {
                self.lossy = true;
                Ok(())
            }
            Err(e) => Err(session_error(e)),
        }
    }

    /// One pass of the unfolding from the triple: its hypotheses and candidates.
    #[allow(clippy::type_complexity)]
    fn pass(
        &mut self,
        root: Pat,
    ) -> anyhow::Result<(
        Vec<Hyp>,
        Vec<(usize, Vec<u64>, Vec<(String, Option<Triple>)>)>,
    )> {
        self.work.set(0);
        self.steps = 0;
        self.complete = true;
        self.needed.clear();
        self.path = vec![root];
        let mut cands = Vec::new();
        let hyps = self.unfold(root, 0, Some(&mut cands))?;
        Ok((hyps, cands))
    }

    /// Whether the rule makes terms (blank nodes in its head or a builtin that makes
    /// one), whose hypotheses are not verified (spec F18 §4.2).
    fn makes_terms(r: &CRule) -> bool {
        r.builtins
            .iter()
            .any(|b| matches!(b.kind, BuiltinKind::MakeTemp | BuiltinKind::MakeSkolem))
            || r.head.iter().any(|h| matches!(h, CHead::Action(..)))
    }

    /// The bindings of rule `ri`'s head `hi` unified with `t`, or `None`.
    fn unify(r: &CRule, hi: usize, t: &Pat) -> Option<Vec<u64>> {
        let CHead::Triple(s, p, o) = &r.head[hi] else {
            return None;
        };
        let mut b = vec![0u64; r.nvars];
        for (slot, v) in [(*s, t[0]), (*p, t[1]), (*o, t[2])] {
            match slot {
                Slot::Const(c) if c != v => return None,
                Slot::Var(x) => {
                    if b[x] != 0 && b[x] != v {
                        return None;
                    }
                    b[x] = v;
                }
                _ => {}
            }
        }
        Some(b)
    }

    /// The completions of rule `ri` from the head bindings `b`, and the builtins that
    /// blocked complete instances.
    fn completions(
        &mut self,
        ri: usize,
        b: Vec<u64>,
        blocked: &mut Vec<(String, Option<Triple>)>,
    ) -> anyhow::Result<Vec<Completion>> {
        let mut out = Vec::new();
        let mut b = b;
        let mut missing = Vec::new();
        self.walk(ri, 0, &mut b, &mut missing, &mut out, blocked)?;
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        ri: usize,
        k: usize,
        b: &mut Vec<u64>,
        missing: &mut Vec<usize>,
        out: &mut Vec<Completion>,
        blocked: &mut Vec<(String, Option<Triple>)>,
    ) -> anyhow::Result<()> {
        self.check_controls()?;
        self.steps += 1;
        if self.steps > self.cap {
            self.complete = false;
            return Ok(());
        }
        let n = self.session.rules()[ri].atoms.len();
        if k == n {
            return self.finish(ri, b, missing, out, blocked);
        }
        let a = self.session.rules()[ri].atoms[k].clone();
        let val = |s: Slot, b: &[u64]| match s {
            Slot::Const(c) => Some(c),
            Slot::Var(v) if b[v] != 0 => Some(b[v]),
            _ => None,
        };
        let pat = [val(a.s, b), val(a.p, b), val(a.o, b)];
        self.ensure(pat);
        let (found, more) = matches(self.session.graph().expect("derived"), pat, MAX_MATCHES)?;
        if more {
            self.complete = false;
        }
        for t in &found {
            let mut newly = Vec::new();
            let mut ok = true;
            for (slot, v) in [(a.s, t[0]), (a.p, t[1]), (a.o, t[2])] {
                if let Slot::Var(x) = slot {
                    if b[x] == 0 {
                        b[x] = v;
                        newly.push(x);
                    } else if b[x] != v {
                        ok = false;
                    }
                }
            }
            if ok {
                self.walk(ri, k + 1, b, missing, out, blocked)?;
            }
            for x in newly {
                b[x] = 0;
            }
        }
        // a ground atom that holds is never missing
        let ground = pat.iter().all(Option::is_some);
        let promote_default = ground
            && !found.is_empty()
            && self.geo.is_some_and(|(_, default)| pat[1] == Some(default))
            && !self
                .session
                .graph()
                .and_then(Graph::base_arc)
                .expect("derived base")
                .any(&mut Memo::default(), pat[0], pat[1], pat[2])?;
        if !(ground && !found.is_empty()) || promote_default {
            missing.push(k);
            self.walk(ri, k + 1, b, missing, out, blocked)?;
            missing.pop();
        }
        Ok(())
    }

    /// A complete walk of rule `ri`: its builtins tested where their arguments are bound,
    /// and its missing atoms as patterns.
    fn finish(
        &mut self,
        ri: usize,
        b: &[u64],
        missing: &[usize],
        out: &mut Vec<Completion>,
        blocked: &mut Vec<(String, Option<Triple>)>,
    ) -> anyhow::Result<()> {
        let r = &self.session.rules()[ri];
        let terms = self.session.terms();
        let g = self.session.graph().expect("derived");
        let mut b = b.to_vec();
        let val = |s: Slot, b: &[u64]| match s {
            Slot::Const(c) => Some(c),
            Slot::Var(v) if b[v] != 0 => Some(b[v]),
            _ => None,
        };
        let infos = self.session.infos();
        for (j, bi) in r.builtins.iter().enumerate() {
            let args: Vec<Option<u64>> = bi.args.iter().map(|&s| val(s, &b)).collect();
            let name = infos
                .get(ri)
                .and_then(|i| i.builtins.get(j))
                .map_or("builtin", String::as_str);
            let fail = match bi.kind {
                BuiltinKind::NoValue => {
                    // a variable no atom bound is a placeholder, which the test cannot read
                    let unbound = bi
                        .args
                        .iter()
                        .any(|s| matches!(s, Slot::Var(v) if b[*v] == 0));
                    if unbound {
                        self.complete = false;
                        continue;
                    }
                    let (found, _) = matches(
                        g,
                        [
                            args.first().copied().flatten(),
                            args.get(1).copied().flatten(),
                            args.get(2).copied().flatten(),
                        ],
                        1,
                    )?;
                    found.first().map(|t| Some(*t))
                }
                _ => {
                    // Use the evaluator itself for the builtin's value semantics and
                    // output bindings. Missing inputs remain placeholders, so only an
                    // instance whose inputs are known can be diagnosed as blocked.
                    if bi
                        .kind
                        .input_positions(bi.args.len())
                        .iter()
                        .any(|&i| args[i].is_none())
                    {
                        // Fresh blank nodes do not cover every possible literal or
                        // datatype value a builtin can require of a missing term.
                        self.complete = false;
                        continue;
                    }
                    let abort = std::sync::atomic::AtomicBool::new(false);
                    let mut ev = Eval::new(
                        g,
                        terms,
                        r,
                        &[],
                        g.len(),
                        &abort,
                        self.opts.explain.cancel.as_deref(),
                        1,
                        Mode::New,
                        self.session.budget(),
                    );
                    ev.set_deadline(self.opts.explain.deadline);
                    ev.collect_at = Some((1, 1));
                    crate::builtins::eval(&mut ev, j, 0, &mut b);
                    if let Some(e) = ev.err.take() {
                        return Err(e);
                    }
                    match ev.collected.pop() {
                        Some(values) => {
                            b = values;
                            None
                        }
                        None => Some(None),
                    }
                }
            };
            if let Some(fact) = fail {
                let call = blocker_call(terms, name, &args, &mut self.work)?;
                blocked.push((call, fact));
                return Ok(());
            }
        }
        if missing.is_empty() {
            // the instance holds, unless a builtin the search cannot test fails
            return Ok(());
        }
        let mut hole_of: Vec<(usize, u64)> = Vec::new();
        let mut pats = Vec::new();
        for &k in missing {
            let a = &r.atoms[k];
            let mut p = [0u64; 3];
            for (i, s) in [a.s, a.p, a.o].into_iter().enumerate() {
                p[i] = match s {
                    Slot::Const(c) => c,
                    Slot::Var(v) if b[v] != 0 => b[v],
                    Slot::Var(v) => match hole_of.iter().find(|(x, _)| *x == v) {
                        Some((_, h)) => *h,
                        None => {
                            let h = HOLE + hole_of.len() as u64;
                            hole_of.push((v, h));
                            h
                        }
                    },
                    Slot::Any => {
                        let h = HOLE + hole_of.len() as u64;
                        hole_of.push((usize::MAX - hole_of.len(), h));
                        h
                    }
                };
            }
            pats.push(p);
        }
        pats.sort_unstable();
        pats.dedup();
        // Intermediate atoms can share support after unfolding, so their count does
        // not bound the size of the final deduplicated hypothesis.
        self.work.try_grow(
            2 * (std::mem::size_of::<Completion>() + pats.len() * std::mem::size_of::<Pat>()),
        )?;
        out.push(Completion { missing: pats });
        Ok(())
    }

    /// The hypotheses for the ground pattern `t` at `depth`: `t` itself, and the
    /// hypotheses of the rules that derive it unfolded one level further.
    fn alternatives(&mut self, t: Pat, depth: u32) -> anyhow::Result<Vec<Hyp>> {
        // a fact being unfolded above is no hypothesis for itself
        if self.path.contains(&t) {
            return Ok(Vec::new());
        }
        let mut alts = vec![(vec![t], 0)];
        let pat = t.map(Some);
        if !self.derivable(pat) {
            return Ok(alts);
        }
        if depth >= self.opts.max_unfold {
            self.complete = false;
            return Ok(alts);
        }
        self.path.push(t);
        let more = self.unfold(t, depth, None);
        self.path.pop();
        alts.extend(more?);
        Ok(alts)
    }

    /// The hypotheses that make the ground fact `t` derivable at `depth`, with the
    /// candidates of the root when `candidates` is given.
    fn unfold(
        &mut self,
        t: Pat,
        depth: u32,
        mut candidates: Option<&mut Vec<CandidateInstance>>,
    ) -> anyhow::Result<Vec<Hyp>> {
        let mut out: Vec<Hyp> = Vec::new();
        if self.geo.is_some_and(|(_, default)| t[1] == default) {
            if let Some(candidates) = candidates.as_deref_mut() {
                candidates.push((GEO_CANDIDATE, vec![t[0], t[2]], Vec::new()));
            }
            if let Some(hypothesis) = self.geometry_alternative(t)? {
                out.push(hypothesis);
            }
        }
        let n = self.rules().len();
        for ri in 0..n {
            let heads = self.rules()[ri].head.len();
            for hi in 0..heads {
                let r = &self.rules()[ri];
                if !matches!(r.head[hi], CHead::Triple(..)) {
                    // a head action gives facts the search does not unfold
                    let hs = head_slots(&r.head[hi]);
                    let one = |x: Slot, v: u64| !matches!(x, Slot::Const(c) if c != v);
                    if one(hs.0, t[0]) && one(hs.1, t[1]) && one(hs.2, t[2]) {
                        self.complete = false;
                    }
                    continue;
                }
                let Some(b) = Self::unify(r, hi, &t) else {
                    continue;
                };
                if Self::makes_terms(r) {
                    if let Some(c) = candidates.as_deref_mut() {
                        c.push((ri, b, Vec::new()));
                    }
                    self.complete = false;
                    continue;
                }
                self.check_controls()?;
                if out.len() >= MAX_PARTIAL_HYPOTHESES {
                    self.complete = false;
                    if let Some(c) = candidates.as_deref_mut() {
                        c.push((ri, b, Vec::new()));
                    }
                    continue;
                }
                if candidates.is_some() {
                    self.cap = self.steps + STEPS_PER_RULE;
                }
                let mut blocked = Vec::new();
                let comps = self.completions(ri, b.clone(), &mut blocked)?;
                if let Some(c) = candidates.as_deref_mut() {
                    c.push((ri, b, blocked));
                }
                for comp in comps {
                    // the completion's placeholders are shared by its missing atoms, and
                    // those of a missing atom's own hypotheses come after them
                    let mut combos: Vec<Combo> = vec![(Vec::new(), holes(&comp.missing) as u64, 1)];
                    for m in comp.missing {
                        let alts = if m.iter().any(|&x| is_hole(x)) {
                            if self.derivable(m.map(|x| (!is_hole(x)).then_some(x))) {
                                // a missing intensional atom with a placeholder is kept
                                // as a fact, not unfolded
                                self.complete = false;
                            }
                            None
                        } else {
                            Some(self.alternatives(m, depth + 1)?)
                        };
                        let mut next = Vec::new();
                        'combine: for (c, free, steps) in &combos {
                            match &alts {
                                None => {
                                    let mut u = c.clone();
                                    u.push(m);
                                    if u.len() <= self.opts.max_missing
                                        && !self.push_combo(&mut next, (u, *free, *steps))?
                                    {
                                        break 'combine;
                                    }
                                }
                                Some(alts) => {
                                    for (a, n) in alts {
                                        let mut u = c.clone();
                                        u.extend(shift(a, *free));
                                        u.sort_unstable();
                                        u.dedup();
                                        if u.len() <= self.opts.max_missing
                                            && !self.push_combo(
                                                &mut next,
                                                (u, free + holes(a) as u64, steps + n),
                                            )?
                                        {
                                            break 'combine;
                                        }
                                    }
                                }
                            }
                        }
                        combos = next;
                        if combos.is_empty() {
                            break;
                        }
                    }
                    let mut combos: Vec<Hyp> = combos
                        .into_iter()
                        .map(|(c, _, n)| (canonical(&c), n))
                        .collect();
                    let room = MAX_PARTIAL_HYPOTHESES.saturating_sub(out.len());
                    if combos.len() > room {
                        self.complete = false;
                        combos.truncate(room);
                    }
                    out.extend(combos);
                }
            }
        }
        Ok(out)
    }
}

/// Whether a term id names a term of the RDF, RDFS or OWL vocabulary.
fn builtin_term(terms: &Terms, x: u64) -> bool {
    const NS: [&str; 3] = [
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
        "http://www.w3.org/2000/01/rdf-schema#",
        "http://www.w3.org/2002/07/owl#",
    ];
    !is_hole(x)
        && matches!(terms.term(x), Some(Term::NamedNode(n)) if NS.iter().any(|ns| n.as_str().starts_with(ns)))
}

/// The facts of a hypothesis that state something about the built-in vocabulary, or
/// equality: a built-in term as subject, a built-in object of a predicate other than
/// `rdf:type`, or `owl:sameAs`.
fn meta(terms: &Terms, rdf_type: u64, same_as: u64, facts: &[Pat]) -> usize {
    facts
        .iter()
        .filter(|f| {
            f[1] == same_as
                || builtin_term(terms, f[0])
                || (f[1] != rdf_type && builtin_term(terms, f[2]))
        })
        .count()
}

/// Why the rules do not derive `triple` over `input`'s view, and what would make them.
pub fn why_not(
    input: &DemandInput<'_>,
    triple: &[Term; 3],
    opts: &WhyNotOptions,
) -> anyhow::Result<WhyNot> {
    let mut opts = opts.clone();
    opts.max_missing = opts.max_missing.clamp(1, MAX_MISSING_LIMIT);
    opts.max_hypotheses = opts.max_hypotheses.clamp(1, 100);
    opts.max_unfold = opts.max_unfold.min(crate::explain::MAX_DEPTH_LIMIT);
    crate::explain::budget_errors(why_not_inner(input, triple, &opts))
}

fn why_not_inner(
    input: &DemandInput<'_>,
    triple: &[Term; 3],
    opts: &WhyNotOptions,
) -> anyhow::Result<WhyNot> {
    let mut session = demand_session(input, &opts.explain, true)?;
    session
        .derive_all(&[demand_of(triple)])
        .map_err(session_error)?;
    let terms = session.terms();
    let root = [
        terms.try_id_for(&triple[0])?,
        terms.try_id_for(&triple[1])?,
        terms.try_id_for(&triple[2])?,
    ];
    if session.graph().is_some_and(|g| g.contains(&root)) {
        return Ok(WhyNot::Entailed);
    }
    let rdf_type = terms.id_for(&Term::NamedNode(oxrdf::vocab::rdf::TYPE.into_owned()));
    let same_as = terms.id_for(&Term::NamedNode(oxrdf::NamedNode::new_unchecked(
        "http://www.w3.org/2002/07/owl#sameAs",
    )));
    let geo = session
        .geo_default()
        .then(|| crate::geo_ids(session.terms()));
    let mut u = Unfold {
        geo,
        work: crate::budget::Held::new(session.budget()),
        session,
        opts,
        rdf_type,
        demanded: FxHashSet::default(),
        needed: Vec::new(),
        path: Vec::new(),
        steps: 0,
        cap: 0,
        complete: true,
        lossy: false,
    };
    u.demanded.insert(root.map(Some));
    let t0 = std::time::Instant::now();
    // each pass asks the demand engine, at once, for the patterns the pass before read
    let (mut hyps, mut cands) = u.pass(root)?;
    tracing::debug!(target: "sparkles_reasoner::whynot", "first pass {:?}, {} patterns to derive", t0.elapsed(), u.needed.len());
    for _ in 0..MAX_ROUNDS {
        let need = std::mem::take(&mut u.needed);
        if need.is_empty() {
            break;
        }
        u.derive(need)?;
        (hyps, cands) = u.pass(root)?;
        tracing::debug!(target: "sparkles_reasoner::whynot", "pass {:?}, {} patterns to derive", t0.elapsed(), u.needed.len());
    }
    if !u.needed.is_empty() || u.lossy {
        u.complete = false;
    }
    hyps.retain(|(h, _)| !h.contains(&root));
    // the candidates, with their unifiers and blocks
    let terms = u.session.terms();
    let mut rules = Vec::new();
    for (ri, b, blocked) in &cands {
        u.check_controls()?;
        if *ri == GEO_CANDIDATE {
            let unifier: Vec<_> = ["feature", "geometry"]
                .into_iter()
                .zip(b)
                .filter_map(|(name, id)| terms.term(*id).map(|term| (name.to_string(), term)))
                .collect();
            u.work.try_grow(
                std::mem::size_of::<Candidate>()
                    + unifier
                        .iter()
                        .map(|(name, term)| name.len() + sparkles_core::explain::term_bytes(term))
                        .sum::<usize>(),
            )?;
            rules.push(Candidate {
                rule: RuleRef {
                    name: "geo-default-geometry".into(),
                    profile: "geosparql".into(),
                    owl2rl: None,
                },
                line: 0,
                unifier,
                blocked_by: Vec::new(),
            });
            continue;
        }
        let r = &u.session.rules()[*ri];
        let info = u.session.infos().get(*ri);
        let mut unifier = Vec::new();
        for (v, &x) in b.iter().enumerate() {
            if x != 0
                && let Some(t) = terms.term(x)
            {
                let name = info
                    .and_then(|i| i.vars.get(v))
                    .cloned()
                    .unwrap_or_else(|| format!("v{v}"));
                u.work.try_grow(
                    2 * std::mem::size_of::<(String, Term)>()
                        + name.len()
                        + sparkles_core::explain::term_bytes(&t),
                )?;
                unifier.push((name, t));
            }
        }
        let profile = info
            .and_then(|i| u.session.sources().get(i.parsed))
            .cloned()
            .unwrap_or_default();
        let mut blocked_by: Vec<Blocked> = Vec::new();
        for (call, fact) in blocked {
            u.check_controls()?;
            let fact =
                fact.and_then(|f| Some([terms.term(f[0])?, terms.term(f[1])?, terms.term(f[2])?]));
            let bytes = fact.as_ref().map_or(0, |t| {
                t.iter()
                    .map(sparkles_core::explain::term_bytes)
                    .sum::<usize>()
            });
            u.work
                .try_grow(call.len() + bytes + 2 * std::mem::size_of::<Blocked>())?;
            let b = Blocked {
                call: call.clone(),
                fact,
            };
            if !blocked_by.contains(&b) {
                blocked_by.push(b);
            }
        }
        u.work
            .try_grow(2 * std::mem::size_of::<Candidate>() + 2 * r.name.len() + profile.len())?;
        rules.push(Candidate {
            rule: RuleRef {
                owl2rl: crate::explain::owl2rl_id(&profile, &r.name),
                name: r.name.clone(),
                profile,
            },
            line: info.map_or(0, |i| i.line),
            unifier,
            blocked_by,
        });
    }
    // minimal hypotheses, best first: each set once, with its fewest rule instances
    hyps.sort();
    hyps.dedup_by(|a, b| a.0 == b.0);
    hyps.retain(|(h, _)| !h.is_empty());
    let mut order: Vec<(usize, usize, u32, usize, usize)> = hyps
        .iter()
        .enumerate()
        .map(|(i, (h, n))| {
            (
                h.len(),
                meta(terms, u.rdf_type, same_as, h),
                *n,
                holes(h),
                i,
            )
        })
        .collect();
    order.sort_unstable();
    let ordered: Vec<Vec<Pat>> = order
        .into_iter()
        .map(|(_, _, _, _, i)| hyps[i].0.clone())
        .collect();
    // Only verified supports can make a larger candidate redundant. A smaller
    // candidate can invalidate its own noValue condition once its facts are added.
    let mut verified: Vec<Vec<Pat>> = Vec::new();
    let mut hypotheses = Vec::new();
    let mut tried = 0usize;
    for h in ordered {
        if verified.iter().any(|k| {
            k.len() < h.len()
                && k.iter()
                    .all(|f| !f.iter().any(|&x| is_hole(x)) && h.contains(f))
        }) {
            continue;
        }
        if hypotheses.len() >= opts.max_hypotheses.max(1) || tried >= MAX_VERIFIED {
            u.complete = false;
            break;
        }
        tried += 1;
        let tv = std::time::Instant::now();
        let mut verified_completely = true;
        let v = verify(input, triple, &u, &h, opts, &mut verified_completely)?;
        u.complete &= verified_completely;
        tracing::debug!(target: "sparkles_reasoner::whynot", "verified {} in {:?}", v.is_some(), tv.elapsed());
        if let Some(v) = v {
            verified.push(h);
            u.work.try_grow(
                v.tree.heap_bytes()
                    + v.missing
                        .iter()
                        .flatten()
                        .map(sparkles_core::explain::term_bytes)
                        .sum::<usize>(),
            )?;
            hypotheses.push(v);
        }
    }
    Ok(WhyNot::NotEntailed {
        rules,
        hypotheses,
        complete: u.complete,
    })
}

/// Keep synthetic defaults consistent with additions to either geometry predicate.
fn add_hypothesis(
    session: &mut Session,
    facts: &[Triple],
    opts: &ExplainOptions,
) -> anyhow::Result<()> {
    if !session.geo_default() {
        return session.add_facts(facts.to_vec()).map_err(session_error);
    }
    let (has, default) = crate::geo_ids(session.terms());
    let base = session
        .graph()
        .and_then(Graph::base_arc)
        .ok_or_else(|| anyhow::anyhow!("the session has no input base"))?;
    let mut features: Vec<u64> = facts
        .iter()
        .filter(|t| t[1] == has || t[1] == default)
        .map(|t| t[0])
        .collect();
    features.sort_unstable();
    features.dedup();
    let stop = crate::explain::session_stop(opts);
    let mut remove = Vec::new();
    let mut generated = Vec::new();
    for feature in features {
        stop().map_err(session_error)?;
        let mut old = FxHashSet::default();
        let mut stopped = None;
        base.for_each(Some(feature), Some(has), None, |t| {
            if let Err(e) = stop() {
                stopped = Some(e);
                return false;
            }
            old.insert(t[2]);
            old.len() < 2
        })?;
        if let Some(e) = stopped {
            return Err(session_error(e));
        }
        let explicit = base.any(&mut Memo::default(), Some(feature), Some(default), None)?;
        if !explicit && old.len() == 1 {
            remove.push([feature, default, *old.iter().next().expect("one geometry")]);
        }
        let mut all = old;
        for t in facts.iter().filter(|t| t[0] == feature && t[1] == has) {
            all.insert(t[2]);
        }
        let explicit = explicit || facts.iter().any(|t| t[0] == feature && t[1] == default);
        if !explicit && all.len() == 1 {
            generated.push([feature, default, *all.iter().next().expect("one geometry")]);
        }
    }
    let graph = session.graph_mut().map_err(session_error)?;
    for t in remove {
        graph.kill(&t);
    }
    session.add_facts(facts.to_vec()).map_err(session_error)?;
    session.add_facts(generated).map_err(session_error)
}

/// Verify hypothesis `h` (in the ids of `u`'s session): its tree, when the demand program
/// over the view with its facts derives the triple.
fn verify(
    input: &DemandInput<'_>,
    triple: &[Term; 3],
    u: &Unfold<'_>,
    h: &[Pat],
    opts: &WhyNotOptions,
    complete: &mut bool,
) -> anyhow::Result<Option<Hypothesis>> {
    let from = u.session.terms();
    // Hypotheses are additions to an RDF snapshot. Generalized intermediate facts
    // may occur in a proof, but a caller cannot insert a literal subject or a blank
    // predicate. A placeholder becomes a blank node and therefore cannot be a predicate.
    if h.iter().any(|f| {
        is_hole(f[1])
            || !matches!(from.term(f[1]), Some(Term::NamedNode(_)))
            || (!is_hole(f[0]) && matches!(from.term(f[0]), Some(Term::Literal(_))))
    }) {
        return Ok(None);
    }
    // Even an ordinary application predicate can derive a schema fact through a
    // custom rule. An overlay therefore never reuses the snapshot's schema table.
    let mut verification_opts = opts.explain.clone();
    verification_opts.max_memory = opts
        .explain
        .max_memory
        .map(|m| m.saturating_sub(u.session.budget().used()));
    let mut s = demand_session(input, &verification_opts, false)?;
    // placeholders as fresh blank nodes of the new session
    let n = holes(h);
    let mut fresh = Vec::with_capacity(n);
    for _ in 0..n {
        fresh.push(s.terms().try_fresh_bnode()?);
    }
    let labels: Vec<String> = (1..=n).map(|i| format!("y{i}")).collect();
    let mut facts: Vec<Triple> = Vec::new();
    let mut missing: Vec<[Term; 3]> = Vec::new();
    for f in h {
        let mut t = [0u64; 3];
        let mut shown: Vec<Term> = Vec::new();
        for i in 0..3 {
            if is_hole(f[i]) {
                let k = (f[i] - HOLE) as usize;
                t[i] = fresh[k];
                shown.push(Term::BlankNode(BlankNode::new_unchecked(&labels[k])));
            } else {
                let term = from
                    .term(f[i])
                    .ok_or_else(|| anyhow::anyhow!("term {} is unknown", f[i]))?;
                t[i] = s.terms().try_id_for(&term)?;
                shown.push(term);
            }
        }
        facts.push(t);
        let [a, b, c] = <[Term; 3]>::try_from(shown).expect("three terms");
        missing.push([a, b, c]);
    }
    add_hypothesis(&mut s, &facts, &verification_opts)?;
    match s.derive_all(&[demand_of(triple)]) {
        Ok(()) => {}
        // a hypothesis whose program passes its size, such as one that adds equality
        // to rules with variable predicates, is not verified
        Err(sparkles_core::Error::Component(c))
            if c.code == crate::query::BUDGET_CODE && c.message.contains("rules") =>
        {
            *complete = false;
            return Ok(None);
        }
        Err(e) => return Err(session_error(e)),
    }
    let terms = s.terms();
    let root = [
        terms.try_id_for(&triple[0])?,
        terms.try_id_for(&triple[1])?,
        terms.try_id_for(&triple[2])?,
    ];
    if !s.graph().is_some_and(|g| g.contains(&root)) {
        return Ok(None);
    }
    // the fresh blank nodes are shown by the placeholders' labels
    let names: Vec<(Term, Term)> = fresh
        .iter()
        .zip(&labels)
        .filter_map(|(&id, l)| {
            Some((
                s.terms().term(id)?,
                Term::BlankNode(BlankNode::new_unchecked(l)),
            ))
        })
        .collect();
    let extra: FxHashSet<Triple> = facts.into_iter().collect();
    let mut tree_opts = verification_opts;
    tree_opts.max_trees = 1;
    let tree = match search_session(&mut s, &extra, root, &tree_opts)? {
        Explained::Found { mut trees, .. } if !trees.is_empty() => trees.swap_remove(0),
        Explained::Partial { tree, .. } => *tree,
        _ => return Ok(None),
    };
    let mut tree = tree;
    rename(&mut tree, &names);
    Ok(Some(Hypothesis {
        missing,
        placeholders: labels,
        tree,
    }))
}

/// Replace the terms of `names` in the tree.
fn rename(d: &mut Derivation, names: &[(Term, Term)]) {
    for t in d.triple.iter_mut() {
        if let Some((_, to)) = names.iter().find(|(from, _)| from == t) {
            *t = to.clone();
        }
    }
    for (_, t) in d.bindings.iter_mut() {
        if let Some((_, to)) = names.iter().find(|(from, _)| from == t) {
            *t = to.clone();
        }
    }
    for p in &mut d.premises {
        rename(p, names);
    }
}

#[cfg(test)]
mod geometry_overlay_tests {
    use super::*;
    use crate::{Extras, Inputs, Profile};
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::store::Store;

    #[test]
    fn overlays_preserve_asserted_defaults_and_recompute_generated_defaults() {
        const PREFIX: &str = "@prefix geo: <http://www.opengis.net/ont/geosparql#> . ";
        let iri = |s: &str| Term::NamedNode(oxrdf::NamedNode::new_unchecked(s));
        let feature = iri("urn:feature");
        let first = iri("urn:first");
        let second = iri("urn:second");
        let has = iri("http://www.opengis.net/ont/geosparql#hasGeometry");
        let default = iri("http://www.opengis.net/ont/geosparql#hasDefaultGeometry");
        let one = "<urn:feature> geo:hasGeometry <urn:first> .";
        let explicit =
            "<urn:feature> geo:hasGeometry <urn:first> ; geo:hasDefaultGeometry <urn:first> .";
        let cases = [
            (
                "",
                vec![[feature.clone(), has.clone(), first.clone()]],
                true,
                false,
            ),
            (
                one,
                vec![[feature.clone(), has.clone(), first.clone()]],
                true,
                false,
            ),
            (
                one,
                vec![[feature.clone(), has.clone(), second.clone()]],
                false,
                false,
            ),
            (
                one,
                vec![[feature.clone(), default.clone(), second.clone()]],
                false,
                true,
            ),
            (
                explicit,
                vec![[feature.clone(), has.clone(), second.clone()]],
                true,
                false,
            ),
        ];
        let extras = Extras {
            geo_default_geometry: true,
            ..Default::default()
        };
        let reasoner = crate::query::QueryReasoner::new(
            Profile::Rules(String::new()),
            &extras,
            Inputs::default(),
        )
        .unwrap();
        for (ttl, additions, expected_first, expected_second) in cases {
            let store = Store::in_memory(Default::default());
            store
                .load(&[Source::from_bytes(
                    format!("{PREFIX}{ttl}").into_bytes(),
                    RdfFormat::Turtle,
                    None,
                )])
                .unwrap();
            let input = DemandInput {
                reasoner: &reasoner,
                snap: store.snapshot(),
                readable: None,
            };
            let options = ExplainOptions::default();
            let mut session = demand_session(&input, &options, false).unwrap();
            let facts: Vec<_> = additions
                .iter()
                .map(|t| t.each_ref().map(|t| session.terms().id_for(t)))
                .collect();
            add_hypothesis(&mut session, &facts, &options).unwrap();
            for (geometry, expected) in [(&first, expected_first), (&second, expected_second)] {
                let triple = [&feature, &default, geometry].map(|t| session.terms().id_for(t));
                assert_eq!(
                    session.graph().unwrap().contains(&triple),
                    expected,
                    "{ttl}, {additions:?}, {geometry}"
                );
            }
        }
    }
}
