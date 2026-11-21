//! Ordered windows (spec F13 §4.4, item 419): the maintained window against the model of
//! the data, its positions against a client that applies the moves, and its refills.

use super::circuit::{Advance, Circuit, DeltaSet};
use super::compile::compile;
use super::ops::Op;
use crate::sparql::{self, QueryOptions};
use crate::store::{Store, StoreOptions};
use crate::task::Control;
use oxrdf::Term;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use std::collections::BTreeMap;

const PREFIX: &str = "PREFIX : <http://ex/> ";
const INT: &str = "http://www.w3.org/2001/XMLSchema#integer";

fn canon(t: &Option<Term>) -> String {
    match t {
        None => "UNDEF".into(),
        Some(Term::NamedNode(n)) => n.as_str().trim_start_matches("http://ex/").to_string(),
        Some(Term::Literal(l)) => l.value().to_string(),
        Some(t) => t.to_string(),
    }
}

fn upd(s: &Store, u: &str) {
    crate::sparql::update::update(s, &format!("{PREFIX}{u}"), &QueryOptions::default())
        .expect("update");
}

fn circuit(store: &Store, q: &str) -> Circuit {
    let parsed = sparql::parse_query(&format!("{PREFIX}{q}"), None, &[]).expect("parse");
    let mut c = Circuit::new(compile(&parsed, None).expect("compile"), None);
    c.build(store.snapshot(), &Control::none()).expect("build");
    c
}

fn step(store: &Store, c: &mut Circuit) {
    let d = DeltaSet::read(store, c.applied, &Control::none());
    let a = c.advance(&d, &Control::none()).expect("advance");
    assert!(!matches!(a, Advance::Reset(_)), "a pass reset");
}

/// The window's rows in order, one string per copy.
fn window(c: &Circuit) -> Vec<String> {
    let rows = c.ordered_rows().expect("an ordered window");
    let mut out = Vec::new();
    for (r, n) in rows {
        assert!(n > 0, "a window row without copies");
        let s = r.iter().map(canon).collect::<Vec<_>>().join(" ");
        for _ in 0..n {
            out.push(s.clone());
        }
    }
    // the result's multiset is the window's
    let mut a = out.clone();
    a.sort();
    let mut b = Vec::new();
    for (r, w) in c.result.iter() {
        let s = c.decode_rows(&[(r.clone(), w)])[0]
            .0
            .iter()
            .map(canon)
            .collect::<Vec<_>>()
            .join(" ");
        for _ in 0..w {
            b.push(s.clone());
        }
    }
    b.sort();
    assert_eq!(a, b, "the result is the window's rows");
    out
}

/// A client's copy of the window, updated by the moves of each pass.
fn apply_moves(client: &mut Vec<String>, c: &Circuit) {
    let Some((removed, added)) = c.moves() else {
        return;
    };
    let text = |t: &[Option<Term>]| t.iter().map(canon).collect::<Vec<_>>().join(" ");
    let mut gone: Vec<(u64, String)> = Vec::new();
    for (p, t, n) in &removed {
        for i in 0..*n {
            gone.push((p + i, text(t)));
        }
    }
    gone.sort_by_key(|g| std::cmp::Reverse(g.0));
    for (p, t) in gone {
        assert_eq!(client[p as usize], t, "a removed row at its old position");
        client.remove(p as usize);
    }
    let mut came: Vec<(u64, String)> = Vec::new();
    for (p, t, n) in &added {
        for i in 0..*n {
            came.push((p + i, text(t)));
        }
    }
    came.sort();
    for (p, t) in came {
        client.insert(p as usize, t);
    }
}

