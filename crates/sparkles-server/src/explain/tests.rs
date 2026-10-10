//! The notes, the template description and the checks of the `explain` role over
//! hand-written plans (C18 §6.6).

use super::*;
use serde_json::json;

fn node(op: &str, desc: &str, est: f64, act: i64, ms: f64, children: Vec<Value>) -> Value {
    json!({
        "operator": op,
        "description": desc,
        "columns": [],
        "estimatedRows": est,
        "estimatedCost": est,
        "actualRows": act,
        "timeMs": ms,
        "children": children,
    })
}

/// Project over a filter over a hash join of a scan and a path, as in §6.6.6.
fn slow_plan() -> Value {
    node(
        "Project",
        "?name",
        9.0,
        14,
        30_000.0,
        vec![node(
            "Filter",
            "REGEX(?name, \"^Ana\") [selectivity 0.01]",
            900.0,
            14,
            29_990.0,
            vec![node(
                "HashJoin",
                "on ?team",
                900.0,
                1_200_000,
                400.0,
                vec![
                    node(
                        "IndexScan",
                        "POS ?p <http://ex.org/memberOf> ?team",
                        214.0,
                        214,
                        1.0,
                        vec![],
                    ),
                    node(
                        "TransitivePath",
                        "?team (<http://ex.org/partOf>)+ <http://ex.org/Commerce>",
                        9.0,
                        4100,
                        300.0,
                        vec![],
                    ),
                ],
            )],
        )],
    )
}

#[test]
fn ids_are_paths_and_own_time_is_derived() {
    let p = Plan::read(&slow_plan()).unwrap();
    let ids: Vec<&str> = p.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, ["0", "0.0", "0.0.0", "0.0.0.0", "0.0.0.1"]);
    let f = p.get("0.0").unwrap();
    assert!((f.self_ms - 29_590.0).abs() < 1e-6);
    assert_eq!(f.description, "REGEX(?name, \"^Ana\")");
    assert!(p.executed);
}

#[test]
fn a_timeout_puts_the_budget_first_then_the_dominant_node() {
    let mut v = slow_plan();
    v["complete"] = false.into();
    v["children"][0]["complete"] = false.into();
    let mut p = Plan::read(&v).unwrap();
    mark_partial(&mut p, true);
    let stop = Stop::of_error_body(&json!({"error": "timed out", "timeoutSeconds": 30})).unwrap();
    let n = notes(&p, Some(&stop));
    assert_eq!(n[0].code, "budget");
    assert!(n[0].text.contains("30 s timeout"), "{}", n[0].text);
    assert_eq!(n[1].code, "dominant");
    assert_eq!(n[1].node.as_deref(), Some("0.0"));
    assert!(p.get("0.0").unwrap().partial);
    // the path is misestimated, and the join blew up
    assert!(
        n.iter()
            .any(|x| x.code == "misestimate" && x.node.as_deref() == Some("0.0.0.1"))
    );
    assert!(
        n.iter()
            .any(|x| x.code == "blowup" && x.node.as_deref() == Some("0.0.0"))
    );
    // partial counts are no misestimate
    assert!(
        !n.iter()
            .any(|x| x.code == "misestimate" && x.node.as_deref() == Some("0.0"))
    );
}

#[test]
fn hidden_estimates_are_not_misestimates() {
    let mut v = slow_plan();
    fn hide(v: &mut Value) {
        v["estimatedRows"] = (-1.0).into();
        v["estimatedCost"] = (-1.0).into();
        for c in v["children"].as_array_mut().unwrap() {
            hide(c);
        }
    }
    hide(&mut v);
    let p = Plan::read(&v).unwrap();
    assert!(p.hidden);
    let n = notes(&p, None);
    assert!(!n.iter().any(|x| x.code == "misestimate"));
    assert!(n.iter().any(|x| x.code == "hidden-estimates"));
}

