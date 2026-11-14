//! Rule-body planning from sampled bindings (spec F23, roadmap item 939).
//!
//! The greedy planner of [`plan`](super::plan) places, at each step, the atom with the
//! fewest estimated matches, and estimates each atom from averages: the rows of its
//! predicate over the predicate's distinct subjects or objects, and a fixed share of an
//! average predicate's rows for an atom whose predicate is a variable. The averages are
//! wrong where the rule itself says which values the atom is asked for. `prp-key` binds
//! the class and the key property from the ontology, and the greedy order then joins the
//! class's members with themselves before it reads a key value, since the average class
//! is small and the average predicate is not.
//!
//! This planner follows the bindings instead. It keeps a sample of up to [`SAMPLE`]
//! bindings of the steps placed so far, taken from the facts the plan will read, and
//! takes as an atom's estimate the mean number of candidates the atom has under those
//! bindings, counted in the derived tier's orders and the store's index. It places the
//! atom with the fewest, extends each sampled binding by facts of that atom, and goes on.
//! Builtins are placed as the greedy planner places them. A variable that a builtin
//! binds has no sampled value, and an atom that reads one keeps the greedy planner's
//! estimate, as does every atom once the sample has no binding left.
//!
//! A sample costs probes of the indexes, so the planner draws one only where it can pay.
//! A body with a variable predicate is always sampled, since the greedy estimate of such
//! an atom is a guess. A body whose predicates are all constants has estimates from the
//! statistics of its relations, and is sampled only when those put its plan above
//! [`LARGE_COST`] candidates. The planner then walks the greedy order with a sample of
//! four bindings first, and keeps it when they put its cost below [`MIN_COST`]
//! candidates, so that a plan that is cheap anyway pays for one short walk and no
//! search. Otherwise it searches as above, and its order replaces the
//! greedy order only when the sample puts its cost at no more than the greedy order's
//! divided by [`SAVING`], and when it needs no order over the whole tier by subject or
//! object that the greedy order does not. Bodies of fewer
//! than [`MIN_ATOMS`] atoms keep the greedy order: the choice among two atoms is which
//! one reads the smaller relation, which the greedy planner's counts already decide.
//! The sample is drawn at fixed places, so that a run plans the same way each time. In
//! the derived tier those are evenly spaced candidates in the order of their ids, which
//! is the same however the facts fall into runs, so the sampled orders follow the facts
//! and not the runs, as the greedy planner's statistics do. That holds while no fact is
//! dead. A dead fact stays among an order's candidates until a compaction of its run
//! drops it, which depends on the runs, so a graph that holds dead facts, as an
//! incremental run does after a deletion, keeps the greedy order of every body. In the
//! store the places are the starts of blocks along a scan, which are fixed for a
//! snapshot. The same quads in other blocks, after another sequence of loads or a
//! compaction, can give another sample and another order.
//!
//! The order of a body's atoms changes the work of an evaluation and the order in which
//! it derives its facts, and never the facts. One kind of rule is the exception, and it
//! keeps the greedy order: a builtin checks a variable that is bound already by value,
//! so that the integer 3 passes for the decimal 3.0, where an atom matches the term
//! itself, and a variable that a builtin binds and an atom or another builtin also
//! holds would be generated in one order and checked in another ([`order_matters`]).
//!
//! A plan's probes are bounded. A probe of the store reads one block of its scan at
//! most, and a sample of a relation one block per start. A plan makes at most
//! [`PROBES`] probes, and the facts they keep are charged to the run's memory budget,
//! [`PROBE_BYTES`] each, until the plan is made. A plan that needs more probes, or
//! more memory than the budget has left, or whose costs overflow, keeps the greedy
//! order. The planner's scans of the store count in the run's scans.

use super::*;
use std::sync::atomic::AtomicU64;

/// The bindings the sample keeps.
pub(crate) const SAMPLE: usize = 16;
/// The bindings of the first walk of the greedy order, which only decides whether the
/// plan is large enough for a full sample and a search.
const FIRST_SAMPLE: usize = 4;
/// The candidates of the derived tier a probe reads for its facts.
const VISIT: usize = 64;
/// The places of a scan of the store that a binding's facts are sampled from. The facts
/// of the derived tier are sampled at evenly spaced candidates, which costs a read of
/// memory each. A fact of the store costs the decoding of its part of a block, and so
/// does each later probe with the values it binds, so the sample takes runs of
/// neighbouring facts from a few places: neighbours share their blocks, in this scan and
/// often in the scans their values lead to.
const STORE_STARTS: usize = 4;
/// The sampled order replaces the greedy order only when its sampled cost is at most
/// the greedy order's divided by this.
pub(crate) const SAVING: f64 = 2.0;
/// Bodies with fewer atoms keep the greedy order.
pub(crate) const MIN_ATOMS: usize = 3;
/// A body whose predicates are all constants is sampled only when the greedy planner's
/// own estimates put its plan above this many candidates. A probe of the store decodes a
/// part of a block, some tens of microseconds, and a walk of a body of six atoms makes
/// about a hundred, so the walk costs under a hundredth of a plan of this size.
pub(crate) const LARGE_COST: f64 = 2_000_000.0;
/// A greedy order whose sampled cost is below this many candidates is kept without a
/// search for another. The search counts each remaining atom under each sampled binding
/// at each step, some hundreds of index probes for a body of six atoms, which is the
/// work of an evaluation of this size.
pub(crate) const MIN_COST: f64 = 50_000.0;