/// Check a window against the whole input in order, `full`, as (key, row) pairs: the
/// keys position by position, and the rows of each key, exactly for keys inside the
/// window and as a subset of the key's rows at its edges, where SPARQL leaves the order
/// of ties open.
fn check_window(
    got: &[String],
    full: &[(String, String)],
    offset: usize,
    limit: usize,
    what: &str,
) {
    let want: Vec<&(String, String)> = full.iter().skip(offset).take(limit).collect();
    assert_eq!(
        got.len(),
        want.len(),
        "{what}: the window's length\n{got:?}\n{want:?}"
    );
    let key_of: BTreeMap<&str, &str> = full.iter().map(|(k, r)| (r.as_str(), k.as_str())).collect();
    let gk: Vec<&str> = got
        .iter()
        .map(|r| {
            *key_of
                .get(r.as_str())
                .unwrap_or_else(|| panic!("{what}: {r} is not in the input"))
        })
        .collect();
    let wk: Vec<&str> = want.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(gk, wk, "{what}: the keys by position\n{got:?}");
    let (first, last) = (wk.first().copied(), wk.last().copied());
    let rows = |v: &mut dyn Iterator<Item = String>| {
        let mut x: Vec<String> = v.collect();
        x.sort();
        x
    };
    for k in wk
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
    {
        let g = rows(&mut got.iter().filter(|r| key_of[r.as_str()] == k).cloned());
        let all = rows(
            &mut full
                .iter()
                .filter(|(fk, _)| fk == k)
                .map(|(_, r)| r.clone()),
        );
        if Some(k) != first && Some(k) != last {
            assert_eq!(g, all, "{what}: the rows of key {k}");
        } else {
            let mut pool = all.clone();
            for r in &g {
                let i = pool
                    .iter()
                    .position(|x| x == r)
                    .unwrap_or_else(|| panic!("{what}: {r} more often than the input holds it"));
                pool.remove(i);
            }
        }
    }
}

/// The whole ordered result of a query from the model, as (key, row) pairs.
type Full = fn(&BTreeMap<u32, i64>) -> Vec<(String, String)>;

/// A query, its slice, and its whole ordered result from the model of subject values.
struct Case {
    q: &'static str,
    offset: usize,
    limit: usize,
    full: Full,
}

fn key(v: i64) -> String {
    format!("{v:+012}")
}

fn by_value_asc(m: &BTreeMap<u32, i64>) -> Vec<(String, String)> {
    let mut v: Vec<(i64, u32)> = m.iter().map(|(s, v)| (*v, *s)).collect();
    v.sort();
    v.into_iter()
        .map(|(v, s)| (key(v), format!("s{s} {v}")))
        .collect()
}

fn by_value_desc_subject(m: &BTreeMap<u32, i64>) -> Vec<(String, String)> {
    let mut v: Vec<(i64, String)> = m.iter().map(|(s, v)| (*v, format!("s{s}"))).collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    // the subject breaks the value's ties, so every row has its own key
    v.into_iter()
        .map(|(v, s)| (format!("{} {s}", key(-v)), format!("{s} {v}")))
        .collect()
}

fn subjects_by_value(m: &BTreeMap<u32, i64>) -> Vec<(String, String)> {
    let mut v: Vec<(i64, u32)> = m.iter().map(|(s, v)| (*v, *s)).collect();
    v.sort();
    v.into_iter()
        .map(|(v, s)| (key(v), format!("s{s}")))
        .collect()
}

fn values(m: &BTreeMap<u32, i64>) -> Vec<(String, String)> {
    let mut v: Vec<i64> = m.values().copied().collect();
    v.sort();
    v.into_iter().map(|v| (key(v), v.to_string())).collect()
}

fn distinct_values_desc(m: &BTreeMap<u32, i64>) -> Vec<(String, String)> {
    let mut v: Vec<i64> = m.values().copied().collect();
    v.sort();
    v.dedup();
    v.reverse();
    v.into_iter().map(|v| (key(-v), v.to_string())).collect()
}

fn counts_desc(m: &BTreeMap<u32, i64>) -> Vec<(String, String)> {
    let mut n: BTreeMap<i64, i64> = BTreeMap::new();
    for v in m.values() {
        *n.entry(*v).or_default() += 1;
    }
    let mut v: Vec<(i64, i64)> = n.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.into_iter()
        .map(|(v, n)| (format!("{} {}", key(-n), key(v)), format!("{v} {n}")))
        .collect()
}

