//! Hash joins over a flat table, merge left joins and galloping index joins: random
//! tables (duplicates, unbound values, one to three shared variables in any column
//! order) and random queries over stores of one or many blocks give the same rows with
//! each switch on and off. The flat hash join gives the rows of a nested-loop join.

use super::exec::{join_tables, left_join};
use super::indexjoin_tests::random_trig;
use super::table::{Table, VarId};
use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

const PREFIXES: &str =
    "PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

fn xorshift(mut x: u64) -> impl FnMut() -> u64 {
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

/// A table over `vars` of `n` rows with ids `1..=domain`, one in `undef` of them unbound
/// (none when `undef` is 0) except in `bound` columns.
fn table(
    next: &mut impl FnMut() -> u64,
    vars: &[VarId],
    n: usize,
    domain: u64,
    undef: u64,
    bound: &[VarId],
) -> Table {
    let mut t = Table::new(vars.to_vec());
    for _ in 0..n {
        let row: Vec<Id> = vars
            .iter()
            .map(|v| {
                if undef > 0 && !bound.contains(v) && next().is_multiple_of(undef) {
                    Id::UNDEF
                } else {
                    Id(1 + next() % domain)
                }
            })
            .collect();
        t.push_row(&row);
    }
    t
}

fn ctx_with(s: &Store, opt: Optimizations) -> Ctx {
    let mut ctx = Ctx::new(s.snapshot());
    ctx.opt = opt;
    ctx
}

/// The rows of `t` in the column order of `vars`, sorted.
fn bag(t: &Table, vars: &[VarId]) -> Vec<Vec<Id>> {
    let p = t.clone().project(vars);
    let mut rows: Vec<Vec<Id>> = (0..p.len()).map(|i| p.row(i)).collect();
    rows.sort();
    rows
}

/// Join shapes: the left and right variables (`k`, `m`, `x` shared in some of them).
fn shapes(ctx: &Ctx) -> Vec<(Vec<VarId>, Vec<VarId>)> {
    let [k, m, x, a, b, c] = ["k", "m", "x", "a", "b", "c"].map(|n| ctx.var(n));
    vec![
        (vec![a, k], vec![k, b]),
        (vec![k, a, c], vec![b, k]),
        (vec![k, m, a], vec![m, b, k]),
        (vec![x, k, m, a], vec![k, x, m]),
        (vec![a, k], vec![b, c]),
    ]
}

/// The rows of the join of `l` and `r` over `vars`, by comparing every pair of rows, with
/// the unmatched rows of `l` when `left` is set (an OPTIONAL without a condition).
fn nested_loop(l: &Table, r: &Table, vars: &[VarId], left: bool) -> Vec<Vec<Id>> {
    let value = |t: &Table, i: usize, v: VarId| t.col_of(v).map_or(Id::UNDEF, |c| t.cols[c][i]);
    let mut rows = Vec::new();
    for i in 0..l.len() {
        let mut matched = false;
        for j in 0..r.len() {
            let fits = l.vars.iter().filter(|v| r.vars.contains(v)).all(|&v| {
                let (a, b) = (value(l, i, v), value(r, j, v));
                a == Id::UNDEF || b == Id::UNDEF || a == b
            });
            if fits {
                matched = true;
                rows.push(
                    vars.iter()
                        .map(|&v| match value(l, i, v) {
                            Id::UNDEF => value(r, j, v),
                            x => x,
                        })
                        .collect(),
                );
            }
        }
        if left && !matched {
            rows.push(vars.iter().map(|&v| value(l, i, v)).collect());
        }
    }
    rows.sort();
    rows
}

/// One part of the rounds of the flat hash join check. Every part draws the tables of all
/// 400 rounds from the same random stream, so the rounds are the same in any split, and
/// checks the rounds that `(round + round / 10) % parts` assigns to `part`. That spreads the
/// large tables of every tenth round evenly over the parts, so the four tests, which run in
/// parallel, take about as long as each other.
fn flat_hash_joins_part(part: usize, parts: usize) {
    let s = Store::in_memory(StoreOptions::default());
    let on = ctx_with(&s, Optimizations::ALL);
    let mut next = xorshift(0x2545_f491_4f6c_dd1d);
    for round in 0..400 {
        for (lv, rv) in shapes(&on) {
            let big = round % 10 == 0;
            let n = |next: &mut dyn FnMut() -> u64| (next() % if big { 2000 } else { 60 }) as usize;
            // large tables with few keys would join into products of millions of rows
            let domain = if big { 1000 } else { [3, 20, 1000][round % 3] };
            let undef = [0, 5][round / 3 % 2];
            let (ln, rn) = (n(&mut next), n(&mut next));
            let mut l = table(&mut next, &lv, ln, domain, undef, &[]);
            let mut r = table(&mut next, &rv, rn, domain, undef, &[]);
            if round % 4 == 1 {
                l.sort_by_vars(&lv[..1]);
            }
            if round % 4 == 2 {
                r.sort_by_vars(&rv[..1]);
            }
            if (round + round / 10) % parts != part {
                continue;
            }
            let mut vars = lv.clone();
            vars.extend(rv.iter().filter(|v| !lv.contains(v)));
            let want = nested_loop(&l, &r, &vars, false);
            for merge in [false, true] {
                let a = join_tables(&on, &l, &r, &[], merge).unwrap();
                assert_eq!(bag(&a, &vars), want, "round {round}: {lv:?} {rv:?}");
            }
            let a = left_join(&on, &l, &r, None, &mut None).unwrap();
            assert_eq!(
                bag(&a, &vars),
                nested_loop(&l, &r, &vars, true),
                "round {round}: left join {lv:?} {rv:?}"
            );
        }
    }
}

#[test]
fn flat_hash_joins_give_the_rows_of_a_nested_loop_join_part_1_of_4() {
    flat_hash_joins_part(0, 4);
}

#[test]
fn flat_hash_joins_give_the_rows_of_a_nested_loop_join_part_2_of_4() {
    flat_hash_joins_part(1, 4);
}

#[test]
fn flat_hash_joins_give_the_rows_of_a_nested_loop_join_part_3_of_4() {
    flat_hash_joins_part(2, 4);
}

#[test]
fn flat_hash_joins_give_the_rows_of_a_nested_loop_join_part_4_of_4() {
    flat_hash_joins_part(3, 4);
}

#[test]
fn merge_left_joins_keep_the_left_order_and_the_rows_of_hash_left_joins() {
    let s = Store::in_memory(StoreOptions::default());
    let on = ctx_with(&s, Optimizations::ALL);
    let off = ctx_with(
        &s,
        Optimizations {
            merge_left_join: false,
            ..Optimizations::ALL
        },
    );
    let [k, a, b, c, rid] = ["k", "a", "b", "c", "rid"].map(|n| on.var(n));
    let mut next = xorshift(0x9e37_79b9_7f4a_7c15);
    let mut merged = 0;
    for round in 0..600 {
        let big = round % 7 == 0;
        let domain = [2, 10, 100, 5000][round % 4].max(if big { 1000 } else { 0 });
        let ln = (next() % if big { 4000 } else { 50 }) as usize;
        // a right side much larger than the left is read by galloping
        let rn = (next() % if big || round % 5 == 0 { 9000 } else { 50 }) as usize;
        let undef = [0, 4][round % 2];
        let mut l = table(&mut next, &[k, a], ln, domain, undef, &[k]);
        // each left row's number, to check the order of the output
        l.vars.push(rid);
        l.cols.push((1..=ln as u64).map(Id).collect());
        let rvars: &[VarId] = if round % 3 == 0 { &[b, k] } else { &[k, b, c] };
        let mut r = table(&mut next, rvars, rn, domain, undef, &[k]);
        l.sort_by_vars(&[k]);
        r.sort_by_vars(&[k]);
        // renumber the rows in their sorted order
        l.cols[2] = (1..=ln as u64).map(Id).collect();
        // a left key left unbound now and then: no merge
        let unbound = round % 11 == 5 && ln > 0;
        if unbound {
            l.cols[0][0] = Id::UNDEF;
        }
        let mut note = None;
        let x = left_join(&on, &l, &r, None, &mut note).unwrap();
        let y = left_join(&off, &l, &r, None, &mut None).unwrap();
        let vars = y.vars.clone();
        assert_eq!(bag(&x, &vars), bag(&y, &vars), "round {round}");
        if unbound {
            assert!(
                !note.is_some_and(|n| n.starts_with("[merge")),
                "round {round}"
            );
            continue;
        }
        assert!(
            note.is_some_and(|n| n.starts_with("[merge")),
            "round {round}"
        );
        merged += 1;
        assert_eq!(x.sorted, l.sorted, "round {round}");
        let ids = &x.cols[x.col_of(rid).unwrap()];
        assert!(ids.windows(2).all(|w| w[0] <= w[1]), "round {round}");
        let mut seen: Vec<Id> = ids.clone();
        seen.dedup();
        assert_eq!(seen.len(), ln, "round {round}: every left row");
    }
    assert!(merged > 400);
}

/// A merge join on two shared variables tests the second one on each pair of the first
/// key's runs before the pair counts, so a constant first key with unique second keys
/// gives one row per left row within a row limit that the product of the runs exceeds.
#[test]
fn merge_joins_on_two_keys_count_only_compatible_pairs_against_the_row_limit() {
    let s = Store::in_memory(StoreOptions::default());
    let mut ctx = ctx_with(&s, Optimizations::ALL);
    ctx.max_rows = 4096;
    let [k, x] = ["k", "x"].map(|n| ctx.var(n));
    let rows = 1000usize;
    let side = |vars: Vec<VarId>| {
        let mut t = Table::new(vars);
        for i in 0..rows {
            t.push_row(&[Id(7), Id(100 + i as u64)]);
        }
        t.sorted = vec![k, x];
        t
    };
    let (l, r) = (side(vec![k, x]), side(vec![k, x]));
    let joined = super::exec::join_tables(&ctx, &l, &r, &[k], true).unwrap();
    assert_eq!(joined.len(), rows);
    assert!(
        joined.cols[1]
            .iter()
            .enumerate()
            .all(|(i, v)| *v == Id(100 + i as u64))
    );
    // A right run longer than a probe piece, of which each left row matches one row,
    // within a row limit below the run's length.
    let long = 40_000usize;
    ctx.max_rows = long - 5_000;
    let mut r = Table::new(vec![k, x]);
    for i in 0..long {
        r.push_row(&[Id(7), Id(100 + i as u64)]);
    }
    r.sorted = vec![k, x];
    let mut l = Table::new(vec![k, x]);
    for i in [0usize, long / 2, long - 1] {
        l.push_row(&[Id(7), Id(100 + i as u64)]);
    }
    l.sorted = vec![k, x];
    let joined = super::exec::join_tables(&ctx, &l, &r, &[k], true).unwrap();
    assert_eq!(joined.len(), 3);
    // The merge OPTIONAL shares the key alone, so every pair of the long run is a match
    // and the run is expanded a piece at a time within a limit above its product.
    let y = ctx.var("y");
    ctx.max_rows = 200_000;
    let mut r = Table::new(vec![k, y]);
    for i in 0..long {
        r.push_row(&[Id(7), Id(100 + i as u64)]);
    }
    r.sorted = vec![k, y];
    let mut note = None;
    let optional = left_join(&ctx, &l, &r, None, &mut note).unwrap();
    assert_eq!(optional.len(), 3 * long);
    assert!(
        note.as_deref().is_some_and(|n| n.starts_with("[merge on")),
        "{note:?}"
    );
}

/// The zippers of merge left joins and anti joins, alone, side by side over parts of the
/// inputs, or galloping, give every left id's lower bound in the right column. The inputs
/// include runs of duplicates, gaps that leave a part without rows on one side, and sizes
/// on both sides of the threshold for several zippers.
#[test]
fn merge_lower_bounds_match_a_binary_search() {
    let s = Store::in_memory(StoreOptions::default());
    let ctx = ctx_with(&s, Optimizations::ALL);
    let mut next = xorshift(0x2545_f491_4f6c_dd1d);
    for round in 0..400 {
        let an = (next() % [10, 300, 3000, 20_000, 60_000][round % 5]) as usize;
        let bn = (next() % [10, 300, 6000, 40_000][(round / 5) % 4]) as usize;
        let domain = [3, 50, 10_000, u64::MAX / 2][(round / 20) % 4];
        let column = |n: usize, next: &mut dyn FnMut() -> u64| {
            let mut c: Vec<Id> = (0..n).map(|_| Id(1 + next() % domain)).collect();
            c.sort();
            c
        };
        let a = column(an, &mut next);
        let mut b = column(bn, &mut next);
        // now and then a right side that is all below or all above a stretch of the left
        if round % 5 == 3 && !a.is_empty() {
            let mid = a[a.len() / 2];
            b.retain(|y| *y < mid || *y > Id(mid.0.saturating_add(domain / 4)));
        }
        let want: Vec<u32> = a
            .iter()
            .map(|x| b.partition_point(|y| y < x) as u32)
            .collect();
        let got = super::join::zipper::lower_bounds(&ctx, &a, &b).unwrap();
        assert_eq!(got, want, "round {round}: {an} x {bn} in 1..={domain}");
    }
}

fn load(s: &Store, text: &str) {
    s.load(&[Source::from_bytes(
        text.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
}

fn update(s: &Store, text: &str) {
    super::update::update(s, &format!("{PREFIXES}{text}"), &QueryOptions::default()).unwrap();
}

fn run(snap: &Arc<crate::store::Snapshot>, q: &str, opt: Optimizations) -> QueryResult {
    let opts = QueryOptions {
        optimizations: Some(opt),
        no_cache: true,
        ..Default::default()
    };
    query(snap.clone(), &format!("{PREFIXES}{q}"), &opts).unwrap_or_else(|e| panic!("{q}: {e}"))
}

fn answer(r: &QueryResult) -> Vec<String> {
    let mut v: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| t.map_or("UNDEF".to_string(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    v.sort();
    v
}

fn has_note(p: &PlanInfo, op: &str, note: &str) -> bool {
    (p.operator == op && p.description.contains(note))
        || p.children.iter().any(|c| has_note(c, op, note))
}

const OPTIONALS: &[&str] = &[
    "SELECT * WHERE { ?s a ex:C0 OPTIONAL { ?s ex:p0 ?a } }",
    "SELECT * WHERE { ?s a ex:C0 OPTIONAL { ?s ex:p0 ?a FILTER(?a != 3 && ?a != ex:s2) } }",
    "SELECT * WHERE { ?s ex:p0 ?x OPTIONAL { ?s ex:p1 ?y } }",
    "SELECT * WHERE { ?s ex:p0 ?x OPTIONAL { ?s ex:p1 ?y FILTER(?y != ?x) } }",
    "SELECT * WHERE { ?s a ex:C1 OPTIONAL { ?s ex:p4 ?t . ?t ex:p0 ?a OPTIONAL { ?t ex:p1 ?b } } }",
    "SELECT * WHERE { ?s ex:p4 ?t OPTIONAL { ?t ex:p0 ?a } }",
    "SELECT * WHERE { ?s a ex:C2 OPTIONAL { ?s ex:p0 ?a } ?s ex:p1 ?b }",
    "SELECT * WHERE { VALUES ?s { ex:s1 ex:s3 UNDEF ex:s7 } OPTIONAL { ?s ex:p0 ?a } }",
    "SELECT * WHERE { ?s a ex:C0 OPTIONAL { { ?s ex:p0 ?a } UNION { ?s ex:p1 ?b } } }",
    "SELECT * WHERE { ?s a ex:C3 OPTIONAL { ?s ex:p2 ?a } OPTIONAL { ?s ex:p3 ?b } }",
    "SELECT (COUNT(*) AS ?n) WHERE { ?s a ex:C0 OPTIONAL { ?s ex:p1 ?o } }",
    "SELECT * WHERE { GRAPH ?g { ?s a ex:C0 OPTIONAL { ?s ex:p0 ?a } } }",
    "SELECT * WHERE { ?s a ex:C0 . ?t a ex:C1 . ?s ex:p4 ?t . ?t ex:p0 ?a . ?s ex:p1 ?b }",
    "SELECT * WHERE { ?s ex:p0 ?a . ?s ex:p1 ?b . ?a ex:p2 ?c }",
];

#[test]
fn optional_and_join_queries_give_the_same_rows_with_each_switch() {
    let mut merges = 0;
    for (round, union) in [(0u64, false), (1, true), (2, false)] {
        let s = Store::in_memory(StoreOptions {
            union_default_graph: union,
            ..Default::default()
        });
        load(
            &s,
            &random_trig(0x51_7cc1 ^ round, 80 + 90 * round as usize),
        );
        for pass in 0..2 {
            if pass == 1 {
                update(
                    &s,
                    "INSERT DATA { ex:n1 a ex:C0 ; ex:p0 3 , ex:s1 ; ex:p1 4 } ; \
                     DELETE WHERE { ?s ex:p1 ex:o3 }",
                );
            }
            let snap = s.snapshot();
            for q in OPTIONALS {
                let want = answer(&run(&snap, q, Optimizations::NONE));
                for (how, opt) in [
                    ("all", Optimizations::ALL),
                    (
                        "no merge left join",
                        Optimizations {
                            merge_left_join: false,
                            ..Optimizations::ALL
                        },
                    ),
                ] {
                    let r = run(&snap, q, opt);
                    assert_eq!(answer(&r), want, "round {round} pass {pass}, {how}: {q}");
                    if how == "all" && has_note(&r.plan, "OptionalJoin", "[merge on") {
                        merges += 1;
                    }
                }
            }
        }
    }
    assert!(merges >= 6, "{merges}");
}

/// Subjects in several blocks of every permutation, some with runs of values that span
/// blocks, probed for random, clustered and absent keys: the galloping reader gives the
/// rows of the reader with a scan per cluster, before and after updates. Every part draws
/// the keys of every round from the same random stream, so the rounds are the same in
/// any split, and checks the queries whose position leaves `part` modulo `parts`. The six
/// tests, one per query, run in parallel.
fn galloping_index_joins_part(part: usize, parts: usize) {
    let s = Store::in_memory(StoreOptions::default());
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..30_000 {
        t.push_str(&format!(
            "ex:s{i:05} a ex:C{} ; ex:p0 {} .\n",
            i % 3,
            i % 97
        ));
        if i % 2 == 0 {
            t.push_str(&format!("ex:s{i:05} ex:p1 ex:s{:05} .\n", (i * 7) % 30_000));
        }
        // a few subjects with runs longer than a block
        if i % 9_973 == 0 && i < 20_000 {
            for j in 0..34_000 {
                t.push_str(&format!("ex:s{i:05} ex:p2 {j} .\n"));
            }
        }
    }
    load(&s, &t);
    assert!(s.snapshot().perm(crate::index::Perm::Pso).blocks.len() >= 4);
    let mut next = xorshift(0x1234_5678_9abc_def1);
    let gallop_off = Optimizations {
        gallop_index_join: false,
        ..Optimizations::ALL
    };
    for pass in 0..2 {
        if pass == 1 {
            update(
                &s,
                "INSERT DATA { ex:s00010 ex:p0 1000 . ex:s19946 ex:p2 -1 . ex:new ex:p0 1 } ; \
                 DELETE DATA { ex:s00500 ex:p0 15 . ex:s09973 ex:p2 5 }",
            );
        }
        let snap = s.snapshot();
        for round in 0..4 {
            let k = [1, 5, 40, 300][round % 4];
            let keys: Vec<String> = (0..k)
                .map(|_| match next() % 10 {
                    0 => "ex:absent".to_string(),
                    1 => format!("ex:s{:05}", 9_973 * (next() % 3)),
                    2 => "ex:s00010 ex:s00011 ex:s00500 ex:new".to_string(),
                    _ => format!("ex:s{:05}", next() % 31_000),
                })
                .collect();
            let values = format!("VALUES ?s {{ {} }}", keys.join(" "));
            for (i, q) in [
                format!("SELECT * WHERE {{ {values} ?s ex:p0 ?a }}"),
                format!("SELECT * WHERE {{ {values} ?s ex:p2 ?a }}"),
                format!("SELECT * WHERE {{ {values} ?s ex:p0 ?a ; ex:p1 ?b }}"),
                format!("SELECT * WHERE {{ {values} ?s ex:p0 ?a ; ex:p2 ?c ; a ?t }}"),
                format!("SELECT * WHERE {{ {values} ?s ex:p1 ?t . ?t ex:p0 ?a }}"),
                format!("SELECT * WHERE {{ {values} ?s a ex:C1 ; ex:p0 ?a }}"),
            ]
            .into_iter()
            .enumerate()
            {
                if i % parts != part {
                    continue;
                }
                let want = answer(&run(&snap, &q, Optimizations::NONE));
                for walk in [Some(true), Some(false)] {
                    for opt in [Optimizations::ALL, gallop_off] {
                        super::indexjoin::FORCE_INDEX_JOIN.with(|f| f.set(true));
                        super::indexjoin::FORCE_WALK.with(|f| f.set(walk));
                        let r = run(&snap, &q, opt);
                        super::indexjoin::FORCE_INDEX_JOIN.with(|f| f.set(false));
                        super::indexjoin::FORCE_WALK.with(|f| f.set(None));
                        assert_eq!(
                            answer(&r),
                            want,
                            "pass {pass}, {walk:?}, gallop {}: {q}",
                            opt.gallop_index_join
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn galloping_index_joins_match_scans_per_cluster_across_blocks_part_1_of_6() {
    galloping_index_joins_part(0, 6);
}

#[test]
fn galloping_index_joins_match_scans_per_cluster_across_blocks_part_2_of_6() {
    galloping_index_joins_part(1, 6);
}

#[test]
fn galloping_index_joins_match_scans_per_cluster_across_blocks_part_3_of_6() {
    galloping_index_joins_part(2, 6);
}

#[test]
fn galloping_index_joins_match_scans_per_cluster_across_blocks_part_4_of_6() {
    galloping_index_joins_part(3, 6);
}

#[test]
fn galloping_index_joins_match_scans_per_cluster_across_blocks_part_5_of_6() {
    galloping_index_joins_part(4, 6);
}

#[test]
fn galloping_index_joins_match_scans_per_cluster_across_blocks_part_6_of_6() {
    galloping_index_joins_part(5, 6);
}

/// An algorithm join admits the bytes of the table it is about to build and refuses a
/// table whose capacity differs. A join of one to a few rows must therefore gather its
/// columns at exactly that length, since a column grown from empty takes the allocator's
/// minimum capacity of four instead.
#[test]
fn an_algorithm_join_of_a_few_rows_fits_its_admitted_capacity() {
    let store = Store::in_memory(StoreOptions::default());
    let ctx = Ctx::new(store.snapshot());
    let [k, a, b] = ["k", "a", "b"].map(|n| ctx.var(n));
    for n in 1..=6u64 {
        let mut l = Table::new(vec![a, k]);
        let mut r = Table::new(vec![k, b]);
        for i in 1..=n {
            l.push_row(&[Id(10 + i), Id(i)]);
            r.push_row(&[Id(i), Id(20 + i)]);
        }
        let out = super::exec::join_algorithm_tables(&ctx, &l, &r)
            .unwrap_or_else(|e| panic!("{n} rows: {e}"));
        assert_eq!(out.table.len(), n as usize);
        for col in &out.table.cols {
            assert_eq!(col.capacity(), n as usize, "{n} rows");
        }
    }
}

/// A join's condition is tested from the ids of its candidate rows, in the merge and
/// the hash form of an OPTIONAL. The FILTER path decodes the terms of a large table
/// first, and doing so for a condition cost an OPTIONAL over 300,000 candidate rows four
/// times its run, so the conditional OPTIONAL over more rows than that path decodes
/// holds no more memory than the OPTIONAL without the condition.
#[test]
fn a_join_condition_decodes_no_terms() {
    const ROWS: usize = 20_000;
    let s = Store::in_memory(StoreOptions::default());
    let mut text = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..ROWS {
        text.push_str(&format!("ex:s{i} a ex:C0 ; ex:p0 ex:o{i} .\n"));
    }
    load(&s, &text);
    let snap = s.snapshot();
    let plain = "SELECT * WHERE { ?s a ex:C0 OPTIONAL { ?s ex:p0 ?o } }";
    let conditional = "SELECT * WHERE { ?s a ex:C0 OPTIONAL { ?s ex:p0 ?o FILTER(?o != ex:x) } }";
    for (how, opt) in [
        ("merge", Optimizations::ALL),
        (
            "hash",
            Optimizations {
                merge_left_join: false,
                ..Optimizations::ALL
            },
        ),
    ] {
        let without = run(&snap, plain, opt);
        let with = run(&snap, conditional, opt);
        assert_eq!(with.rows().len(), ROWS, "{how}");
        assert!(
            has_note(
                &with.plan,
                "OptionalJoin",
                if how == "merge" {
                    "[merge on"
                } else {
                    "[flat hash table"
                }
            ),
            "{how}: {}",
            with.plan.description
        );
        // The condition's mask and its merged rows of two columns at most, and the hash
        // kernel's scratch for a chunk of candidate pairs. Decoding the 20,000 terms of
        // ?o takes about 2 MiB more.
        let chunk = if how == "hash" {
            super::exec::KEYED_CHUNK as u64 * (2 * 8 + 24) + 256
        } else {
            0
        };
        let slack = ROWS as u64 * (1 + 2 * 8) + chunk + 1024;
        assert!(
            with.mem_peak_bytes <= without.mem_peak_bytes + slack,
            "{how}: the condition took the peak from {} to {} bytes",
            without.mem_peak_bytes,
            with.mem_peak_bytes
        );
    }
}