/// The probes one plan may make. The first walk, the walk of the greedy order and the
/// search of a body of `n` atoms make at most `(4 + 16) * n + 16 * n * (n + 1) / 2`,
/// which is under this for bodies of up to 14 atoms. A longer body that needs more
/// keeps the greedy order.
pub(crate) const PROBES: usize = 2048;
/// The bytes a probe is charged to the run's memory budget, an upper bound of what it
/// keeps: its entry in the plan's table and at most [`SAMPLE`] facts of 24 bytes in a
/// buffer of up to twice that.
pub(crate) const PROBE_BYTES: usize = 1024;

/// Whether every body of [`MIN_ATOMS`] atoms or more is sampled and searched, whatever
/// its plan costs, which a test sets.
static SEARCH_ALL: AtomicBool = AtomicBool::new(false);

/// Sample and search every body of [`MIN_ATOMS`] atoms or more in the runs of this
/// process, whatever its plan costs. Tests on small inputs set it, since their plans
/// are all below [`LARGE_COST`] and [`MIN_COST`].
pub(crate) fn search_all(on: bool) {
    SEARCH_ALL.store(on, Ordering::Relaxed);
}

/// The plans of this process that took the sampled order, for tests that must know the
/// sampled orders ran.
static CHANGED: AtomicU64 = AtomicU64::new(0);

/// The plans of this process that took the sampled order so far.
pub(crate) fn changed_plans() -> u64 {
    CHANGED.load(Ordering::Relaxed)
}

/// The cost of an order by the greedy planner's estimates: the candidates its steps
/// visit, as [`Walk::run`] counts them from the sample.
fn estimated_cost(g: &Graph, rule: &CRule, ranges: &[(u32, u32)], order: &[usize]) -> f64 {
    let mut bound = vec![false; rule.nvars];
    let mut builtins: Vec<usize> = (0..rule.builtins.len()).collect();
    let mut scratch = Vec::new();
    let (mut card, mut cost) = (1.0f64, 0.0f64);
    for &i in order {
        place_builtins(rule, &mut bound, &mut builtins, &mut scratch);
        let a = &rule.atoms[i];
        let fanout = estimate(a, &bound, ranges[i], Some(g));
        cost += card * fanout.max(1.0);
        card *= fanout;
        bind_atom(a, &mut bound);
    }
    cost
}

/// The steps of the atoms in `order`, with each builtin placed as soon as its inputs are
/// bound, as [`plan_with`] places them. `None` when a builtin's inputs are never bound.
pub(crate) fn steps_in_order(
    rule: &CRule,
    ranges: &[(u32, u32)],
    order: &[usize],
) -> Option<Vec<Step>> {
    let mut bound = vec![false; rule.nvars];
    let mut steps = Vec::with_capacity(rule.atoms.len() + rule.builtins.len());
    let mut left: Vec<usize> = (0..rule.builtins.len()).collect();
    for &i in order {
        place_builtins(rule, &mut bound, &mut left, &mut steps);
        bind_atom(&rule.atoms[i], &mut bound);
        steps.push(Step::Atom {
            atom: i,
            lo: ranges[i].0,
            hi: ranges[i].1,
        });
    }
    place_builtins(rule, &mut bound, &mut left, &mut steps);
    left.is_empty().then_some(steps)
}

fn place_builtins(rule: &CRule, bound: &mut [bool], left: &mut Vec<usize>, steps: &mut Vec<Step>) {
    let mut progress = true;
    while progress {
        progress = false;
        left.retain(|&j| {
            let f = &rule.builtins[j];
            if f.inputs.iter().all(|&v| bound[v]) {
                steps.push(Step::Builtin(j));
                for &v in &f.outputs {
                    bound[v] = true;
                }
                progress = true;
                false
            } else {
                true
            }
        });
    }
}

fn bind_atom(a: &Atom, bound: &mut [bool]) {
    for s in [a.s, a.p, a.o] {
        if let Slot::Var(v) = s {
            bound[v] = true;
        }
    }
}

/// The atoms of `steps`, in their order.
fn atom_order(steps: &[Step]) -> Vec<usize> {
    steps
        .iter()
        .filter_map(|s| match *s {
            Step::Atom { atom, .. } => Some(atom),
            Step::Builtin(_) => None,
        })
        .collect()
}