fn negated(m: &BTreeMap<u32, i64>) -> Vec<(String, String)> {
    let mut v: Vec<(i64, String)> = m.iter().map(|(s, v)| (-*v, format!("s{s}"))).collect();
    v.sort();
    v.into_iter()
        .map(|(k, s)| (format!("{} {s}", key(k)), format!("{s} {}", -k)))
        .collect()
}

const CASES: &[Case] = &[
    Case {
        q: "SELECT ?s ?v WHERE { ?s :v ?v } ORDER BY ?v LIMIT 5",
        offset: 0,
        limit: 5,
        full: by_value_asc,
    },
    Case {
        q: "SELECT ?s ?v WHERE { ?s :v ?v } ORDER BY DESC(?v) ?s LIMIT 3 OFFSET 4",
        offset: 4,
        limit: 3,
        full: by_value_desc_subject,
    },
    Case {
        q: "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 4 OFFSET 2",
        offset: 2,
        limit: 4,
        full: subjects_by_value,
    },
    Case {
        q: "SELECT ?v WHERE { ?s :v ?v } ORDER BY ?v LIMIT 6",
        offset: 0,
        limit: 6,
        full: values,
    },
    Case {
        q: "SELECT DISTINCT ?v WHERE { ?s :v ?v } ORDER BY DESC(?v) LIMIT 3 OFFSET 1",
        offset: 1,
        limit: 3,
        full: distinct_values_desc,
    },
    Case {
        q: "SELECT ?v (COUNT(?s) AS ?n) WHERE { ?s :v ?v } GROUP BY ?v ORDER BY DESC(?n) ?v LIMIT 3",
        offset: 0,
        limit: 3,
        full: counts_desc,
    },
    Case {
        q: "SELECT ?s ?v WHERE { ?s :v ?v } ORDER BY (0 - ?v) ?s LIMIT 2 OFFSET 1",
        offset: 1,
        limit: 2,
        full: negated,
    },
    Case {
        q: "SELECT ?s ?v WHERE { ?s :v ?v } ORDER BY ?v LIMIT 0",
        offset: 0,
        limit: 0,
        full: by_value_asc,
    },
];

fn triple(s: u32, v: i64) -> String {
    format!(":s{s} :v \"{v}\"^^<{INT}>")
}

/// Random histories of single-valued subjects: each commit adds, removes or changes a
/// few, and some remove many at once, which runs the reserves out.
fn differential(seed: u64, subjects: u32, range: i64, steps: usize, remaps: bool) {
    let mut rng = StdRng::seed_from_u64(seed);
    let s = Store::in_memory(StoreOptions::default());
    let mut model: BTreeMap<u32, i64> = BTreeMap::new();
    let mut init = String::new();
    for i in 0..subjects {
        if rng.random_bool(0.7) {
            let v = rng.random_range(0..range);
            model.insert(i, v);
            init.push_str(&triple(i, v));
            init.push_str(" . ");
        }
    }
    upd(&s, &format!("INSERT DATA {{ {init} }}"));
    let mut cs: Vec<Circuit> = CASES.iter().map(|c| circuit(&s, c.q)).collect();
    let mut clients: Vec<Vec<String>> = cs.iter().map(window).collect();
    for (k, c) in CASES.iter().enumerate() {
        check_window(&clients[k], &(c.full)(&model), c.offset, c.limit, c.q);
    }
    for n in 0..steps {
        let (mut del, mut ins) = (String::new(), String::new());
        let many = rng.random_bool(0.05);
        let changes = if many {
            subjects as usize / 2
        } else {
            rng.random_range(1..4)
        };
        let mut touched = std::collections::BTreeSet::new();
        for _ in 0..changes {
            let i = rng.random_range(0..subjects);
            // one change per subject and commit, so that the model follows the update
            if !touched.insert(i) {
                continue;
            }
            if let Some(v) = model.remove(&i) {
                del.push_str(&triple(i, v));
                del.push_str(" . ");
                if !many && rng.random_bool(0.5) {
                    let v = rng.random_range(0..range);
                    model.insert(i, v);
                    ins.push_str(&triple(i, v));
                    ins.push_str(" . ");
                }
            } else if !many {
                let v = rng.random_range(0..range);
                model.insert(i, v);
                ins.push_str(&triple(i, v));
                ins.push_str(" . ");
            }
        }
        upd(
            &s,
            &format!("DELETE DATA {{ {del} }} ; INSERT DATA {{ {ins} }}"),
        );
        let remap = remaps && n % 17 == 16;
        for (k, c) in CASES.iter().enumerate() {
            let circ = &mut cs[k];
            if remap {
                // a checkpoint's encoding round trip keeps the rows and their order
                let snap = circ.snap.clone().expect("built");
                let before = window(circ);
                let body = circ.encode(&snap).expect("encode");
                circ.decode(&body, &snap).expect("decode");
                assert_eq!(window(circ), before, "{}: the window after a load", c.q);
            }
            step(&s, circ);
            apply_moves(&mut clients[k], circ);
            let got = window(circ);
            assert_eq!(clients[k], got, "{} step {n}: the client's copy", c.q);
            check_window(
                &got,
                &(c.full)(&model),
                c.offset,
                c.limit,
                &format!("{} step {n} seed {seed}", c.q),
            );
        }
    }
}

