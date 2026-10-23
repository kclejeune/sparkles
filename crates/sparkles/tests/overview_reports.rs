//! `ds.graph().groups` grouped by the community hierarchy of a GraphRAG reports graph
//! (specs U06 and C22 §4): the levels, the groups with their report titles, statuses and
//! member counts, the links counted between them, the members of the finest level, the
//! members a folded child left with its parent, and a view that may not read the reports
//! graph, which learns nothing of it.
//!
//! The reports graph here is written by hand in the form the server's build writes it.
//! `crates/sparkles-server` runs a real build against stub providers and checks the same
//! route on its output.

use serde_json::{Value as J, json};
use sparkles::Dataset;
use sparkles::access::{GraphAccess, Graphs};
use sparkles::branch::BranchOptions;
use sparkles::overview::GroupsRequest;
use sparkles::sparql::QueryOptions;
use std::sync::Arc;

const REPORTS: &str = "urn:x-sparkles:graphrag:reports";

/// Two teams. Team A has two sub-communities, `a6` was folded into A, and team B has
/// none, so B is at the finest level already.
const DATA: &str = r#"PREFIX ex: <http://ex/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
INSERT DATA { GRAPH <urn:g:data> {
  ex:a1 rdfs:label "Ann" ; ex:knows ex:a2, ex:a4 .
  ex:a2 rdfs:label "Ben" ; ex:knows ex:a3 .
  ex:a3 rdfs:label "Cid" ; ex:knows ex:b1 .
  ex:a4 ex:knows ex:a5 .
  ex:a5 a ex:Person .
  ex:a6 ex:knows ex:a1 .
  ex:b1 ex:knows ex:b2 .
  ex:b2 ex:knows ex:b3 .
  ex:b3 ex:note "x" .
} }"#;

const HIERARCHY: &str = r#"PREFIX ex: <http://ex/>
PREFIX gr: <urn:x-sparkles:graphrag#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
INSERT DATA { GRAPH <urn:x-sparkles:graphrag:reports> {
  <urn:c:A> a gr:Community ; gr:level 0 ; gr:memberCount 6 ;
    gr:member ex:a1, ex:a2, ex:a3, ex:a4, ex:a5, ex:a6 ; gr:report <urn:r:A> .
  <urn:r:A> rdfs:label "Team A"@en ; gr:status "reviewed" .
  <urn:c:B> a gr:Community ; gr:level 0 ; gr:memberCount 3 ;
    gr:member ex:b1, ex:b2, ex:b3 ; gr:report <urn:r:B> .
  <urn:r:B> rdfs:label "Team B"@en ; gr:status "stale" .
  <urn:c:A1> a gr:Community ; gr:level 1 ; skos:broader <urn:c:A> ; gr:memberCount 3 ;
    gr:member ex:a1, ex:a2, ex:a3 ; gr:report <urn:r:A1> .
  <urn:r:A1> rdfs:label "A one"@en ; gr:status "unreviewed" .
  <urn:c:A2> a gr:Community ; gr:level 1 ; skos:broader <urn:c:A> ; gr:memberCount 2 ;
    gr:member ex:a4, ex:a5 .
} }"#;

fn dataset() -> Dataset {
    let ds = Dataset::memory();
    ds.update(DATA).unwrap();
    ds.update(HIERARCHY).unwrap();
    ds
}

fn req(j: J) -> GroupsRequest {
    GroupsRequest::from_json(&j).unwrap()
}

fn groups(ds: &Dataset, j: J) -> J {
    ds.graph()
        .groups(&req(j), &QueryOptions::default())
        .unwrap()
}

fn top(ds: &Dataset) -> J {
    groups(ds, json!({ "by": { "reports": {} } }))
}