/// Whether the order of the rule's atoms can change what the rule derives. A builtin
/// that is given a bound variable where it binds one checks the variable by value
/// ([`Eval::bind_and_continue`]), so that the integer 3 passes for the decimal 3.0, and
/// an atom matches the term itself. A variable that a builtin binds and that an atom or
/// another builtin also holds is therefore generated in one order and checked in
/// another, and the two can differ in the facts they derive and in the terms of those
/// facts.
pub(crate) fn order_matters(rule: &CRule) -> bool {
    rule.builtins.iter().enumerate().any(|(j, f)| {
        f.outputs.iter().any(|&v| {
            let holds = |s: Slot| s == Slot::Var(v);
            rule.atoms
                .iter()
                .any(|a| holds(a.s) || holds(a.p) || holds(a.o))
                || rule
                    .builtins
                    .iter()
                    .enumerate()
                    .any(|(k, other)| k != j && other.outputs.contains(&v))
        })
    })
}

/// Whether an order of sampled cost `cost` replaces the greedy order of sampled cost
/// `greedy`: when it costs at most the greedy order's cost divided by [`SAVING`]. Costs
/// that overflowed, or that are not numbers, say nothing, and the greedy order stays.
fn saves(cost: f64, greedy: f64) -> bool {
    cost.is_finite() && greedy.is_finite() && cost * SAVING <= greedy
}

/// The plan of `rule` over `ranges`: the greedy planner's, or the sampled order when
/// the sample makes it at least [`SAVING`] times cheaper. The flag says which.
pub(crate) fn plan(g: &Graph, rule: &CRule, ranges: &[(u32, u32)]) -> Option<(Vec<Step>, bool)> {
    plan_within(g, rule, ranges, PROBES)
}

/// [`plan`] with at most `probes` probes.
fn plan_within(
    g: &Graph,
    rule: &CRule,
    ranges: &[(u32, u32)],
    probes: usize,
) -> Option<(Vec<Step>, bool)> {
    let greedy = super::plan(rule, ranges, Some(g))?;
    if rule.atoms.len() < MIN_ATOMS {
        return Some((greedy, false));
    }
    // a rule whose order can change its facts keeps the order it had, and a graph with
    // dead facts has candidates that depend on its runs
    if order_matters(rule) || g.dead_rows() > 0 {
        return Some((greedy, false));
    }
    let greedy_order = atom_order(&greedy);
    // a body whose predicates are all constants has estimates from its relations'
    // statistics, and the sample is drawn for it only when they make the plan a large one
    let guessed = rule.atoms.iter().any(|a| !matches!(a.p, Slot::Const(_)));
    let search_all = SEARCH_ALL.load(Ordering::Relaxed);
    if !search_all && !guessed && estimated_cost(g, rule, ranges, &greedy_order) < LARGE_COST {
        return Some((greedy, false));
    }
    let mut memo = Memo::default();
    let mut walk = Walk {
        g,
        rule,
        ranges,
        memo: &mut memo,
        probes: FxHashMap::default(),
        held: Held::new(g.budget()),
        limit: probes,
        spent: false,
    };
    let chosen = (|| {
        // the greedy order first, with a few bindings: an order they find cheap is kept
        // without a search for another, which would cost more than it could save
        if !search_all && walk.run(Some(&greedy_order), FIRST_SAMPLE).1 < MIN_COST {
            return None;
        }
        let (_, greedy_cost) = walk.run(Some(&greedy_order), SAMPLE);
        let (order, cost) = walk.run(None, SAMPLE);
        // a walk that ran out of probes or of memory, or failed to read the store, has
        // no estimate
        if walk.spent || order == greedy_order || !saves(cost, greedy_cost) {
            return None;
        }
        let steps = steps_in_order(rule, ranges, &order)?;
        let (s, o) = index_needs(rule, &steps);
        let (gs, go) = index_needs(rule, &greedy);
        if (s && !gs) || (o && !go) {
            return None;
        }
        tracing::trace!(
            rule = %rule.name,
            greedy = ?greedy_order,
            sampled = ?order,
            greedy_cost,
            cost,
            "sampled order"
        );
        CHANGED.fetch_add(1, Ordering::Relaxed);
        Some(steps)
    })();
    if let Some(b) = g.base() {
        b.add_scans(memo.scans);
    }
    Some(match chosen {
        Some(steps) => (steps, true),
        None => (greedy, false),
    })
}

/// A pattern and its rows: the key of a probe.
type Pattern = ([Option<u64>; 3], u32, u32);

/// What a probe of a pattern found: its candidates, and the first few facts among them.
struct Probe {
    count: f64,
    facts: Vec<Triple>,
}

struct Walk<'a> {
    g: &'a Graph,
    rule: &'a CRule,
    ranges: &'a [(u32, u32)],
    memo: &'a mut Memo,
    /// the probes made so far: the walk of the greedy order and the search share the
    /// bindings of the steps they have in common, and a step's facts extend the sample
    /// after its count chose it
    probes: FxHashMap<Pattern, std::rc::Rc<Probe>>,
    /// the probes' charge to the run's memory budget, released when the plan is made
    held: Held,
    /// the probes the plan may make
    limit: usize,
    /// whether the plan ran out of probes or of memory, or a read of the store failed:
    /// its probes are empty from then on, and it keeps the greedy order
    spent: bool,
}