#[test]
fn windows_follow_the_model_through_random_histories() {
    for seed in 0..6 {
        differential(seed, 40, 12, 120, true);
    }
}

#[test]
fn windows_follow_the_model_without_reloads() {
    for seed in 0..6 {
        differential(seed, 40, 12, 120, false);
    }
}

#[test]
fn windows_refill_their_reserves_on_large_inputs() {
    for seed in 100..102 {
        differential(seed, 400, 300, 80, true);
    }
}

fn window_op(c: &Circuit) -> &super::ops::window::Window {
    c.root.result_window().expect("a window").0
}

/// Inserts and deletes above, inside and below the window, with an offset.
#[test]
fn changes_around_the_window_move_its_rows() {
    let s = Store::in_memory(StoreOptions::default());
    let data: Vec<String> = (1..=10).map(|i| triple(i, i as i64)).collect();
    upd(&s, &format!("INSERT DATA {{ {} }}", data.join(" . ")));
    let q = "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 3 OFFSET 2";
    let mut c = circuit(&s, q);
    assert_eq!(window(&c), ["s3", "s4", "s5"]);
    let moves = |c: &Circuit| {
        let (r, a) = c.moves().expect("the pass reached the window");
        let f = |v: Vec<(u64, Vec<Option<Term>>, u64)>| {
            v.into_iter()
                .map(|(p, t, n)| format!("{p}:{}x{n}", canon(&t[0])))
                .collect::<Vec<_>>()
        };
        (f(r), f(a))
    };
    // before the offset: every row moves down one place
    upd(&s, &format!("INSERT DATA {{ {} }}", triple(0, 0)));
    step(&s, &mut c);
    assert_eq!(window(&c), ["s2", "s3", "s4"]);
    assert_eq!(moves(&c), (vec!["2:s5x1".into()], vec!["0:s2x1".into()]));
    // a delete before the offset moves them back up
    upd(&s, &format!("DELETE DATA {{ {} }}", triple(0, 0)));
    step(&s, &mut c);
    assert_eq!(window(&c), ["s3", "s4", "s5"]);
    assert_eq!(moves(&c), (vec!["0:s2x1".into()], vec!["2:s5x1".into()]));
    // after the window: nothing to send
    upd(&s, &format!("INSERT DATA {{ {} }}", triple(20, 100)));
    step(&s, &mut c);
    assert_eq!(moves(&c), (vec![], vec![]));
    // inside the window
    upd(&s, &format!("DELETE DATA {{ {} }}", triple(4, 4)));
    step(&s, &mut c);
    assert_eq!(window(&c), ["s3", "s5", "s6"]);
    assert_eq!(moves(&c), (vec!["1:s4x1".into()], vec!["2:s6x1".into()]));
    // a row that moves inside the window keeps its terms and changes its position
    upd(
        &s,
        &format!(
            "DELETE DATA {{ {} }} ; INSERT DATA {{ {} }}",
            triple(6, 6),
            triple(6, 4)
        ),
    );
    step(&s, &mut c);
    assert_eq!(window(&c), ["s3", "s6", "s5"]);
    assert_eq!(moves(&c), (vec!["2:s6x1".into()], vec!["1:s6x1".into()]));
    assert!(c.result.iter().count() == 3);
}