#[test]
fn the_template_cites_nodes_in_every_sentence() {
    let p = Plan::read(&slow_plan()).unwrap();
    let labels = LocalNames {
        prefixes: &[],
        labels: Default::default(),
    };
    let s = describe(&p, "SELECT", &labels);
    assert!(!s.is_empty() && s.len() <= 4);
    for x in &s {
        assert!(!x.nodes.is_empty(), "{x:?}");
        assert!(x.nodes.iter().all(|n| p.has(n)), "{x:?}");
    }
    assert!(s[0].text.contains("memberOf"), "{}", s[0].text);
    assert!(s[0].text.contains("partOf"), "{}", s[0].text);
    assert!(
        s.iter()
            .any(|x| x.text.to_lowercase().contains("keeping those where")),
        "{s:?}"
    );
}

#[test]
fn a_streamed_plan_reads_like_an_eager_one() {
    let v = json!({
        "id": "0",
        "operator": {"id": "0", "operator": "Limit", "description": "offset 0 limit 3",
                     "columns": ["p"], "estimatedRows": 3.0, "estimatedCost": 3.0,
                     "actualRows": 3, "timeMs": 0.2, "children": []},
        "materializes": false, "fullInputBeforeOutput": false, "growingState": false,
        "complete": true,
        "children": [{
            "id": "0.0",
            "operator": {"id": "0.0", "operator": "IndexScan", "description": "PSO ?p <http://ex.org/name> ?n",
                         "columns": ["p", "n"], "estimatedRows": 500.0, "estimatedCost": 500.0,
                         "actualRows": 3, "timeMs": 0.1, "stoppedEarly": true, "children": []},
            "materializes": false, "fullInputBeforeOutput": false, "growingState": false,
            "complete": false, "children": []
        }]
    });
    let p = Plan::read(&v).unwrap();
    assert_eq!(p.nodes.len(), 2);
    let scan = p.get("0.0").unwrap();
    assert!(scan.stopped_early);
    let n = notes(&p, None);
    assert!(n.iter().any(|x| x.code == "stopped-early"));
    assert!(!n.iter().any(|x| x.code == "misestimate"));
}

#[test]
fn a_plan_that_is_not_a_plan_is_refused() {
    assert!(Plan::read(&json!({"x": 1})).is_err());
    assert!(Plan::read(&json!([1, 2])).is_err());
    let mut deep = node("Scan", "", 1.0, 1, 0.0, vec![]);
    for _ in 0..300 {
        deep = node("Filter", "", 1.0, 1, 0.0, vec![deep]);
    }
    assert!(Plan::read(&deep).is_err());
}

#[test]
fn model_answers_are_checked() {
    let p = Plan::read(&slow_plan()).unwrap();
    let n = notes(&p, None);
    let answer = json!({
        "asks": [
            {"text": "Finds people in Commerce teams.", "nodes": ["0.0.0"]},
            {"text": "Something about a node that is not there.", "nodes": ["0.9"]}
        ],
        "notes": [
            {"node": "0.0", "text": "The regex filter took 40 s."},
            {"node": "0.0.0.1", "text": "The path was estimated at 9 rows and gave 4,100."}
        ]
    });
    let c = model::check(&answer, &p, &n, &[]);
    assert_eq!(c.asks.len(), 1);
    assert_eq!(c.asks[0].nodes, ["0.0.0"]);
    assert_eq!(c.dropped, 1);
    assert_eq!(c.replaced, 1);
    let filter = c
        .notes
        .iter()
        .find(|x| x.node.as_deref() == Some("0.0"))
        .unwrap();
    assert_eq!(filter.source, "explain");
    assert!(!filter.text.contains("40 s"));
    let path = c
        .notes
        .iter()
        .find(|x| x.node.as_deref() == Some("0.0.0.1") && x.code == "misestimate")
        .unwrap();
    assert_eq!(path.source, "model");
}

#[test]
fn numbers_round_and_shorten() {
    assert_eq!(fmt_ms(29_590.0), "30 s");
    assert_eq!(fmt_ms(2100.0), "2.1 s");
    assert_eq!(fmt_ms(0.42), "0.4 ms");
    assert_eq!(fmt_rows(4100.0), "4,100");
    assert_eq!(fmt_rows(1_200_000.0), "1.2M");
    assert_eq!(
        clean("on ?a [hash 3 KiB] <http://x/[y]>"),
        "on ?a <http://x/[y]>"
    );
    assert_eq!(vars_in("?a and $b with ?a"), ["a", "b"]);
}