/// The id of the group labelled `label`.
fn id(answer: &J, label: &str) -> String {
    answer["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["label"] == label)
        .unwrap_or_else(|| panic!("no group {label}: {answer}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The edges as (source, target, count), with the ends named by `names`.
fn edges(answer: &J, names: &[(&str, String)]) -> Vec<(String, String, u64)> {
    let name = |k: &str| {
        names
            .iter()
            .find(|(_, key)| key == k)
            .map(|(n, _)| n.to_string())
            .unwrap_or_else(|| k.to_string())
    };
    let mut v: Vec<_> = answer["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                name(e["source"].as_str().unwrap()),
                name(e["target"].as_str().unwrap()),
                e["count"].as_u64().unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

#[test]
fn the_dataset_opens_at_the_coarsest_level() {
    let ds = dataset();
    let a = top(&ds);
    assert_eq!(a["available"], true, "{a}");
    assert_eq!(a["commit"], 2);
    assert_eq!(
        a["grouping"],
        json!({ "kind": "reports", "predicate": null, "level": 0 })
    );
    assert_eq!(
        a["within"],
        json!({ "reports": { "graph": REPORTS, "level": 0 }, "size": 9 })
    );
    assert_eq!(
        a["levels"],
        json!([{ "level": 0, "communities": 2 }, { "level": 1, "communities": 2 }])
    );
    let g = a["groups"].as_array().unwrap();
    let row = |i: usize| {
        (
            g[i]["label"].as_str().unwrap().to_string(),
            g[i]["size"].as_u64().unwrap(),
            g[i]["children"].as_u64().unwrap(),
            g[i]["report"]["status"].as_str().unwrap().to_string(),
            g[i]["community"].as_str().unwrap().to_string(),
        )
    };
    assert_eq!(
        (row(0), row(1)),
        (
            ("Team A".into(), 6, 2, "reviewed".into(), "urn:c:A".into()),
            ("Team B".into(), 3, 0, "stale".into(), "urn:c:B".into())
        )
    );
    assert_eq!(g[0]["report"]["iri"], "urn:r:A");
    assert_eq!(
        g[0]["top"],
        json!([
            { "iri": "http://ex/a1", "label": "Ann" },
            { "iri": "http://ex/a2", "label": "Ben" },
            { "iri": "http://ex/a3", "label": "Cid" }
        ])
    );
    let names = [
        ("A", format!("g:{}", id(&a, "Team A"))),
        ("B", format!("g:{}", id(&a, "Team B"))),
    ];
    // five links inside A, one from A to B, two inside B
    assert_eq!(
        edges(&a, &names),
        [
            ("A".into(), "A".into(), 5),
            ("A".into(), "B".into(), 1),
            ("B".into(), "B".into(), 2)
        ]
    );
    // a finer level opens the dataset there
    let fine = groups(&ds, json!({ "by": { "reports": { "level": 1 } } }));
    let labels: Vec<&str> = fine["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["A one", "Community A2"]);
    assert!(fine["groups"][1]["report"].is_null());
}

#[test]
fn a_community_opens_into_the_level_below_and_its_folded_members() {
    let ds = dataset();
    let t = top(&ds);
    let a_id = id(&t, "Team A");
    let a = groups(&ds, json!({ "within": { "group": a_id } }));
    assert_eq!(a["grouping"]["level"], 1);
    assert_eq!(a["within"]["community"], "urn:c:A");
    assert_eq!(a["within"]["title"], "Team A");
    assert_eq!(a["within"]["size"], 6);
    let g = a["groups"].as_array().unwrap();
    let rows: Vec<(String, u64, bool)> = g
        .iter()
        .map(|g| {
            (
                g["label"].as_str().unwrap().to_string(),
                g["size"].as_u64().unwrap(),
                g["folded"] == true,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("A one".to_string(), 3, false),
            ("Community A2".to_string(), 2, false),
            ("Members of no community below".to_string(), 1, true)
        ]
    );
    let names = [
        ("A1", format!("g:{}", id(&a, "A one"))),
        ("A2", format!("g:{}", id(&a, "Community A2"))),
        (
            "F",
            format!("g:{}", id(&a, "Members of no community below")),
        ),
        ("B", format!("g:{}", id(&t, "Team B"))),
    ];
    assert_eq!(
        edges(&a, &names),
        [
            ("A1".into(), "A1".into(), 2),
            ("A1".into(), "A2".into(), 1),
            ("A1".into(), "B".into(), 1),
            ("A2".into(), "A2".into(), 1),
            ("F".into(), "A1".into(), 1)
        ]
    );

    // the finest level opens into its members, linked to the groups drawn beside them
    let a1 = groups(&ds, json!({ "within": { "group": id(&a, "A one") } }));
    assert_eq!(a1["grouping"]["kind"], "members");
    assert_eq!(
        a1["members"],
        json!([
            { "iri": "http://ex/a1", "label": "Ann" },
            { "iri": "http://ex/a2", "label": "Ben" },
            { "iri": "http://ex/a3", "label": "Cid" }
        ])
    );
    let mut names = names.to_vec();
    for m in ["a1", "a2", "a3"] {
        names.push((m, format!("m:http://ex/{m}")));
    }
    assert_eq!(
        edges(&a1, &names),
        [
            ("F".into(), "a1".into(), 1),
            ("a1".into(), "A2".into(), 1),
            ("a1".into(), "a2".into(), 1),
            ("a2".into(), "a3".into(), 1),
            ("a3".into(), "B".into(), 1)
        ]
    );
    // the folded members, and every member of A on request
    let f = groups(
        &ds,
        json!({ "within": { "group": id(&a, "Members of no community below") } }),
    );
    assert_eq!(f["members"], json!([{ "iri": "http://ex/a6" }]));
    assert_eq!(f["within"]["folded"], true);
    let all = groups(
        &ds,
        json!({ "within": { "group": id(&t, "Team A") }, "by": "members" }),
    );
    assert_eq!(all["members"].as_array().unwrap().len(), 6);
    // a community without children opens into its members
    let b = groups(&ds, json!({ "within": { "group": id(&t, "Team B") } }));
    assert_eq!(b["grouping"]["kind"], "members");
    assert_eq!(b["within"]["size"], 3);
}

/// Review round 1: the folded members count against the limit, so an answer that the
/// limit fills with child communities says it left them out.
#[test]
fn a_full_answer_that_leaves_out_the_folded_members_is_truncated() {
    let ds = dataset();
    let a_id = id(&top(&ds), "Team A");
    let full = groups(&ds, json!({ "within": { "group": a_id }, "limit": 2 }));
    assert_eq!(full["groups"].as_array().unwrap().len(), 2);
    assert_eq!(full["truncated"], true);
    let room = groups(&ds, json!({ "within": { "group": a_id }, "limit": 3 }));
    assert_eq!(room["groups"].as_array().unwrap().len(), 3);
    assert_eq!(room["truncated"], false);
}

/// Review round 1: a group id names its reports graph, so a server default for the
/// dataset's graph does not replace it when a client opens the group.
#[test]
fn a_group_keeps_its_reports_graph_under_a_server_default() {
    let ds = dataset();
    let a_id = id(&top(&ds), "Team A");
    let mut r = req(json!({ "within": { "group": a_id }, "by": { "reports": {} } }));
    r.default_reports_graph("urn:x-sparkles:graphrag:elsewhere");
    let a = ds.graph().groups(&r, &QueryOptions::default()).unwrap();
    assert_eq!(a["within"]["title"], "Team A");
    assert_eq!(a["within"]["reports"]["graph"], REPORTS);
    // the default applies to a request for the whole dataset
    let mut r = req(json!({ "by": { "reports": {} } }));
    r.default_reports_graph("urn:x-sparkles:graphrag:elsewhere");
    let none = ds.graph().groups(&r, &QueryOptions::default()).unwrap();
    assert_eq!(none["unavailable"], "none");
}

#[test]
fn requests_that_do_not_fit_are_refused() {
    let ds = dataset();
    for (body, says) in [
        (
            json!({ "within": { "class": "http://ex/Person" }, "by": { "reports": {} } }),
            "whole dataset",
        ),
        (json!({ "by": "auto" }), "within is required"),
        (json!({ "by": { "reports": { "level": 99 } } }), "0 to 15"),
        (
            json!({ "by": { "reports": { "graph": "http://ex/g" } } }),
            "under urn:x-sparkles:graphrag:",
        ),
        (
            json!({ "by": { "reports": { "graph": "urn:x-sparkles:graphrag:disclosure" } } }),
            "footprints",
        ),
        (
            json!({ "by": { "reports": { "depth": 1 } } }),
            "unknown member",
        ),
    ] {
        let e = GroupsRequest::from_json(&body).unwrap_err().to_string();
        assert!(e.contains(says), "{body}: {e}");
    }
    let t = top(&ds);
    let a = id(&t, "Team A");
    let e = GroupsRequest::from_json(
        &json!({ "within": { "group": a }, "by": { "property": "http://ex/knows" } }),
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("opens by"), "{e}");
    // a level the reports do not have
    let e = ds
        .graph()
        .groups(
            &req(json!({ "by": { "reports": { "level": 4 } } })),
            &QueryOptions::default(),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("levels are 0, 1"), "{e}");
    // a made-up path
    let forged = t["groups"][1]["id"].as_str().unwrap().to_string();
    let b = groups(&ds, json!({ "within": { "group": forged } }));
    assert_eq!(b["available"], true);
    let swapped = {
        use base64::Engine;
        let e = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let body = json!({ "g": REPORTS, "l": 0, "p": ["urn:c:B", "urn:c:A1"] }).to_string();
        format!("r1.{}", e.encode(body))
    };
    let e = ds
        .graph()
        .groups(
            &req(json!({ "within": { "group": swapped } })),
            &QueryOptions::default(),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("one path"), "{e}");
}

#[test]
fn the_reports_belong_to_main() {
    let ds = dataset();
    ds.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = ds.branch("dev").unwrap();
    let e = dev
        .graph()
        .groups(
            &req(json!({ "by": { "reports": {} } })),
            &QueryOptions::default(),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("read on main"), "{e}");
    // the class overview stays on every branch
    assert!(
        dev.graph()
            .groups(
                &req(json!({ "within": { "class": "http://ex/Person" } })),
                &QueryOptions::default()
            )
            .is_ok()
    );
}

#[test]
fn a_past_state_reads_its_own_reports() {
    let ds = Dataset::memory();
    ds.update(DATA).unwrap();
    // an in-memory dataset keeps a past state that a snapshot pins
    ds.snapshots()
        .create("before", &"1".parse().unwrap(), &Default::default())
        .unwrap();
    ds.update(HIERARCHY).unwrap();
    let before = groups(
        &ds,
        json!({ "by": { "reports": {} }, "at": "snapshot:before" }),
    );
    assert_eq!(before["available"], false);
    assert_eq!(before["unavailable"], "none");
    assert_eq!(before["commit"], 1);
    let now = groups(&ds, json!({ "by": { "reports": {} } }));
    assert_eq!(now["groups"].as_array().unwrap().len(), 2);
}

/// A view that hides the reports graph, as the disclosure guard hides an unproven one,
/// learns nothing of it: no title, status, count, community or report.
#[test]
fn a_view_without_the_reports_graph_learns_nothing() {
    let ds = dataset();
    let t = top(&ds);
    let ids: Vec<String> = t["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap().to_string())
        .collect();
    let hidden = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::All.guarded("urn:x-sparkles:graphrag:*", &[]),
            Graphs::none(),
        ))),
        ..Default::default()
    };
    let mut bodies = vec![json!({ "by": { "reports": {} } })];
    for i in &ids {
        bodies.push(json!({ "within": { "group": i } }));
        bodies.push(json!({ "within": { "group": i }, "by": "members" }));
    }
    for body in bodies {
        let a = ds.graph().groups(&req(body.clone()), &hidden).unwrap();
        assert_eq!(a["available"], false, "{body}: {a}");
        assert_eq!(a["unavailable"], "access", "{a}");
        let text = a.to_string();
        for secret in [
            "Team",
            "A one",
            "urn:c:",
            "urn:r:",
            "reviewed",
            "stale",
            "http://ex/",
        ] {
            assert!(!text.contains(secret), "{body} leaks {secret}: {text}");
        }
        assert!(a["groups"].as_array().unwrap().is_empty());
        assert!(a["edges"].as_array().unwrap().is_empty());
        assert_eq!(a["within"]["size"], 0);
    }
    // the same view with the reports graph proven reads it
    let proven = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::All.guarded("urn:x-sparkles:graphrag:*", &[REPORTS.to_string()]),
            Graphs::none(),
        ))),
        ..Default::default()
    };
    let a = ds
        .graph()
        .groups(&req(json!({ "by": { "reports": {} } })), &proven)
        .unwrap();
    assert_eq!(a["available"], true);
    assert_eq!(a["groups"][0]["label"], "Team A");
}

#[test]
fn a_dataset_without_reports_says_so() {
    let ds = Dataset::memory();
    ds.update(DATA).unwrap();
    let a = top(&ds);
    assert_eq!(a["available"], false);
    assert_eq!(a["unavailable"], "none");
    assert!(
        a["reason"]
            .as_str()
            .unwrap()
            .contains("No GraphRAG communities")
    );
}