/// Duplicates count once per copy, and a window may cut through a row's copies.
#[test]
fn duplicates_take_one_position_each() {
    let s = Store::in_memory(StoreOptions::default());
    let data: Vec<String> = (1..=6)
        .map(|i| triple(i, if i <= 3 { 1 } else { 2 }))
        .collect();
    upd(&s, &format!("INSERT DATA {{ {} }}", data.join(" . ")));
    let mut c = circuit(
        &s,
        "SELECT ?v WHERE { ?s :v ?v } ORDER BY ?v LIMIT 4 OFFSET 1",
    );
    assert_eq!(window(&c), ["1", "1", "2", "2"]);
    let mut client = window(&c);
    upd(&s, &format!("DELETE DATA {{ {} }}", triple(1, 1)));
    step(&s, &mut c);
    apply_moves(&mut client, &c);
    assert_eq!(window(&c), ["1", "2", "2", "2"]);
    assert_eq!(client, window(&c));
}

/// A run of deletions at the front empties the reserve once per `max(k, 100)` rows.
#[test]
fn the_reserve_refills_once_per_hundred_deletions() {
    let s = Store::in_memory(StoreOptions::default());
    let data: Vec<String> = (0..300).map(|i| triple(i, i as i64)).collect();
    upd(&s, &format!("INSERT DATA {{ {} }}", data.join(" . ")));
    let q = "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 2";
    let mut c = circuit(&s, q);
    for i in 0..250u32 {
        upd(&s, &format!("DELETE DATA {{ {} }}", triple(i, i as i64)));
        step(&s, &mut c);
        assert_eq!(window(&c), [format!("s{}", i + 1), format!("s{}", i + 2)]);
    }
    // the operator keeps 2 + 100 rows, so 250 deletions empty it twice
    assert_eq!(window_op(&c).refills, 2);
}

/// The compiler's choices: windows where the order is maintained, recompute nodes
/// elsewhere, and no positions for a window in a subquery.
#[test]
fn windows_and_their_fallbacks_compile() {
    let parse = |q: &str| sparql::parse_query(&format!("{PREFIX}{q}"), None, &[]).unwrap();
    let c = |q: &str| compile(&parse(q), None).unwrap();
    for q in [
        "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 3",
        "SELECT ?s WHERE { ?s :v ?v } ORDER BY (?v * 2) LIMIT 3 OFFSET 2",
        "SELECT DISTINCT ?v WHERE { ?s :v ?v } ORDER BY ?v LIMIT 3",
        "SELECT ?s WHERE { GRAPH :g { ?s :v ?v } } ORDER BY ?v LIMIT 3",
    ] {
        let k = c(q);
        assert!(!k.root.has_recompute(), "{q}: {:#?}", k.root.plan(&k.vars));
        assert!(
            k.root.result_window().is_some_and(|(w, _)| w.positions),
            "{q}"
        );
    }
    for q in [
        // the order of DISTINCT rows by a variable they do not hold
        "SELECT DISTINCT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 3",
        "SELECT ?s WHERE { ?s :v ?v } LIMIT 3",
        "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v OFFSET 3",
        "SELECT ?s WHERE { ?s :v ?v } ORDER BY DESC(EXISTS { ?s :q ?y }) LIMIT 1",
        "SELECT ?g ?s WHERE { GRAPH ?g { { SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 3 } } }",
    ] {
        assert!(c(q).root.has_recompute(), "{q}");
    }
    let k =
        c("SELECT ?s ?x WHERE { { SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 3 } ?s :w ?x }");
    assert!(!k.root.has_recompute());
    assert!(k.root.result_window().is_none());
    fn has_window(n: &super::ops::Node) -> bool {
        matches!(n.op, Op::Window(_)) || n.children().iter().any(|c| has_window(c))
    }
    assert!(has_window(&k.root));
}

