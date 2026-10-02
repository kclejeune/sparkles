//! Join ordering on cost summaries against the exhaustive program over plan trees:
//! random stars, chains, cycles, snowflakes and other connected groups of 2 to 16
//! patterns, with and without filters, over a random store. Both give the same
//! solutions. Up to ten patterns, the plan on summaries costs no more than the
//! exhaustive program's, and from 13 patterns on, no more than the greedy plan the
//! planner made before. Building the plan checks that every summary matches its plan's
//! cost and rows exactly.

use super::indexjoin::FORCE_INDEX_JOIN;
use super::plan::Node;
use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

const PREFIXES: &str = "PREFIX ex: <http://ex.org/> ";

fn xorshift(mut x: u64) -> impl FnMut() -> u64 {
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

/// Subjects `ex:s0…` of classes `ex:C0…ex:C3` with mostly one value of each of
/// `ex:p0…ex:p5`: objects `ex:o0…`, integers or strings for `ex:p0…ex:p2`, other
/// subjects for `ex:p3` and `ex:p4`, integers for `ex:p5`.
fn random_ttl(seed: u64, n: usize) -> String {
    let mut next = xorshift(seed);
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..n {
        t.push_str(&format!("ex:s{i} a ex:C{} .\n", next() % 4));
        for p in 0..6 {
            let count = match next() % 10 {
                0 | 1 => 0,
                9 => 2,
                _ => 1,
            };
            for _ in 0..count {
                let o = match (p, next() % 3) {
                    (3 | 4, _) => format!("ex:s{}", next() % n as u64),
                    (5, _) | (_, 0) => format!("{}", next() % 10),
                    (_, 1) => format!("ex:o{}", next() % 6),
                    _ => format!("\"v{}\"", next() % 5),
                };
                t.push_str(&format!("ex:s{i} ex:p{p} {o} .\n"));
            }
        }
    }
    t
}

/// A random connected group of `n` patterns of the given shape, with a filter now and
/// then.
fn random_group(next: &mut impl FnMut() -> u64, shape: u64, n: usize) -> String {
    let mut pats: Vec<String> = Vec::new();
    let link = |next: &mut dyn FnMut() -> u64| format!("ex:p{}", 3 + next() % 2);
    let value = |next: &mut dyn FnMut() -> u64, i: usize| match next() % 8 {
        0 => format!("ex:o{}", next() % 6),
        _ => format!("?o{i}"),
    };
    let mut vars = 1;
    match shape {
        // a star on one subject
        0 => {
            pats.push(format!("?v0 a ex:C{}", next() % 4));
            for i in 1..n {
                pats.push(format!("?v0 ex:p{} {}", next() % 6, value(next, i)));
            }
        }
        // a chain, or a cycle when it closes on its start
        1 | 2 => {
            for i in 0..n {
                let to = if shape == 2 && i == n - 1 { 0 } else { i + 1 };
                pats.push(format!("?v{i} {} ?v{to}", link(next)));
            }
            vars = n + 1;
        }
        // a snowflake: a chain whose nodes have a few values each
        3 => {
            let mut i = 0;
            while pats.len() < n {
                if next().is_multiple_of(3) && vars > 1 {
                    let v = next() as usize % vars;
                    pats.push(format!("?v{v} ex:p{} {}", next() % 3, value(next, i)));
                } else {
                    pats.push(format!("?v{} {} ?v{vars}", vars - 1, link(next)));
                    vars += 1;
                }
                i += 1;
            }
        }
        // anything connected
        _ => {
            for i in 0..n {
                let s = format!("?v{}", next() as usize % vars);
                let (p, o) = if next().is_multiple_of(2) {
                    let o = if next().is_multiple_of(4) && vars > 2 {
                        format!("?v{}", next() as usize % vars)
                    } else {
                        vars += 1;
                        format!("?v{}", vars - 1)
                    };
                    (link(next), o)
                } else {
                    (format!("ex:p{}", next() % 6), value(next, i))
                };
                if next().is_multiple_of(3) && o.starts_with("?v") {
                    pats.push(format!("{o} {p} {s}"));
                } else {
                    pats.push(format!("{s} {p} {o}"));
                }
            }
        }
    }
    let mut q = format!("SELECT * WHERE {{ {} .", pats.join(" . "));
    match next() % 5 {
        0 => q.push_str(&format!(" FILTER(?v0 != ex:s{})", next() % 50)),
        1 if vars > 1 => q.push_str(&format!(" FILTER(?v0 != ?v{})", vars - 1)),
        2 => q.push_str(" FILTER(STRLEN(STR(?v0)) > 4)"),
        _ => {}
    }
    q.push_str(" }");
    q
}

fn pruned_off() -> Optimizations {
    Optimizations::ALL.disable("pruned_join_order").unwrap()
}

/// The plan of `q`.
fn plan(snap: &Arc<Snapshot>, q: &str, opt: Optimizations) -> Node {
    let text = format!("{PREFIXES}{q}");
    let parsed = parse_query(&text, None, &[]).unwrap_or_else(|e| panic!("{q}: {e}"));
    let (pattern, dataset, base) = split(&parsed);
    let opts = QueryOptions {
        optimizations: Some(opt),
        no_cache: true,
        ..Default::default()
    };
    let ctx = make_ctx(snap.clone(), &opts, dataset, base);
    Planner::new(&ctx)
        .plan(pattern, &ActiveGraph::Default, Vec::new())
        .unwrap_or_else(|e| panic!("{q}: {e}"))
}

/// The solutions of `q`, sorted.
fn answer(snap: &Arc<Snapshot>, q: &str, opt: Optimizations) -> Vec<String> {
    let opts = QueryOptions {
        optimizations: Some(opt),
        no_cache: true,
        ..Default::default()
    };
    let r = query(snap.clone(), &format!("{PREFIXES}{q}"), &opts)
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut rows: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| t.map_or("UNDEF".to_string(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    rows.sort();
    rows
}

fn store(seed: u64, n: usize) -> Arc<Snapshot> {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        random_ttl(seed, n).into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.snapshot()
}

/// Check one group: the same solutions both ways, and the cost the sizes promise. The
/// exhaustive program is slow on dense groups of more than nine patterns in a debug
/// build, so those are only planned and run on summaries.
fn check(snap: &Arc<Snapshot>, q: &str, n: usize, dense: bool, opt: Optimizations) {
    if dense && (10..=12).contains(&n) {
        plan(snap, q, opt);
        answer(snap, q, opt);
        return;
    }
    let off = Optimizations {
        pruned_join_order: false,
        ..opt
    };
    let (new, old) = (plan(snap, q, opt), plan(snap, q, off));
    if n <= 10 || n >= 13 {
        assert!(
            new.cost <= old.cost * (1.0 + 1e-12),
            "{n} patterns cost {} instead of {}: {q}",
            new.cost,
            old.cost
        );
    }
    assert_eq!(answer(snap, q, opt), answer(snap, q, off), "{q}");
}

#[test]
fn ordering_on_summaries_matches_the_exhaustive_program() {
    let snap = store(0x9e37_79b9_7f4a_7c15, 150);
    let mut next = xorshift(0x2545_f491_4f6c_dd1d);
    for round in 0..120 {
        let n = 2 + round % 15;
        let shape = round as u64 % 5;
        let q = random_group(&mut next, shape, n);
        check(&snap, &q, n, shape.is_multiple_of(4), Optimizations::ALL);
    }
}

/// Index joins off, and forced wherever they apply: the summaries follow the costs
/// either way.
#[test]
fn ordering_on_summaries_with_index_joins_off_and_forced() {
    let snap = store(0x5851_f42d_4c95_7f2d, 120);
    let mut next = xorshift(0x1405_7b7e_f767_814f);
    let no_index = Optimizations::ALL.disable("batched_join").unwrap();
    for round in 0..40 {
        let n = 2 + round % 11;
        let shape = round as u64 % 5;
        let q = random_group(&mut next, shape, n);
        check(&snap, &q, n, shape.is_multiple_of(4), no_index);
        // forced index joins cost their input and one more row, so many plans tie on
        // cost and differ in rows: only the solutions are compared
        FORCE_INDEX_JOIN.with(|f| f.set(true));
        let off = pruned_off();
        if !shape.is_multiple_of(4) || n < 10 {
            assert_eq!(
                answer(&snap, &q, Optimizations::ALL),
                answer(&snap, &q, off),
                "{q}"
            );
        }
        FORCE_INDEX_JOIN.with(|f| f.set(false));
    }
}

/// Stars, where every subset of the patterns is connected, cost no more than the
/// exhaustive program's plans.
#[test]
fn stars_cost_no_more() {
    let snap = store(0x94d0_49bb_1331_11eb, 200);
    let mut next = xorshift(0xbf58_476d_1ce4_e5b9);
    for n in 2..=9 {
        for _ in 0..2 {
            let q = random_group(&mut next, 0, n);
            check(&snap, &q, n, true, Optimizations::ALL);
        }
    }
}

/// Hundreds of patterns plan in bounded time: greedily, with every join costed once
/// per pair of plans.
#[test]
fn large_groups_plan_quickly() {
    let snap = store(0xd6e8_feb8_6659_fd93, 100);
    let mut next = xorshift(0x9e37_79b9_7f4a_7c15);
    for (shape, n) in [(0, 200), (4, 400), (3, 60)] {
        let q = random_group(&mut next, shape, n);
        let t = Instant::now();
        plan(&snap, &q, Optimizations::ALL);
        let took = t.elapsed();
        assert!(
            took < Duration::from_secs(20),
            "{n} patterns took {took:?} to plan"
        );
    }
}