impl Walk<'_> {
    /// Walk an order of the rule's atoms with a sample of its bindings: `forced` when
    /// given, and otherwise the order that places the atom with the fewest sampled
    /// candidates at each step. Returns the order and its cost, the candidates its steps
    /// are estimated to visit. The sample keeps `keep` bindings.
    fn run(&mut self, forced: Option<&[usize]>, keep: usize) -> (Vec<usize>, f64) {
        let rule = self.rule;
        let mut bound = vec![false; rule.nvars];
        // the variables the sampled bindings have values of
        let mut known = vec![false; rule.nvars];
        let mut sample: Vec<Vec<u64>> = vec![vec![0u64; rule.nvars]];
        let mut left: Vec<usize> = (0..rule.atoms.len()).collect();
        let mut builtins: Vec<usize> = (0..rule.builtins.len()).collect();
        let mut order = Vec::with_capacity(left.len());
        let (mut card, mut cost) = (1.0f64, 0.0f64);
        let mut scratch = Vec::new();
        while !left.is_empty() {
            place_builtins(rule, &mut bound, &mut builtins, &mut scratch);
            let (at, fanout) = match forced {
                Some(f) => {
                    let i = f[order.len()];
                    let at = left.iter().position(|&x| x == i).unwrap();
                    (at, self.fanout(i, &bound, &known, &sample))
                }
                None => {
                    // the fewest sampled candidates, then the greedy planner's choice
                    let mut best: Option<(usize, f64, f64)> = None;
                    for (at, &i) in left.iter().enumerate() {
                        let f = self.fanout(i, &bound, &known, &sample);
                        let e = estimate(&rule.atoms[i], &bound, self.ranges[i], Some(self.g));
                        let better = match best {
                            None => true,
                            Some((_, bf, be)) => {
                                f.total_cmp(&bf).then(e.total_cmp(&be)) == std::cmp::Ordering::Less
                            }
                        };
                        if better {
                            best = Some((at, f, e));
                        }
                    }
                    let (at, f, _) = best.unwrap();
                    (at, f)
                }
            };
            let i = left.remove(at);
            // every binding of the prefix asks the atom once, and reads its candidates
            cost += card * fanout.max(1.0);
            card *= fanout;
            sample = self.extend(i, &bound, &sample, keep);
            let a = &rule.atoms[i];
            bind_atom(a, &mut bound);
            if !sample.is_empty() {
                bind_atom(a, &mut known);
            }
            order.push(i);
        }
        (order, cost)
    }

    /// The pattern of `a` under the binding `b`: the values of its subject, predicate and
    /// object, and whether a position holds a variable that is bound without a sampled
    /// value.
    fn pattern(a: &Atom, b: &[u64], bound: &[bool]) -> ([Option<u64>; 3], bool) {
        let mut blind = false;
        let v = [a.s, a.p, a.o].map(|s| match s {
            Slot::Const(c) => Some(c),
            Slot::Any => None,
            Slot::Var(v) if b[v] != 0 => Some(b[v]),
            Slot::Var(v) => {
                blind |= bound[v];
                None
            }
        });
        (v, blind)
    }

    /// The candidates atom `i` has under a binding of the steps so far: the mean over the
    /// sample, or the greedy planner's estimate when the sample cannot say.
    fn fanout(&mut self, i: usize, bound: &[bool], known: &[bool], sample: &[Vec<u64>]) -> f64 {
        let a = &self.rule.atoms[i];
        let (lo, hi) = self.ranges[i];
        let statistical = estimate(a, bound, (lo, hi), Some(self.g));
        // a variable bound by a builtin has no sampled value, and a pattern without a
        // predicate is counted over the whole tier only when its order exists
        let unknown = |s: Slot| matches!(s, Slot::Var(v) if bound[v] && !known[v]);
        let no_predicate = match a.p {
            Slot::Const(_) => false,
            Slot::Var(v) => !known[v],
            Slot::Any => true,
        };
        if sample.is_empty() || no_predicate || unknown(a.s) || unknown(a.o) {
            return statistical;
        }
        let mut total = 0.0;
        for b in sample {
            let (pattern, _) = Self::pattern(a, b, bound);
            total += self.probe(pattern, lo, hi).count;
        }
        total / sample.len() as f64
    }

    /// The candidates of a pattern in the rows `[lo, hi)` and up to [`SAMPLE`] facts
    /// among them, from one read of the derived tier's order and one of the store's
    /// index where the store's facts are few. A pattern with a predicate alone, or with
    /// nothing bound, takes its count from the statistics and its facts from along the
    /// relation.
    fn probe(&mut self, pattern: [Option<u64>; 3], lo: u32, hi: u32) -> std::rc::Rc<Probe> {
        if let Some(p) = self.probes.get(&(pattern, lo, hi)) {
            return p.clone();
        }
        if !self.spent
            && (self.probes.len() >= self.limit || self.held.try_grow(PROBE_BYTES).is_err())
        {
            self.spent = true;
        }
        if self.spent {
            return std::rc::Rc::new(Probe {
                count: 0.0,
                facts: Vec::new(),
            });
        }
        let p = std::rc::Rc::new(self.probe_now(pattern, lo, hi));
        self.probes.insert((pattern, lo, hi), p.clone());
        p
    }

    fn probe_now(&mut self, [s, p, o]: [Option<u64>; 3], lo: u32, hi: u32) -> Probe {
        let g = self.g;
        let base = g.base().filter(|_| g.reads_base(lo, hi));
        let matches = |t: &Triple| {
            s.is_none_or(|x| x == t[0])
                && p.is_none_or(|x| x == t[1])
                && o.is_none_or(|x| x == t[2])
        };
        let mut facts: Vec<Triple> = Vec::new();
        if let (Some(s), Some(p), Some(o)) = (s, p, o) {
            // a membership test
            let t = [s, p, o];
            let held = g
                .cands(Some(s), Some(p), Some(o), lo, hi)
                .iter()
                .any(|id| g.alive(id))
                || base.is_some_and(|b| match b.contains_with(self.memo, &t) {
                    Ok(held) => held,
                    Err(_) => {
                        self.spent = true;
                        false
                    }
                });
            if held {
                facts.push(t);
            }
            return Probe {
                count: facts.len() as f64,
                facts,
            };
        }
        let cands = g.cands(s, p, o, lo, hi);
        for id in cands.spread(VISIT) {
            if g.alive(id) {
                let t = g.triple(id);
                if matches(&t) {
                    facts.push(t);
                }
            }
        }
        if facts.len() > SAMPLE {
            let step = facts.len().div_ceil(SAMPLE);
            facts = facts.into_iter().step_by(step).collect();
        }
        let derived = cands.count() as f64;
        let Some(base) = base else {
            return Probe {
                count: derived,
                facts,
            };
        };
        if s.is_none() && o.is_none() {
            // a relation, or every fact: the statistics count it, and its facts come from
            // a few places along it
            let stored = match p {
                Some(p) => base.count_p(p) as f64,
                None => base.len() as f64,
            };
            let want = SAMPLE - facts.len().min(SAMPLE / 2);
            let mut some = Vec::new();
            let read = base.sample(self.memo, [s, p, o], want, STORE_STARTS, |t| {
                some.push(t);
                true
            });
            self.spent |= read.is_err();
            mix(&mut facts, some);
            return Probe {
                count: derived + stored,
                facts,
            };
        }
        // a probe with a subject or an object: its first facts, which are all of them
        // when they are few, and then the scan counted them. A scan of several blocks
        // is read in its first block alone, and the index counts it.
        let mut some = Vec::new();
        let read = if base.blocks(s, p, o) <= 1 {
            base.for_each_with(self.memo, s, p, o, |t| {
                some.push(t);
                some.len() <= SAMPLE
            })
        } else {
            base.sample(self.memo, [s, p, o], SAMPLE + 1, 1, |t| {
                some.push(t);
                true
            })
        };
        let ended = read.unwrap_or_else(|_| {
            self.spent = true;
            true
        });
        let stored = if ended {
            some.len() as f64
        } else {
            some.truncate(SAMPLE);
            base.count(s, p, o) as f64
        };
        mix(&mut facts, some);
        Probe {
            count: derived + stored,
            facts,
        }
    }

    /// The sample after atom `i`: bindings of `sample` extended by facts of the atom.
    fn extend(
        &mut self,
        i: usize,
        bound: &[bool],
        sample: &[Vec<u64>],
        keep: usize,
    ) -> Vec<Vec<u64>> {
        if sample.is_empty() {
            return Vec::new();
        }
        let g = self.g;
        let a = &self.rule.atoms[i];
        let (lo, hi) = self.ranges[i];
        let hidden = !matches!(a.p, Slot::Const(_));
        // a few facts for each binding, and more when the bindings are few
        let want = (2 * keep).div_ceil(sample.len()).clamp(1, keep);
        let mut out: Vec<Vec<u64>> = Vec::new();
        for b in sample {
            let (pattern, _) = Self::pattern(a, b, bound);
            let probe = self.probe(pattern, lo, hi);
            let mut got = 0;
            for t in &probe.facts {
                if hidden && g.is_hidden(t[1]) {
                    continue;
                }
                if let Some(nb) = extended(a, *t, b) {
                    out.push(nb);
                    got += 1;
                    if got == want {
                        break;
                    }
                }
            }
        }
        if out.len() > keep {
            let step = out.len().div_ceil(keep);
            out = out.into_iter().step_by(step).collect();
        }
        out
    }
}