/// A window in a subquery strips its key columns and follows the engine.
#[test]
fn a_window_in_a_subquery_follows_the_engine() {
    let s = Store::in_memory(StoreOptions::default());
    let data: Vec<String> = (1..=8)
        .map(|i| format!("{} . :s{i} :w :x{}", triple(i, i as i64 * 3 % 7), i % 3))
        .collect();
    upd(&s, &format!("INSERT DATA {{ {} }}", data.join(" . ")));
    let q = "SELECT ?s ?x WHERE { { SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v ?s LIMIT 3 OFFSET 1 } ?s :w ?x }";
    let mut c = circuit(&s, q);
    let oracle = |s: &Store| {
        let r = sparql::query(
            s.snapshot(),
            &format!("{PREFIX}{q}"),
            &QueryOptions {
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        let mut v: Vec<String> = r
            .test_application_rows()
            .iter()
            .map(|r| r.iter().map(canon).collect::<Vec<_>>().join(" "))
            .collect();
        v.sort();
        v
    };
    let got = |c: &Circuit| {
        let mut v: Vec<String> = c
            .rows()
            .into_iter()
            .flat_map(|(r, w)| {
                std::iter::repeat_n(
                    r.iter().map(canon).collect::<Vec<_>>().join(" "),
                    w as usize,
                )
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(got(&c), oracle(&s));
    for u in [
        format!("DELETE DATA {{ {} }}", triple(1, 3)),
        format!("INSERT DATA {{ {} }}", triple(9, 0)),
        "INSERT DATA { :s9 :w :x1 }".to_string(),
    ] {
        upd(&s, &u);
        step(&s, &mut c);
        assert!(c.moves().is_none());
        assert_eq!(got(&c), oracle(&s), "{u}");
    }
}

/// Review of item 419: a refill that finds more copies of a kept row adds them to its
/// entry, so later deletions remove every copy.
#[test]
fn a_refill_adds_copies_to_the_kept_entry() {
    let s = Store::in_memory(StoreOptions::default());
    let data: Vec<String> = (0..200).map(|i| triple(i, 1)).collect();
    upd(&s, &format!("INSERT DATA {{ {} }}", data.join(" . ")));
    let mut c = circuit(&s, "SELECT ?v WHERE { ?s :v ?v } ORDER BY ?v LIMIT 10");
    let del = |from: u32, to: u32| {
        let d: Vec<String> = (from..to).map(|i| triple(i, 1)).collect();
        format!("DELETE DATA {{ {} }}", d.join(" . "))
    };
    upd(&s, &del(0, 105));
    step(&s, &mut c);
    assert_eq!(window(&c).len(), 10);
    assert_eq!(window_op(&c).refills, 1);
    upd(&s, &del(105, 195));
    step(&s, &mut c);
    assert_eq!(window(&c), ["1"; 5]);
    upd(&s, &del(195, 200));
    step(&s, &mut c);
    assert!(window(&c).is_empty());
}

/// Review of item 419: a refill evaluates its input again, whose `SAMPLE` may then take
/// another value than the maintained one, and the window keeps only rows the input holds.
#[test]
fn a_refill_follows_rows_its_input_changed() {
    let s = Store::in_memory(StoreOptions::default());
    let data: Vec<String> = (0..200)
        .map(|i| format!(":g{i:03} :w \"10\"^^<{INT}>"))
        .collect();
    upd(&s, &format!("INSERT DATA {{ {} }}", data.join(" . ")));
    let q = "SELECT ?g (SAMPLE(?x) AS ?s) WHERE { ?g :w ?x } GROUP BY ?g ORDER BY ?g LIMIT 2";
    let mut c = circuit(&s, q);
    assert_eq!(window(&c), ["g000 10", "g001 10"]);
    upd(&s, &format!("INSERT DATA {{ :g000 :w \"1\"^^<{INT}> }}"));
    step(&s, &mut c);
    let d: Vec<String> = (1..=101)
        .map(|i| format!(":g{i:03} :w \"10\"^^<{INT}>"))
        .collect();
    upd(&s, &format!("DELETE DATA {{ {} }}", d.join(" . ")));
    step(&s, &mut c);
    assert_eq!(window_op(&c).refills, 1);
    let w = window(&c);
    assert_eq!(w.len(), 2);
    assert!(w[0].starts_with("g000 ") && w[1] == "g102 10", "{w:?}");
}

/// Review of item 419: a large insert into a window that held its whole input keeps the
/// window's vector, and its charge, near the cap.
#[test]
fn a_large_insert_keeps_the_window_bounded() {
    let s = Store::in_memory(StoreOptions::default());
    let mut c = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 1");
    let data: Vec<String> = (0..5000).map(|i| triple(i, i as i64)).collect();
    upd(&s, &format!("INSERT DATA {{ {} }}", data.join(" . ")));
    step(&s, &mut c);
    assert_eq!(window(&c), ["s0"]);
    let w = window_op(&c);
    assert!(w.slots() <= 2 * 101 + 64, "{} slots", w.slots());
    assert!(c.root.bytes() < 200_000, "{} bytes", c.root.bytes());
}

/// Review of item 419: the hidden key of an order expression cannot take the name of a
/// query's variable.
#[test]
fn a_hidden_key_never_meets_a_query_variable() {
    let s = Store::in_memory(StoreOptions::default());
    let q = "SELECT ?__incr1 WHERE { VALUES ?__incr1 { 1 2 } } ORDER BY (0 - ?__incr1) LIMIT 1";
    let c = circuit(&s, q);
    assert_eq!(window(&c), ["2"]);
}

/// Review of item 419: a refill whose evaluation changes a kept row's key drops the
/// stale row and places the new one in order.
#[test]
fn a_refill_reorders_rows_its_input_changed() {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = vec![format!(":g0 :w \"300\"^^<{INT}>")];
    data.extend((1..=102).map(|i| format!(":g{i} :w \"{}\"^^<{INT}>", 100 + i)));
    upd(&s, &format!("INSERT DATA {{ {} }}", data.join(" . ")));
    let q = "SELECT ?g ?x WHERE { { SELECT ?g (SAMPLE(?v) AS ?x) WHERE { ?g :w ?v } GROUP BY ?g } } ORDER BY DESC(?x) LIMIT 2";
    let mut c = circuit(&s, q);
    assert_eq!(window(&c), ["g0 300", "g102 202"]);
    // the maintained sample of g0 stays 300, and an evaluation takes the smallest, 1
    upd(&s, &format!("INSERT DATA {{ :g0 :w \"1\"^^<{INT}> }}"));
    step(&s, &mut c);
    assert_eq!(window(&c), ["g0 300", "g102 202"]);
    let d: Vec<String> = (2..=102)
        .map(|i| format!(":g{i} :w \"{}\"^^<{INT}>", 100 + i))
        .collect();
    upd(&s, &format!("DELETE DATA {{ {} }}", d.join(" . ")));
    step(&s, &mut c);
    assert_eq!(window_op(&c).refills, 1);
    assert_eq!(window(&c), ["g1 101", "g0 1"]);
}

/// Review of item 419: a LIMIT or OFFSET beyond the 64-bit signed range neither wraps
/// nor loses rows.
#[test]
fn huge_limits_and_offsets_do_not_wrap() {
    let s = Store::in_memory(StoreOptions::default());
    let data: Vec<String> = (1..=10).map(|i| triple(i, i as i64)).collect();
    upd(&s, &format!("INSERT DATA {{ {} }}", data.join(" . ")));
    let all = circuit(
        &s,
        "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 10000000000000000000",
    );
    assert_eq!(window(&all).len(), 10);
    let none = circuit(
        &s,
        "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 10 OFFSET 18446744073709551615",
    );
    assert!(window(&none).is_empty());
}

/// A window under a provenance setting refills like one without, and each of its rows
/// reports its copies as derivations whose atoms are not known.
#[test]
fn a_window_with_provenance_refills() {
    use super::prov::Provenance;
    let s = Store::in_memory(StoreOptions::default());
    let data: Vec<String> = (0..150).map(|i| triple(i, i as i64)).collect();
    upd(&s, &format!("INSERT DATA {{ {} }}", data.join(" . ")));
    let q = "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 2";
    let parsed = sparql::parse_query(&format!("{PREFIX}{q}"), None, &[]).expect("parse");
    let mut c = Circuit::new(compile(&parsed, None).expect("compile"), None);
    c.set_provenance(Provenance::why()).expect("provenance");
    c.build(s.snapshot(), &Control::none()).expect("build");
    assert_eq!(window(&c), ["s0", "s1"]);
    let d: Vec<String> = (0..120).map(|i| triple(i, i as i64)).collect();
    upd(&s, &format!("DELETE DATA {{ {} }}", d.join(" . ")));
    step(&s, &mut c);
    assert_eq!(window(&c), ["s120", "s121"]);
    assert_eq!(window_op(&c).refills, 1);
    for (r, w) in c.result.iter() {
        let a = c.annotation(r).expect("an annotation");
        assert_eq!(a.count, w);
        assert!(a.truncated, "a window's derivations are not known");
    }
}

/// Dates with and without a timezone have no total order, so a window that holds them
/// rebuilds its rows from its input at each pass, keeps the right number of rows and
/// takes them from the input.
#[test]
fn a_window_over_keys_without_a_total_order_rebuilds() {
    const DT: &str = "http://www.w3.org/2001/XMLSchema#dateTime";
    let s = Store::in_memory(StoreOptions::default());
    let date = |i: u32, tz: bool| {
        let z = if tz { "Z" } else { "" };
        // minutes apart, so a date without a timezone is indeterminate against one with
        format!(":d{i} :v \"2026-01-01T12:{i:02}:00{z}\"^^<{DT}>")
    };
    let data: Vec<String> = (0..20).map(|i| date(i, i % 2 == 0)).collect();
    upd(&s, &format!("INSERT DATA {{ {} }}", data.join(" . ")));
    let q = "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 3 OFFSET 1";
    let mut c = circuit(&s, q);
    let names: std::collections::BTreeSet<String> = (0..20).map(|i| format!("d{i}")).collect();
    let check = |c: &Circuit, left: &std::collections::BTreeSet<String>| {
        let w = window(c);
        assert_eq!(w.len(), 3.min(left.len().saturating_sub(1)));
        let mut seen = std::collections::BTreeSet::new();
        for r in &w {
            assert!(left.contains(r), "{r} is not in the input");
            assert!(seen.insert(r.clone()), "{r} twice");
        }
    };
    check(&c, &names);
    assert!(window_op(&c).partial(), "the keys have no total order");
    let mut left = names;
    for i in 0..18u32 {
        upd(&s, &format!("DELETE DATA {{ {} }}", date(i, i % 2 == 0)));
        left.remove(&format!("d{i}"));
        step(&s, &mut c);
        check(&c, &left);
    }
    upd(&s, &format!("INSERT DATA {{ {} }}", date(30, false)));
    left.insert("d30".into());
    step(&s, &mut c);
    check(&c, &left);
}
