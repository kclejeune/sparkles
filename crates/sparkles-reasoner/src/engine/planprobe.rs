//! A diagnostic of rule-body planning (roadmap item 939): the cost of every atom order
//! of each plan of an iteration, measured on the facts the iteration reads.
//!
//! For each plan the probe evaluates the planner's order and then every other order of
//! its atoms, on one thread each, with the builtins placed as the planner places them.
//! An order stops once it has run for longer than the best so far. Bodies of more than
//! six atoms try a sample of their orders. The probe prints one
//! line per plan to the standard error, with the time, the rows visited and the facts
//! matched of the planner's order and of the fastest order, and discards what the orders
//! derive. The run itself continues with the planner's plans, so it derives the same
//! facts with and without the probe.
//!
//! The probe is compiled only with the crate's `plan-probe` feature, which no released
//! build turns on, and it is switched on for a whole process. It perturbs the run it
//! measures, so a probed run is for its probe lines and not for its time or its memory.
//! The engine builds both orders over the whole tier before it probes, which the run's
//! memory budget is charged for and which can make a run with a tight budget fail. The
//! evaluations of the orders are charged to no budget, and the first evaluations of the
//! planner's own order have no time limit, as the run's evaluation of it has none. A
//! plan of a rule whose builtins bind variables is not probed, since each evaluation
//! would add the terms those builtins make to the run's terms.

use super::sampled::steps_in_order;
use super::*;
use std::time::{Duration, Instant};

static PROBE: AtomicBool = AtomicBool::new(false);

/// Turn the probe on or off for the runs of this process.
pub(crate) fn set(on: bool) {
    PROBE.store(on, Ordering::Relaxed);
}

pub(crate) fn enabled() -> bool {
    PROBE.load(Ordering::Relaxed)
}

/// Every order is tried for bodies of up to this many atoms, and a sample of
/// [`SAMPLE`] orders for longer ones.
const MAX_ATOMS: usize = 6;
const SAMPLE: usize = 3000;
/// The time the orders of one plan may take in all.
const TASK_BUDGET: Duration = Duration::from_secs(30);

fn permutations(n: usize) -> Vec<Vec<usize>> {
    fn go(left: &mut Vec<usize>, cur: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if left.is_empty() {
            out.push(cur.clone());
            return;
        }
        for k in 0..left.len() {
            let x = left.remove(k);
            cur.push(x);
            go(left, cur, out);
            cur.pop();
            left.insert(k, x);
        }
    }
    let mut out = Vec::new();
    go(&mut (0..n).collect(), &mut Vec::new(), &mut out);
    out
}

/// Up to [`SAMPLE`] orders of the rule's atoms in which each atom after the first shares
/// a variable with the atoms before it when one does, drawn with a fixed seed.
fn sample(rule: &CRule) -> Vec<Vec<usize>> {
    let n = rule.atoms.len();
    let vars = |i: usize| {
        let a = &rule.atoms[i];
        [a.s, a.p, a.o].into_iter().filter_map(|s| match s {
            Slot::Var(v) => Some(v),
            _ => None,
        })
    };
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = |m: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % m
    };
    let mut seen: FxHashSet<Vec<usize>> = FxHashSet::default();
    let mut out = Vec::new();
    for _ in 0..SAMPLE * 4 {
        if out.len() == SAMPLE {
            break;
        }
        let mut bound = vec![false; rule.nvars];
        let mut left: Vec<usize> = (0..n).collect();
        let mut order = Vec::with_capacity(n);
        while !left.is_empty() {
            let joined: Vec<usize> = left
                .iter()
                .copied()
                .filter(|&i| vars(i).any(|v| bound[v]))
                .collect();
            let pool = if joined.is_empty() { &left } else { &joined };
            let i = pool[next(pool.len())];
            left.retain(|&x| x != i);
            for v in vars(i) {
                bound[v] = true;
            }
            order.push(i);
        }
        if seen.insert(order.clone()) {
            out.push(order);
        }
    }
    out
}

struct Run {
    micros: u64,
    stats: [u64; 4],
    out: usize,
    stopped: bool,
}

