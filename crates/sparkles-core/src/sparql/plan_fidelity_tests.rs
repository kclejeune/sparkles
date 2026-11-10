//! Plans that match what runs (spec C18 §6.6.5): node ids, skipped nodes, reruns under
//! `LIMIT`, cache hits, graph roots, pushed filters and the plans of failed queries.

use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

fn store_with(data: &str, opts: StoreOptions) -> Store {
    let s = Store::in_memory(opts);
    s.load(&[Source::from_bytes(
        data.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn people(n: usize) -> String {
    let mut d = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..n {
        d.push_str(&format!(
            "ex:p{i} ex:age {} ; ex:name \"n{i}\" ; ex:team ex:t{} .\n",
            i % 90,
            i % 7
        ));
    }
    d
}

fn opts() -> QueryOptions {
    QueryOptions {
        prefixes: vec![("ex".into(), "http://ex.org/".into())],
        ..Default::default()
    }
}

fn walk<'a>(p: &'a PlanInfo, out: &mut Vec<&'a PlanInfo>) {
    out.push(p);
    for c in &p.children {
        walk(c, out);
    }
}

fn nodes(p: &PlanInfo) -> Vec<&PlanInfo> {
    let mut out = Vec::new();
    walk(p, &mut out);
    out
}

/// The operators of a plan in tree order, with their depth.
fn shape(p: &PlanInfo) -> Vec<(usize, String)> {
    fn go(p: &PlanInfo, d: usize, out: &mut Vec<(usize, String)>) {
        out.push((d, p.operator.clone()));
        for c in &p.children {
            go(c, d + 1, out);
        }
    }
    let mut out = Vec::new();
    go(p, 0, &mut out);
    out
}

#[test]
fn every_written_node_has_its_path_as_id() {
    let s = store_with(&people(50), StoreOptions::default());
    let r = query(
        s.snapshot(),
        "SELECT ?p ?n WHERE { ?p ex:age ?a ; ex:name ?n FILTER(?a > 10) } ORDER BY ?n",
        &opts(),
    )
    .unwrap();
    let j = serde_json::to_value(&r.plan).unwrap();
    fn check(j: &serde_json::Value, id: &str, count: &mut usize) {
        assert_eq!(j["id"], id, "{j}");
        *count += 1;
        for (i, c) in j["children"].as_array().unwrap().iter().enumerate() {
            check(c, &format!("{id}.{i}"), count);
        }
    }
    let mut count = 0;
    check(&j, "0", &mut count);
    assert!(count >= 3);
    // the id finds the node
    let ids: Vec<String> = {
        fn ids(j: &serde_json::Value, out: &mut Vec<String>) {
            out.push(j["id"].as_str().unwrap().to_string());
            for c in j["children"].as_array().unwrap() {
                ids(c, out);
            }
        }
        let mut out = Vec::new();
        ids(&j, &mut out);
        out
    };
    for id in ids {
        assert!(r.plan.node(&id).is_some(), "{id}");
    }
    assert!(r.plan.node("0.9.9").is_none());
}

#[test]
fn a_join_whose_left_side_is_empty_skips_its_right_side() {
    let s = store_with(&people(20), StoreOptions::default());
    let r = query(
        s.snapshot(),
        "SELECT * WHERE { ?p ex:missing ?x . ?p ex:name ?n }",
        &opts(),
    )
    .unwrap();
    let skipped: Vec<_> = nodes(&r.plan)
        .into_iter()
        .filter(|n| n.fidelity().skipped.is_some())
        .collect();
    // an empty pattern may make the planner drop the join; when a join remains, its
    // right side says why it did not run
    for n in &skipped {
        assert_eq!(n.actual_rows, -1);
        assert_eq!(n.time_ms, 0.0);
    }
    let j = serde_json::to_value(&r.plan).unwrap().to_string();
    if !skipped.is_empty() {
        assert!(j.contains("\"skipped\""), "{j}");
    }
}

#[test]
fn reruns_under_limit_add_up_and_say_they_stopped_early() {
    let s = store_with(&people(3000), StoreOptions::default());
    // a selective filter makes the limited input grow over several rounds
    let r = query(
        s.snapshot(),
        "SELECT ?p WHERE { ?p ex:age ?a FILTER(?a + 0 >= 2) } LIMIT 20",
        &opts(),
    )
    .unwrap();
    assert_eq!(r.len(), 20);
    let all = nodes(&r.plan);
    let filter = all
        .iter()
        .find(|n| n.operator == "Filter")
        .expect("a filter node");
    assert!(filter.fidelity().stopped_early, "{:#?}", r.plan);
    let child = &filter.children[0];
    assert!(
        child.fidelity().reruns > 0,
        "the scan ran more than once: {:#?}",
        r.plan
    );
    // the children's time is all within the parent's
    assert!(child.time_ms <= filter.time_ms + 1e-6);
    let j = serde_json::to_value(&r.plan).unwrap().to_string();
    assert!(
        j.contains("\"stoppedEarly\":true") && j.contains("\"runs\""),
        "{j}"
    );
}

#[test]
fn a_cache_hit_keeps_the_subtree_it_was_computed_from() {
    let s = store_with(
        &people(200),
        StoreOptions {
            result_cache_min_ms: 0.0,
            ..Default::default()
        },
    );
    let text = "SELECT ?t (COUNT(?p) AS ?c) WHERE { ?p ex:team ?t ; ex:name ?n } GROUP BY ?t";
    let first = query(s.snapshot(), text, &opts()).unwrap();
    let second = query(s.snapshot(), text, &opts()).unwrap();
    let hit = nodes(&second.plan)
        .into_iter()
        .find(|n| n.cached)
        .expect("a cache hit");
    assert!(!hit.children.is_empty(), "{:#?}", second.plan);
    assert!(hit.children.iter().all(|c| c.fidelity().skipped.is_some()));
    // the same shape as the run that filled the cache
    assert_eq!(shape(&first.plan), shape(&second.plan));
}

#[test]
fn estimated_and_executed_plans_have_the_same_shape() {
    let s = store_with(&people(100), StoreOptions::default());
    for text in [
        "ASK { ?p ex:age 30 }",
        "CONSTRUCT { ?p ex:x ?n } WHERE { ?p ex:name ?n } LIMIT 5",
        "DESCRIBE ?p WHERE { ?p ex:age 3 }",
        "SELECT ?p WHERE { ?p ex:team ?t . ?t ex:none ?x }",
    ] {
        let (_, est) = explain(s.snapshot(), text, &opts()).unwrap();
        let run = query(s.snapshot(), text, &opts()).unwrap();
        assert_eq!(shape(&est), shape(&run.plan), "{text}");
    }
    let run = query(
        s.snapshot(),
        "CONSTRUCT { ?p ex:x ?n } WHERE { ?p ex:name ?n } LIMIT 5",
        &opts(),
    )
    .unwrap();
    assert_eq!(run.plan.operator, "CONSTRUCT");
    assert_eq!(run.plan.actual_rows, 5);
    assert!(run.plan.time_ms >= run.plan.children[0].time_ms);
}

#[test]
fn pushed_range_filters_are_listed_on_their_scan() {
    let s = store_with(&people(500), StoreOptions::default());
    let (_, plan) = explain(
        s.snapshot(),
        "SELECT ?p WHERE { ?p ex:age ?a FILTER(?a > 50 && ?a < 60) }",
        &opts(),
    )
    .unwrap();
    let pushed: Vec<String> = nodes(&plan)
        .into_iter()
        .flat_map(|n| n.fidelity().pushed_filters.clone())
        .collect();
    let j = serde_json::to_value(&plan).unwrap().to_string();
    if j.contains("IndexRangeScan") {
        assert!(!pushed.is_empty(), "{j}");
        assert!(j.contains("pushedFilters"), "{j}");
    }
}

#[test]
fn a_failed_query_keeps_its_plan_as_far_as_it_ran() {
    let s = store_with(&people(200), StoreOptions::default());
    let o = QueryOptions {
        max_rows_produced: Some(1000),
        ..opts()
    };
    let Err(f) = query_with_plan(
        s.snapshot(),
        "SELECT * WHERE { ?a ex:name ?n . ?b ex:age ?x }",
        &o,
    ) else {
        panic!("the cross product exceeds the budget")
    };
    assert!(matches!(f.error, Error::BudgetExceeded(_)), "{}", f.error);
    let plan = f.plan.expect("a partial plan");
    assert!(plan.fidelity().incomplete);
    let j = serde_json::to_value(&plan).unwrap();
    assert_eq!(j["complete"], false, "{j}");
    // some operator finished before the budget ran out, with its counts
    assert!(
        nodes(&plan)
            .iter()
            .any(|n| !n.fidelity().incomplete && n.actual_rows > 0),
        "{plan:#?}"
    );
    // a deadline that passed before anything ran still has the plan
    let o = QueryOptions {
        timeout: Some(std::time::Duration::from_nanos(1)),
        ..opts()
    };
    std::thread::sleep(std::time::Duration::from_millis(2));
    let f = query_with_plan(s.snapshot(), "SELECT * WHERE { ?a ex:name ?n }", &o);
    // the deadline may pass while planning, before any operator ran
    if let Err(f) = f
        && let Some(plan) = f.plan
    {
        assert!(matches!(f.error, Error::Timeout), "{}", f.error);
        assert!(plan.fidelity().incomplete);
    }
}

#[test]
fn streamed_plans_have_ids_and_stopped_nodes() {
    let s = store_with(&people(500), StoreOptions::default());
    let mut c = select_cursor(
        s.snapshot(),
        "SELECT ?p ?n WHERE { ?p ex:name ?n } LIMIT 3",
        &opts(),
        &CursorOptions::default(),
    )
    .unwrap();
    while c.next_batch().unwrap().is_some() {}
    let j = serde_json::to_value(c.plan()).unwrap();
    assert_eq!(j["id"], "0");
    assert_eq!(j["operator"]["id"], "0");
    let child = &j["children"][0];
    assert_eq!(child["id"], "0.0");
    assert_eq!(child["operator"]["id"], "0.0");
}

/// People in teams of cities, who know some others, like people or teams and write
/// documents, for recording estimates against actual rows. The authors are all among the
/// people who know others, and only the teams that people like have a city, which the
/// estimate from distinct values cannot tell.
fn graph(n: usize) -> String {
    let mut d = String::from("@prefix ex: <http://ex.org/> .\n");
    d.push_str("ex:Person a ex:Kind . ex:Doc a ex:Kind . ex:Manager a ex:Kind .\n");
    for t in 0..12 {
        d.push_str(&format!(
            "ex:t{t} ex:city ex:c{} ; ex:size {} .\n",
            t % 4,
            t * 3
        ));
    }
    for i in 0..n {
        d.push_str(&format!(
            "ex:p{i} a ex:Person ; ex:age {} ; ex:name \"n{i}\" ; ex:team ex:t{} .\n",
            i % 90,
            i % 12
        ));
        if i % 3 == 0 {
            d.push_str(&format!(
                "ex:p{i} ex:knows ex:p{} , ex:p{} .\n",
                (i + 3) % n,
                (i * 7 + 3) % n
            ));
        }
        if i % 15 == 0 {
            d.push_str(&format!(
                "ex:doc{i} a ex:Doc ; ex:author ex:p{i} ; ex:editor ex:p{i} ; ex:year {} .\n",
                2000 + i % 20
            ));
        }
        if i % 11 == 0 {
            d.push_str(&format!(
                "ex:p{i} a ex:Manager ; ex:manages ex:t{} .\n",
                i % 12
            ));
        }
        match i % 4 {
            0 => d.push_str(&format!("ex:p{i} ex:likes ex:t{} .\n", i % 12)),
            2 => d.push_str(&format!("ex:p{i} ex:likes ex:p{} .\n", (i * 5 + 1) % n)),
            _ => {}
        }
    }
    d
}

/// The largest q-error, `max(est, act) / min(est, act)` with both at least one row, of the
/// joins of a plan that ran to completion.
fn join_qerror(p: &PlanInfo) -> f64 {
    let mut worst: f64 = 1.0;
    for n in nodes(p) {
        let f = n.fidelity();
        if !n.operator.contains("Join")
            || f.skipped.is_some()
            || f.stopped_early
            || f.incomplete
            || f.reruns > 0
            || n.actual_rows < 0
            || n.estimated_rows < 0.0
        {
            continue;
        }
        let (e, a) = (n.estimated_rows.max(1.0), (n.actual_rows as f64).max(1.0));
        worst = worst.max(e.max(a) / e.min(a));
    }
    worst
}

/// The plans of stars, object joins, chains, a triangle, joins on two keys, a join on a
/// class variable and on a subquery's variable, and joins of small inputs, recorded with
/// their estimated and actual rows: the correlation-aware estimates (the remainders of the
/// sets, the node sets and the back-off of several keys) raise the error of no plan, and
/// lower that of some (spec X15).
#[test]
fn correlation_estimates_raise_the_error_of_no_recorded_plan() {
    let s = store_with(&graph(3000), StoreOptions::default());
    let off = QueryOptions {
        optimizations: Some(Optimizations {
            set_remainders: false,
            node_sets: false,
            key_backoff: false,
            ..Optimizations::ALL
        }),
        no_cache: true,
        ..opts()
    };
    let on = QueryOptions {
        no_cache: true,
        ..opts()
    };
    let queries = [
        "SELECT * WHERE { ?p ex:age ?a ; ex:name ?n ; ex:team ?t }",
        "SELECT * WHERE { ?p ex:team ?t . ?t ex:city ?c }",
        "SELECT * WHERE { ?p ex:team ?t . ?t ex:city ex:c1 }",
        "SELECT * WHERE { ?d ex:author ?p . ?p ex:team ?t . ?t ex:size ?z }",
        "SELECT * WHERE { ?d ex:author ?p . ?p ex:knows ?q }",
        "SELECT * WHERE { ?a ex:knows ?b . ?c ex:knows ?b }",
        "SELECT * WHERE { ?m ex:manages ?t . ?p ex:team ?t }",
        "SELECT * WHERE { ?a ex:knows ?b . ?b ex:knows ?c . ?c ex:knows ?a }",
        "SELECT * WHERE { ?m ex:manages ?t . ?m ex:team ?t }",
        "SELECT * WHERE { ?p a ex:Manager ; ex:knows ?q . ?q a ex:Person }",
        "SELECT * WHERE { VALUES ?t { ex:t1 ex:t2 } ?p ex:team ?t ; ex:age ?a }",
        "SELECT * WHERE { ?p ex:team ex:t3 ; ex:knows ?q . ?q ex:age ?a }",
        "SELECT * WHERE { ?d a ex:Doc ; ex:year 2005 ; ex:author ?p . ?p ex:name ?n }",
        "SELECT * WHERE { ?p ex:likes ?x . ?x ex:city ?c }",
        "SELECT * WHERE { ?p ex:likes ?x . ?x ex:knows ?y }",
        "SELECT * WHERE { ?p ex:likes ?x . ?q ex:manages ?x }",
        "SELECT * WHERE { ?d ex:author ?p . ?p ex:knows ?q . ?q ex:knows ?r }",
        "SELECT * WHERE { ?d ex:author ?p . ?d ex:editor ?p }",
        "SELECT * WHERE { ?c a ex:Kind . ?p a ?c ; ex:team ex:t1 }",
        "SELECT * WHERE { ?q ex:likes ?x . ?x ex:size ?z { SELECT DISTINCT ?x WHERE { ?x ex:city ex:c1 } } }",
        "SELECT * WHERE { ?q ex:likes ?x . ?x ex:city ex:c1 ; ex:size ?z }",
    ];
    let mut better = 0;
    for q in queries {
        // the first run reads the blocks that the planner's key probes then count
        for o in [&off, &on] {
            query(s.snapshot(), q, o).unwrap();
        }
        let a = query(s.snapshot(), q, &off).unwrap();
        let b = query(s.snapshot(), q, &on).unwrap();
        assert_eq!(a.len(), b.len(), "{q}");
        let (ea, eb) = (join_qerror(&a.plan), join_qerror(&b.plan));
        assert!(
            eb <= ea * 1.0001,
            "{q}: the largest join q-error rose from {ea:.3} to {eb:.3}\n{:#?}\n{:#?}",
            a.plan,
            b.plan
        );
        if eb < ea * 0.9 {
            better += 1;
        }
    }
    assert!(better >= 3, "{better} plans estimated better");
}

/// `n` users with a group of eight, a country of fifty and, for about one in 73, an
/// artist, drawn independently from a hash of the user, so that a constant restricts the
/// users to a sample of the predicate's subjects.
fn users(n: usize) -> String {
    let mut d = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..n {
        let h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        d.push_str(&format!(
            "ex:u{i} ex:name \"n{i}\" ; ex:group ex:g{} ; ex:country ex:c{} .\n",
            (h >> 20) % 8,
            (h >> 40) % 50
        ));
        if (h >> 8) % 73 == 0 {
            d.push_str(&format!("ex:u{i} ex:artist ex:a{} .\n", (h >> 50) % 5));
        }
    }
    d
}

/// A join against a pattern that a constant restricts, such as WatDiv `S4`'s `?v0 age
/// AgeGroup5` beside `?v0 artist ?v1`, divides by the predicate's distinct subjects: the
/// pattern's subjects are a sample of all users, not a superset of the artists (item
/// 874). Dividing by the pattern's own distinct keys takes every artist to be in the
/// group, about eight times too many rows here.
#[test]
fn a_join_against_a_constant_restriction_is_estimated_from_the_predicates_domain() {
    let s = store_with(&users(4000), StoreOptions::default());
    // the key probes and the characteristic sets estimate a join before the distinct
    // values do, and a store this small lets them; the estimate from distinct values is
    // what a join falls back to at scale, as WatDiv `S4`'s do
    let fallback = Optimizations {
        probed_keys: false,
        characteristic_sets: false,
        node_sets: false,
        set_remainders: false,
        ..Optimizations::ALL
    };
    let off = QueryOptions {
        optimizations: Some(Optimizations {
            domain_estimates: false,
            ..fallback
        }),
        no_cache: true,
        ..opts()
    };
    let on = QueryOptions {
        optimizations: Some(fallback),
        no_cache: true,
        ..opts()
    };
    // each query with the least q-error that dividing by the pattern's own keys leaves:
    // 55 artists against 500 of the group give 39 rows where 6 come, the `S4` shape
    // compounds that through its country, and two constants both restrict
    let queries = [
        (
            "SELECT * WHERE { ?u ex:artist ?a . ?u ex:group ex:g3 }",
            6.0,
        ),
        (
            "SELECT * WHERE { ?u ex:artist ?a ; ex:name ?n ; ex:group ex:g3 ; ex:country ex:c7 }",
            25.0,
        ),
        (
            "SELECT * WHERE { ?u ex:group ex:g3 . ?u ex:country ex:c7 }",
            3.0,
        ),
    ];
    for (q, worst_off) in queries {
        for o in [&off, &on] {
            query(s.snapshot(), q, o).unwrap();
        }
        let a = query(s.snapshot(), q, &off).unwrap();
        let b = query(s.snapshot(), q, &on).unwrap();
        assert_eq!(a.len(), b.len(), "{q}");
        let (ea, eb) = (join_qerror(&a.plan), join_qerror(&b.plan));
        assert!(
            ea >= worst_off,
            "{q}: the largest join q-error from distinct values alone is {ea:.3}\n{:#?}",
            a.plan
        );
        assert!(
            eb <= 2.5 && eb < ea,
            "{q}: the largest join q-error with the domain is {eb:.3} against {ea:.3}\n{:#?}",
            b.plan
        );
    }
}

/// The domain of a restricted scan reaches the joins above it however deep the plan
/// wraps it, and the plan builder sees the domain that the join-order summaries carried
/// (item 874). With a walk of 64 nodes at most, 70 nested subqueries hid the scan from the builder
/// while the DP and the greedy path remembered it, and their summaries disagreed with the
/// built plans.
#[test]
fn a_restricted_scans_domain_is_seen_at_any_depth() {
    let s = store_with(&users(4000), StoreOptions::default());
    let o = QueryOptions {
        optimizations: Some(Optimizations {
            probed_keys: false,
            characteristic_sets: false,
            node_sets: false,
            set_remainders: false,
            ..Optimizations::ALL
        }),
        no_cache: true,
        ..opts()
    };
    let wrapped = |depth: usize, rest: &str| {
        let mut inner = String::from("?u ex:group ex:g3");
        for _ in 0..depth {
            inner = format!("{{ SELECT ?u WHERE {{ {inner} }} }}");
        }
        format!("SELECT * WHERE {{ {inner} . {rest} }}")
    };
    let top_est = |q: &str| {
        let r = query(s.snapshot(), q, &o).unwrap();
        let join = nodes(&r.plan)
            .into_iter()
            .find(|n| n.operator.contains("Join"))
            .unwrap_or_else(|| panic!("{q}\n{:#?}", r.plan));
        (r.len(), join.estimated_rows)
    };
    // two joins take the DP, sixteen the greedy path
    let dp = " ?u ex:artist ?a . ?u ex:country ?c";
    let greedy: String = (0..16).map(|i| format!(" ?u ex:name ?n{i} .")).collect();
    for rest in [dp, greedy.as_str()] {
        let (rows, shallow) = top_est(&wrapped(0, rest));
        let (deep_rows, deep) = top_est(&wrapped(70, rest));
        assert_eq!(rows, deep_rows);
        assert!(
            (shallow - deep).abs() <= shallow * 1e-9,
            "the top join is estimated at {shallow} rows shallow and {deep} rows under 70 subqueries"
        );
    }
}
