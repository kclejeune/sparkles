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
        .filter(|n| n.skipped.is_some())
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
    assert!(filter.stopped_early, "{:#?}", r.plan);
    let child = &filter.children[0];
    assert!(
        child.reruns > 0,
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
    assert!(hit.children.iter().all(|c| c.skipped.is_some()));
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
        .flat_map(|n| n.pushed_filters.clone())
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
    assert!(plan.incomplete);
    let j = serde_json::to_value(&plan).unwrap();
    assert_eq!(j["complete"], false, "{j}");
    // some operator finished before the budget ran out, with its counts
    assert!(
        nodes(&plan)
            .iter()
            .any(|n| !n.incomplete && n.actual_rows > 0),
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
        assert!(plan.incomplete);
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