/// Probe the plans `tasks` of an iteration over the delta `[ds, de)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn probe(
    g: &Graph,
    rules: &[CRule],
    terms: &Terms,
    tasks: &[(usize, Vec<Step>)],
    iteration: usize,
    replays: bool,
    ds: u32,
    de: u32,
) {
    let abort = AtomicBool::new(false);
    let budget = Budget::unlimited();
    let run = |r: &CRule, steps: &[Step], limit: Option<Duration>| -> Run {
        let mut ev = Eval::new(
            g,
            terms,
            r,
            steps,
            de,
            &abort,
            None,
            usize::MAX,
            Mode::New,
            &budget,
        );
        if replays && implicit_fact_delta(g, r, ds, de) {
            ev.delta_since = Some(ds);
        }
        let t = Instant::now();
        ev.set_deadline(limit.map(|l| t + l));
        ev.run(0, &mut vec![0u64; r.nvars], None);
        Run {
            micros: t.elapsed().as_micros() as u64,
            stats: ev.stats,
            out: ev.out.len(),
            stopped: ev.stopped() || ev.err.is_some(),
        }
    };
    let lines: Vec<String> = tasks
        .par_iter()
        .map(|(ri, steps)| {
            let r = &rules[*ri];
            if r.builtins.iter().any(|f| !f.outputs.is_empty()) {
                return format!(
                    "PLANPROBE-SKIPPED\t{iteration}\t{}\ta builtin binds a variable",
                    r.name
                );
            }
            let mut ranges = vec![(0, 0); r.atoms.len()];
            let mut greedy = Vec::new();
            for s in steps {
                if let Step::Atom { atom, lo, hi } = *s {
                    ranges[atom] = (lo, hi);
                    greedy.push(atom);
                }
            }
            let first = run(r, steps, None);
            let again = run(r, steps, None);
            let mut greedy_micros = first.micros.min(again.micros);
            let mut best: (u64, Vec<usize>, [u64; 4]) =
                (greedy_micros, greedy.clone(), again.stats);
            let mut tried = 1usize;
            let mut differs = 0usize;
            if greedy.len() >= 2 {
                let orders = if greedy.len() <= MAX_ATOMS {
                    permutations(greedy.len())
                } else {
                    sample(r)
                };
                let started = Instant::now();
                for order in orders {
                    if order == greedy {
                        continue;
                    }
                    if started.elapsed() > TASK_BUDGET {
                        break;
                    }
                    let Some(st) = steps_in_order(r, &ranges, &order) else {
                        continue;
                    };
                    tried += 1;
                    let limit = Duration::from_micros(best.0 + 100);
                    let x = run(r, &st, Some(limit));
                    if x.stopped {
                        continue;
                    }
                    if x.out != again.out {
                        differs += 1;
                    }
                    if x.micros < best.0 {
                        best = (x.micros, order, x.stats);
                    }
                }
            }
            // the fastest order was chosen among noisy times: time it and the planner's
            // order again, so that the reported pair is not the selection's luck
            if best.1 != greedy {
                let st = steps_in_order(r, &ranges, &best.1).unwrap();
                let (mut b, mut gr) = (u64::MAX, u64::MAX);
                for _ in 0..2 {
                    b = b.min(run(r, &st, None).micros);
                    gr = gr.min(run(r, steps, None).micros);
                }
                best.0 = b;
                greedy_micros = gr;
            }
            let delta = (0..r.atoms.len())
                .find(|&i| ranges[i].0 == ds && ranges[i].0 != 0)
                .map_or(-1, |i| i as i64);
            format!(
                "PLANPROBE\t{iteration}\t{}\t{}\t{delta}\t{:?}\t{greedy_micros}\t{}\t{}\t{}\t{}\t{:?}\t{}\t{}\t{}\t{tried}\t{differs}",
                r.name,
                r.atoms.len(),
                greedy,
                again.stats[0],
                again.stats[1],
                again.stats[2],
                again.out,
                best.1,
                best.0,
                best.2[0],
                best.2[1],
            )
        })
        .collect();
    for l in lines {
        eprintln!("{l}");
    }
}