/// The facts of the derived tier and of the store in one sample of at most [`SAMPLE`]:
/// all of both when they fit, and otherwise as many of each as of the other.
fn mix(facts: &mut Vec<Triple>, stored: Vec<Triple>) {
    if facts.len() + stored.len() > SAMPLE {
        let keep = (SAMPLE - stored.len().min(SAMPLE / 2)).min(facts.len());
        facts.truncate(keep);
    }
    let room = SAMPLE - facts.len();
    facts.extend(stored.into_iter().take(room));
}

/// The binding `b` with the variables of `a` set from fact `t`, when the fact matches
/// the atom under it.
fn extended(a: &Atom, t: Triple, b: &[u64]) -> Option<Vec<u64>> {
    let mut nb = b.to_vec();
    for (slot, v) in [(a.s, t[0]), (a.p, t[1]), (a.o, t[2])] {
        match slot {
            Slot::Const(c) if c != v => return None,
            Slot::Var(x) if nb[x] == 0 => nb[x] = v,
            Slot::Var(x) if nb[x] != v => return None,
            _ => {}
        }
    }
    Some(nb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles_core::store::{Store, StoreOptions};

    const KEY: u64 = 1000;
    const FIRST: u64 = 1001;
    const REST: u64 = 1002;
    const NIL: u64 = 1003;
    const TYPE: u64 = 1004;
    const ID: u64 = 1005;
    const SAME: u64 = 1006;
    const PERSON: u64 = 2000;
    const LIST: u64 = 3000;

    fn atom(s: Slot, p: Slot, o: Slot) -> Atom {
        Atom { s, p, o }
    }

    /// `prp-key` for one key property, without its `notEqual`:
    /// `(?c hasKey ?l) (?l first ?p) (?l rest nil) (?x type ?c) (?x ?p ?z) (?y type ?c)
    /// (?y ?p ?z) -> (?x sameAs ?y)`.
    fn key_rule() -> CRule {
        use Slot::{Const, Var};
        let (c, l, p, x, z, y) = (Var(0), Var(1), Var(2), Var(3), Var(4), Var(5));
        CRule {
            name: "key".into(),
            nvars: 6,
            builtins: vec![],
            atoms: vec![
                atom(c, Const(KEY), l),
                atom(l, Const(FIRST), p),
                atom(l, Const(REST), Const(NIL)),
                atom(x, Const(TYPE), c),
                atom(x, p, z),
                atom(y, Const(TYPE), c),
                atom(y, p, z),
            ],
            head: vec![CHead::Triple(x, Const(SAME), y)],
        }
    }

    /// A keyed class of `members` individuals, two of them with one key value, beside 60
    /// classes of five members and 40 properties of 200 facts, so that the average class
    /// is small and the average property is not.
    fn key_graph(members: u64) -> Graph {
        key_graph_in(members, usize::MAX)
    }

    /// [`key_graph`] with its facts added `batch` at a time, each batch a run of its own.
    fn key_graph_in(members: u64, batch: usize) -> Graph {
        let mut g = Graph::default();
        let mut ts: Vec<Triple> = vec![[PERSON, KEY, LIST], [LIST, FIRST, ID], [LIST, REST, NIL]];
        for i in 0..members {
            ts.push([10_000 + i, TYPE, PERSON]);
            ts.push([10_000 + i, ID, 50_000 + i.min(members - 2)]);
        }
        for c in 0..60 {
            for i in 0..5 {
                ts.push([20_000 + c * 5 + i, TYPE, 2001 + c]);
            }
        }
        for p in 0..40 {
            for i in 0..200 {
                ts.push([30_000 + i, 4000 + p, 60_000 + i]);
            }
        }
        for chunk in ts.chunks(batch.min(ts.len())) {
            g.add_batch(chunk.iter().copied());
        }
        g
    }

    /// The same facts plan the same way however they fall into runs, as the greedy
    /// planner's statistics follow the facts and not the runs.
    #[test]
    fn sampled_orders_follow_the_facts_not_the_runs() {
        let r = key_rule();
        let whole = key_graph(400);
        let ranges = vec![(0, whole.len()); r.atoms.len()];
        let (steps, by_sample) = plan(&whole, &r, &ranges).unwrap();
        assert!(by_sample);
        for batch in [97, 1000, 5000] {
            let split = key_graph_in(400, batch);
            assert_eq!(split.len(), whole.len());
            let (other, by_sample) = plan(&split, &r, &ranges).unwrap();
            assert!(by_sample, "batches of {batch}");
            assert_eq!(atom_order(&other), atom_order(&steps), "batches of {batch}");
            // and a part of the facts, which cuts the runs of the whole graph
            let part = vec![(0, 5000); r.atoms.len()];
            assert_eq!(
                plan(&split, &r, &part).map(|p| atom_order(&p.0)),
                plan(&whole, &r, &part).map(|p| atom_order(&p.0)),
                "batches of {batch}"
            );
        }
    }

    fn position(steps: &[Step], a: usize) -> usize {
        atom_order(steps).iter().position(|&x| x == a).unwrap()
    }

    /// (candidate rows visited, derived facts) of a plan.
    fn evaluate(g: &Graph, r: &CRule, steps: &[Step]) -> (u64, Vec<Triple>) {
        let t = Terms::new(Store::in_memory(StoreOptions::default()).snapshot());
        let abort = AtomicBool::new(false);
        let budget = Budget::unlimited();
        let mut ev = Eval::new(
            g,
            &t,
            r,
            steps,
            g.len(),
            &abort,
            None,
            usize::MAX,
            Mode::New,
            &budget,
        );
        ev.run(0, &mut vec![0u64; r.nvars], None);
        let rows = ev.stats[0];
        let mut out = ev.take_out().0;
        out.sort_unstable();
        (rows, out)
    }

    /// The greedy order joins the keyed class with itself before it reads a key value,
    /// and the sampled order reads the key value of each member first. Both derive the
    /// same facts, and the sampled order visits a small share of the rows.
    #[test]
    fn a_key_is_read_before_the_class_is_joined_with_itself() {
        let members = 400;
        let g = key_graph(members);
        let r = key_rule();
        let ranges = vec![(0, g.len()); r.atoms.len()];
        let greedy = super::super::plan(&r, &ranges, Some(&g)).unwrap();
        assert!(
            position(&greedy, 5) < position(&greedy, 4)
                && position(&greedy, 5) < position(&greedy, 6),
            "the greedy order no longer joins the class with itself: {greedy:?}"
        );
        let (sampled, by_sample) = plan(&g, &r, &ranges).unwrap();
        assert!(by_sample, "{sampled:?}");
        // each member's key value, then the other holder of the value, then its class
        assert!(position(&sampled, 4) < position(&sampled, 6));
        assert!(position(&sampled, 6) < position(&sampled, 5), "{sampled:?}");
        let (greedy_rows, greedy_out) = evaluate(&g, &r, &greedy);
        let (sampled_rows, sampled_out) = evaluate(&g, &r, &sampled);
        assert_eq!(greedy_out, sampled_out);
        // each member with itself, and the two that share a value with each other
        assert_eq!(sampled_out.len(), members as usize + 2);
        assert!(greedy_rows >= members * members, "{greedy_rows}");
        assert!(sampled_rows < 8 * members, "{sampled_rows}");
        // the same graph plans the same way each time
        assert_eq!(
            atom_order(&plan(&g, &r, &ranges).unwrap().0),
            atom_order(&sampled)
        );
    }

    /// Bodies of two atoms keep the greedy order, and so does a body whose sampled order
    /// is not clearly cheaper.
    #[test]
    fn the_greedy_order_stays_without_a_clear_saving() {
        use Slot::{Const, Var};
        let g = key_graph(50);
        let two = CRule {
            name: "two".into(),
            nvars: 3,
            builtins: vec![],
            atoms: vec![
                atom(Var(0), Const(TYPE), Const(PERSON)),
                atom(Var(0), Const(ID), Var(1)),
            ],
            head: vec![CHead::Triple(Var(0), Const(SAME), Var(0))],
        };
        let ranges = vec![(0, g.len()); 2];
        let (steps, by_sample) = plan(&g, &two, &ranges).unwrap();
        assert!(!by_sample);
        assert_eq!(
            atom_order(&steps),
            atom_order(&super::super::plan(&two, &ranges, Some(&g)).unwrap())
        );
        // a chain whose atoms all have one match per binding: any order costs the same
        let chain = CRule {
            name: "chain".into(),
            nvars: 3,
            builtins: vec![],
            atoms: vec![
                atom(Var(0), Const(4000), Var(1)),
                atom(Var(0), Const(4001), Var(1)),
                atom(Var(0), Const(4002), Var(2)),
            ],
            head: vec![CHead::Triple(Var(0), Const(SAME), Var(2))],
        };
        let ranges = vec![(0, g.len()); 3];
        let (steps, by_sample) = plan(&g, &chain, &ranges).unwrap();
        assert!(!by_sample);
        assert_eq!(
            atom_order(&steps),
            atom_order(&super::super::plan(&chain, &ranges, Some(&g)).unwrap())
        );
    }

    /// An order that would need an order over the whole tier that the greedy order does
    /// not need is not taken.
    #[test]
    fn no_order_over_the_whole_tier_is_added() {
        let g = key_graph(400);
        let r = key_rule();
        let ranges = vec![(0, g.len()); r.atoms.len()];
        let (steps, _) = plan(&g, &r, &ranges).unwrap();
        let greedy = super::super::plan(&r, &ranges, Some(&g)).unwrap();
        let (s, o) = index_needs(&r, &steps);
        let (gs, go) = index_needs(&r, &greedy);
        assert!((!s || gs) && (!o || go));
    }

    /// A graph that holds a dead fact keeps the greedy order, however its facts fall
    /// into runs: an order's candidates count a dead fact until a compaction drops it.
    #[test]
    fn a_graph_with_dead_facts_keeps_the_greedy_order() {
        let r = key_rule();
        for batch in [usize::MAX, 97, 5000] {
            let mut g = key_graph_in(400, batch);
            let ranges = vec![(0, g.len()); r.atoms.len()];
            assert!(plan(&g, &r, &ranges).unwrap().1, "batches of {batch}");
            assert!(g.kill(&[20_000, TYPE, 2001]));
            let (steps, by_sample) = plan(&g, &r, &ranges).unwrap();
            assert!(!by_sample, "batches of {batch}");
            assert_eq!(
                atom_order(&steps),
                atom_order(&super::super::plan(&r, &ranges, Some(&g)).unwrap())
            );
        }
    }

    /// A plan that runs out of probes, or whose probes the memory budget cannot hold,
    /// keeps the greedy order, and the budget is charged nothing once the plan is made.
    #[test]
    fn a_plan_out_of_probes_or_memory_keeps_the_greedy_order() {
        let r = key_rule();
        let mut g = key_graph(400);
        let ranges = vec![(0, g.len()); r.atoms.len()];
        let greedy = atom_order(&super::super::plan(&r, &ranges, Some(&g)).unwrap());
        assert!(plan(&g, &r, &ranges).unwrap().1);
        let (steps, by_sample) = plan_within(&g, &r, &ranges, 20).unwrap();
        assert!(!by_sample);
        assert_eq!(atom_order(&steps), greedy);
        // a budget with room for the graph and for ten probes
        let used = g.budget().used();
        let budget = Budget::new(Some(g.heap_bytes() + 10 * PROBE_BYTES));
        g.set_budget(&budget).unwrap();
        let before = budget.used();
        let (steps, by_sample) = plan(&g, &r, &ranges).unwrap();
        assert!(!by_sample, "used {used} of {before}");
        assert_eq!(atom_order(&steps), greedy);
        assert_eq!(budget.used(), before);
        // and with room for all of them the plan is the sampled one, and its probes'
        // charge is released
        let budget = Budget::new(Some(g.heap_bytes() + PROBES * PROBE_BYTES));
        g.set_budget(&budget).unwrap();
        let before = budget.used();
        assert!(plan(&g, &r, &ranges).unwrap().1);
        assert_eq!(budget.used(), before);
        assert!(budget.peak() > before);
    }

    /// Costs that overflowed, or that are not numbers, never replace the greedy order.
    #[test]
    fn costs_that_overflowed_keep_the_greedy_order() {
        assert!(saves(10.0, 20.0));
        assert!(saves(0.0, 1.0));
        assert!(!saves(10.0, 19.0));
        // the product of many large fanouts, in both orders or in one
        assert!(!saves(f64::INFINITY, f64::INFINITY));
        assert!(!saves(1.0, f64::INFINITY));
        assert!(!saves(f64::INFINITY, 1.0));
        // infinity times a fanout of zero
        assert!(!saves(f64::NAN, 1.0));
        assert!(!saves(1.0, f64::NAN));
        assert!(!saves(f64::NAN, f64::NAN));
    }

    /// A variable that a builtin binds and an atom also holds makes the order matter,
    /// and such a rule keeps the greedy order. A builtin that only tests its
    /// arguments, or whose result goes to the head alone, does not.
    #[test]
    fn a_rule_whose_order_matters_keeps_the_greedy_order() {
        use Slot::{Const, Var};
        let builtin = |kind: BuiltinKind, args: Vec<Slot>| {
            let var = |i: usize| match args[i] {
                Var(v) => Some(v),
                _ => None,
            };
            CBuiltin {
                origin: 0,
                kind,
                inputs: kind
                    .input_positions(args.len())
                    .into_iter()
                    .filter_map(var)
                    .collect(),
                outputs: kind
                    .output_positions(args.len())
                    .into_iter()
                    .filter_map(var)
                    .collect(),
                args: args.clone(),
                regex: None,
            }
        };
        let mut r = key_rule();
        assert!(!order_matters(&r));
        // a test of two bound variables
        r.builtins = vec![builtin(BuiltinKind::NotEqual, vec![Var(3), Var(5)])];
        assert!(!order_matters(&r));
        // a sum that goes to a variable of the head alone
        r.nvars = 7;
        r.builtins = vec![builtin(BuiltinKind::Sum, vec![Var(4), Const(1), Var(6)])];
        assert!(!order_matters(&r));
        let g = key_graph(400);
        let ranges = vec![(0, g.len()); r.atoms.len()];
        assert!(plan(&g, &r, &ranges).unwrap().1);
        // two builtins that bind one variable
        r.builtins
            .push(builtin(BuiltinKind::Sum, vec![Var(4), Const(2), Var(6)]));
        assert!(order_matters(&r));
        // a sum that an atom reads: the key value of `?y` is the sum
        r.nvars = 6;
        r.builtins = vec![builtin(BuiltinKind::Sum, vec![Var(3), Const(1), Var(4)])];
        assert!(order_matters(&r));
        let (steps, by_sample) = plan(&g, &r, &ranges).unwrap();
        assert!(!by_sample);
        assert_eq!(
            atom_order(&steps),
            atom_order(&super::super::plan(&r, &ranges, Some(&g)).unwrap())
        );
    }

    #[test]
    fn steps_follow_the_order_and_place_builtins_early() {
        let r = key_rule();
        let ranges: Vec<(u32, u32)> = (0..7).map(|i| (i, i + 10)).collect();
        let order = [3, 0, 1, 2, 4, 6, 5];
        let steps = steps_in_order(&r, &ranges, &order).unwrap();
        assert_eq!(atom_order(&steps), order);
        for s in &steps {
            let Step::Atom { atom, lo, hi } = *s else {
                panic!("a builtin in a rule without one");
            };
            assert_eq!((lo, hi), ranges[atom]);
        }
    }
}
