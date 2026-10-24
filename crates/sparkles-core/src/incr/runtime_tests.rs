//! The runtime's acceptance examples (spec F13 §10).

use super::live::{LiveEvent, LiveOptions};
use super::runtime::{Host, OpenHost, Runtime, RuntimeOptions};
use super::views::{ViewDef, ViewState};
use crate::access::{
    Caller, GraphAccess, GraphRule, Graphs, Limits, Protection, Rule, TripleRules,
};
use crate::io::RdfFormat;
use crate::sparql::viewsvc::ViewRead;
use crate::sparql::{self, QueryOptions};
use crate::store::{Store, StoreOptions};
use oxrdf::Term;
use std::sync::Arc;
use std::time::Duration;

const PREFIX: &str = "PREFIX : <http://ex/> PREFIX org: <http://www.w3.org/ns/org#> \
                      PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
                      PREFIX foaf: <http://xmlns.com/foaf/0.1/> ";
const DATA: &str = r#"
@prefix : <http://ex/> .
@prefix org: <http://www.w3.org/ns/org#> .
:a org:memberOf :t1 . :b org:memberOf :t1 . :c org:memberOf :t2 .
:a :age 30 . :b :age 40 . :c :age 50 .
"#;
const TEAM_SIZES: &str =
    "SELECT ?team (COUNT(?p) AS ?n) WHERE { ?p org:memberOf ?team } GROUP BY ?team";

fn q(s: &str) -> String {
    format!("{PREFIX}{s}")
}

fn load(s: &Store) {
    s.load(&[crate::io::Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .expect("load");
}

fn upd(s: &Store, u: &str) {
    crate::sparql::update::update(s, &q(u), &QueryOptions::default()).expect("update");
}

fn runtime(s: &Store, root: Option<std::path::PathBuf>) -> Arc<Runtime> {
    let rt = Runtime::open(root, s.dataset_id(), RuntimeOptions::default());
    rt.set_host(Arc::new(OpenHost));
    rt
}

fn show(t: &Option<Term>) -> String {
    match t {
        None => "UNDEF".into(),
        Some(Term::NamedNode(n)) => n.as_str().trim_start_matches("http://ex/").to_string(),
        Some(Term::Literal(l)) => l.value().to_string(),
        Some(t) => t.to_string(),
    }
}

fn rows_of(rows: &[(Vec<Option<Term>>, i64)]) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .flat_map(|(r, w)| {
            let s = r.iter().map(show).collect::<Vec<_>>().join(" ");
            std::iter::repeat_n(s, *w as usize)
        })
        .collect();
    out.sort();
    out
}

fn read(
    rt: &Runtime,
    s: &Store,
    name: &str,
    caller: Option<&Arc<GraphAccess>>,
) -> crate::Result<Vec<String>> {
    let r = rt.read(&ViewRead {
        name,
        at: s.snapshot().commit,
        allow_stale: false,
        caller,
        deadline: None,
    })?;
    Ok(rows_of(&r.rows))
}

fn oracle(s: &Store, query: &str, graphs: Option<Arc<GraphAccess>>) -> Vec<String> {
    let r = sparql::query(
        s.snapshot(),
        &q(query),
        &QueryOptions {
            graphs,
            no_cache: true,
            ..Default::default()
        },
    )
    .expect("oracle");
    let rows: Vec<(Vec<Option<Term>>, i64)> = r.rows().into_iter().map(|r| (r, 1)).collect();
    rows_of(&rows)
}

fn live_rows(e: &LiveEvent) -> (Vec<String>, Vec<String>) {
    let f = |rows: &[super::live::LiveRow]| {
        let mut v: Vec<String> = rows
            .iter()
            .map(|r| {
                let s = r.terms.iter().map(show).collect::<Vec<_>>().join(" ");
                if r.count == 1 {
                    s
                } else {
                    format!("{s} x{}", r.count)
                }
            })
            .collect();
        v.sort();
        v
    };
    match e {
        LiveEvent::Snapshot { rows, .. } => (Vec::new(), f(rows)),
        LiveEvent::Diff { removed, added, .. } => (f(removed), f(added)),
        other => panic!("not rows: {other:?}"),
    }
}

/// A1 to A4: a view and a live query on `team-sizes` through a run of commits.
#[test]
fn a1_to_a4_team_sizes() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let info = rt
        .put(&s, "team-sizes", ViewDef::table(q(TEAM_SIZES)))
        .expect("put");
    assert!(info.plan.is_some());
    rt.round(&s);
    // A1
    assert_eq!(read(&rt, &s, "team-sizes", None).unwrap(), ["t1 2", "t2 1"]);
    assert_eq!(rt.get(&s, "team-sizes").unwrap().state, ViewState::Current);
    let live = rt
        .live(&s, &q(TEAM_SIZES), None, &LiveOptions::default())
        .expect("live");
    let first = live.try_next().expect("snapshot");
    assert!(matches!(first, LiveEvent::Snapshot { reset: false, .. }));
    assert_eq!(live_rows(&first).1, ["t1 2", "t2 1"]);
    // A2
    upd(&s, "INSERT DATA { :d org:memberOf :t2 }");
    rt.round(&s);
    assert_eq!(read(&rt, &s, "team-sizes", None).unwrap(), ["t1 2", "t2 2"]);
    let e = live.try_next().expect("a diff");
    assert_eq!(live_rows(&e), (vec!["t2 1".into()], vec!["t2 2".into()]));
    assert!(live.try_next().is_none());
    // A3
    let before = rt.get(&s, "team-sizes").unwrap();
    upd(&s, "INSERT DATA { :x foaf:name \"X\" }");
    rt.round(&s);
    let after = rt.get(&s, "team-sizes").unwrap();
    assert_eq!(after.passes, before.passes, "the footprint skips the pass");
    assert_eq!(after.skipped, before.skipped + 1);
    assert_eq!(after.state, ViewState::Current);
    assert!(live.try_next().is_none());
    // A4
    upd(
        &s,
        "DELETE DATA { :a org:memberOf :t1 } ; INSERT DATA { :a org:memberOf :t1 }",
    );
    rt.round(&s);
    assert!(live.try_next().is_none(), "no net change, no diff");
    assert_eq!(read(&rt, &s, "team-sizes", None).unwrap(), ["t1 2", "t2 2"]);
    // the view and the live query share one circuit
    assert_eq!(rt.stats(&s).circuits, 1);
    drop(live);
    rt.round(&s);
    assert_eq!(rt.stats(&s).live_queries, 0);
}

/// A5: OPTIONAL retracts the unbound row when the label arrives.
#[test]
fn a5_optional_label() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let query = "SELECT ?team ?l WHERE { ?p org:memberOf ?team OPTIONAL { ?team rdfs:label ?l } }";
    let live = rt
        .live(&s, &q(query), None, &LiveOptions::default())
        .expect("live");
    let _ = live.try_next();
    upd(&s, "INSERT DATA { :t1 rdfs:label \"One\" }");
    rt.round(&s);
    let e = live.try_next().expect("diff");
    let (rem, add) = live_rows(&e);
    assert_eq!(rem, ["t1 UNDEF x2"]);
    assert_eq!(add, ["t1 One x2"]);
}

fn persistent(dir: &std::path::Path, change_log: bool) -> Store {
    Store::open(
        dir,
        StoreOptions {
            change_log,
            history_max_generations: if change_log { 8 } else { 1 },
            ..Default::default()
        },
    )
    .expect("open")
}

/// A6: after a restart with commits made since the checkpoint, the view applies them
/// from the change log instead of being rebuilt.
#[test]
fn a6_recovery_applies_the_commits_since_the_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = persistent(dir.path(), true);
        load(&s);
        let rt = runtime(&s, Some(dir.path().to_path_buf()));
        rt.put(&s, "team-sizes", ViewDef::table(q(TEAM_SIZES)))
            .unwrap();
        rt.round(&s);
        rt.close(&s);
    }
    {
        let s = persistent(dir.path(), true);
        upd(&s, "INSERT DATA { :d org:memberOf :t2 }");
        upd(&s, "INSERT DATA { :e org:memberOf :t3 }");
        upd(&s, "DELETE DATA { :a org:memberOf :t1 }");
    }
    let s = persistent(dir.path(), true);
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    rt.round(&s);
    let st = rt.stats(&s);
    assert_eq!(st.loaded, 1, "the checkpoint was loaded");
    assert_eq!(st.resets, 0, "applied, not rebuilt");
    assert_eq!(st.passes, 1, "one pass for the three commits");
    assert_eq!(
        read(&rt, &s, "team-sizes", None).unwrap(),
        oracle(&s, TEAM_SIZES, None)
    );
}

/// A7: with the change log off, the view is rebuilt after a restart.
#[test]
fn a7_without_the_change_log_the_view_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = persistent(dir.path(), false);
        load(&s);
        let rt = runtime(&s, Some(dir.path().to_path_buf()));
        rt.put(&s, "team-sizes", ViewDef::table(q(TEAM_SIZES)))
            .unwrap();
        rt.round(&s);
        rt.close(&s);
    }
    {
        let s = persistent(dir.path(), false);
        upd(&s, "INSERT DATA { :d org:memberOf :t2 }");
        // a compaction leaves no history to diff the states with
        s.compact().unwrap();
    }
    let s = persistent(dir.path(), false);
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    assert_eq!(rt.get(&s, "team-sizes").unwrap().state, ViewState::Building);
    rt.round(&s);
    assert_eq!(rt.stats(&s).resets, 1, "rebuilt");
    assert_eq!(read(&rt, &s, "team-sizes", None).unwrap(), ["t1 2", "t2 2"]);
}

struct Reports;

impl Host for Reports {
    fn access(&self, principal: Option<&str>) -> crate::Result<Option<Arc<GraphAccess>>> {
        match principal {
            Some("reports") | None => Ok(None),
            Some(p) => Err(crate::Error::NotFound(format!("no principal {p}"))),
        }
    }
    fn stored_query(&self, _: &str) -> crate::Result<String> {
        Err(crate::Error::NotFound("none".into()))
    }
}

/// A8: a caller who cannot read the default graph cannot read a view of it.
#[test]
fn a8_view_access_is_refused_to_narrower_callers() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    rt.set_host(Arc::new(Reports));
    let mut def = ViewDef::table(q(TEAM_SIZES));
    def.principal = Some("reports".into());
    rt.put(&s, "team-sizes", def).unwrap();
    rt.round(&s);
    let narrow = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["http://ex/g1"], &[])),
        Graphs::none(),
    ));
    let e = read(&rt, &s, "team-sizes", Some(&narrow)).unwrap_err();
    assert!(
        e.to_string().contains("view-access") || format!("{e:?}").contains("view-access"),
        "{e:?}"
    );
    let all = Arc::new(GraphAccess::all());
    assert_eq!(
        read(&rt, &s, "team-sizes", Some(&all)).unwrap(),
        ["t1 2", "t2 1"]
    );
    // an unknown principal is refused at definition
    let mut def = ViewDef::table(q(TEAM_SIZES));
    def.principal = Some("nobody".into());
    assert!(rt.put(&s, "other", def).is_err());
}

fn hiding(pred: &str, classes: Option<Vec<String>>) -> Arc<GraphAccess> {
    let protection = Protection {
        name: "hide".into(),
        predicates: Some(vec![pred.into()]),
        classes,
        subclasses: false,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    };
    Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(protection),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    ))
}

/// A9: subscribers with different protections get the diffs of their own masked
/// snapshots, for protections by predicate and protections that depend on the data.
#[test]
fn a9_live_queries_follow_their_protections() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    upd(&s, "INSERT DATA { :a a :Secret }");
    let rt = runtime(&s, None);
    let query = "SELECT ?p ?a WHERE { ?p org:memberOf ?t OPTIONAL { ?p :age ?a } }";
    let by_pred = hiding("http://ex/age", None);
    let by_class = hiding("http://ex/age", Some(vec!["http://ex/Secret".into()]));
    let subs: Vec<(Option<Arc<GraphAccess>>, _)> = [None, Some(by_pred), Some(by_class)]
        .into_iter()
        .map(|a| {
            let l = rt
                .live(&s, &q(query), a.clone(), &LiveOptions::default())
                .unwrap();
            (a, l)
        })
        .collect();
    assert_eq!(rt.stats(&s).circuits, 3);
    let mut state: Vec<Vec<String>> = Vec::new();
    for (a, l) in &subs {
        let (_, rows) = live_rows(&l.try_next().unwrap());
        assert_eq!(rows, expand(&oracle(&s, query, a.clone())));
        state.push(rows);
    }
    for u in [
        "INSERT DATA { :d org:memberOf :t2 . :d :age 20 }",
        "INSERT DATA { :b a :Secret }",
        "DELETE DATA { :a a :Secret }",
        "DELETE DATA { :c :age 50 }",
    ] {
        upd(&s, u);
        rt.round(&s);
        for (i, (a, l)) in subs.iter().enumerate() {
            while let Some(e) = l.try_next() {
                apply(&mut state[i], &e);
            }
            state[i].sort();
            assert_eq!(
                state[i],
                expand(&oracle(&s, query, a.clone())),
                "{u}, subscriber {i}"
            );
        }
    }
}

fn expand(rows: &[String]) -> Vec<String> {
    rows.to_vec()
}

fn apply(state: &mut Vec<String>, e: &LiveEvent) {
    let one = |r: &super::live::LiveRow| r.terms.iter().map(show).collect::<Vec<_>>().join(" ");
    match e {
        LiveEvent::Snapshot { rows, .. } => {
            state.clear();
            for r in rows.iter() {
                for _ in 0..r.count {
                    state.push(one(r));
                }
            }
        }
        LiveEvent::Diff { removed, added, .. } => {
            for r in removed.iter() {
                for _ in 0..r.count {
                    let i = state
                        .iter()
                        .position(|x| *x == one(r))
                        .expect("a removed row is held");
                    state.remove(i);
                }
            }
            for r in added.iter() {
                for _ in 0..r.count {
                    state.push(one(r));
                }
            }
        }
        other => panic!("{other:?}"),
    }
}

/// A10: a path with `+` is a `recompute` node, and `NOW()` is refused.
#[test]
fn a10_paths_recompute_and_now_is_refused() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let info = rt
        .put(
            &s,
            "reach",
            ViewDef::table(q("SELECT ?p ?q WHERE { ?p foaf:knows+ ?q }")),
        )
        .unwrap();
    let plan = serde_json::to_string(&info.plan).unwrap();
    assert!(plan.contains("recompute"), "{plan}");
    let e = rt
        .put(
            &s,
            "now",
            ViewDef::table(q("SELECT ?p (NOW() AS ?t) WHERE { ?p org:memberOf ?x }")),
        )
        .unwrap_err();
    assert!(
        format!("{e:?}").contains("not-incremental") && e.to_string().contains("NOW"),
        "{e:?}"
    );
}

/// A14: a window of one row sends a diff only when the first team changes.
#[test]
fn a14_a_window_changes_only_with_its_rows() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let query = "SELECT ?team (COUNT(?p) AS ?n) WHERE { ?p org:memberOf ?team } GROUP BY ?team ORDER BY DESC(?n) LIMIT 1";
    let live = rt
        .live(&s, &q(query), None, &LiveOptions::default())
        .unwrap();
    assert_eq!(live_rows(&live.try_next().unwrap()).1, ["t1 2"]);
    upd(&s, "INSERT DATA { :d org:memberOf :t2 }");
    rt.round(&s);
    // t2 has 2 as well; the first row may stay t1 or change by the tie order
    let _ = live.try_next();
    upd(&s, "INSERT DATA { :x foaf:name \"x\" }");
    rt.round(&s);
    assert!(live.try_next().is_none());
    upd(&s, "INSERT DATA { :e org:memberOf :t2 }");
    rt.round(&s);
    let mut state = Vec::new();
    apply(
        &mut state,
        &LiveEvent::Snapshot {
            commit: 0,
            vars: Arc::from(Vec::new()),
            rows: Arc::from(Vec::new()),
            value: None,
            reset: false,
        },
    );
    let e = live.try_next().expect("the first team changed");
    let (_, added) = live_rows(&e);
    assert_eq!(added, ["t2 3"]);
}

/// A15: a live query resumed after a commit receives the diff since it, from the
/// history of a running circuit or by evaluating the query before the changes, and a
/// reset when the changes are not known.
#[test]
fn a15_resume_sends_the_diff_or_a_reset() {
    let dir = tempfile::tempdir().unwrap();
    let s = persistent(dir.path(), true);
    load(&s);
    let rt = runtime(&s, None);
    let start = s.snapshot().commit;
    let running = rt
        .live(&s, &q(TEAM_SIZES), None, &LiveOptions::default())
        .unwrap();
    for u in [
        "INSERT DATA { :d org:memberOf :t2 }",
        "INSERT DATA { :x foaf:name \"x\" }",
        "INSERT DATA { :e org:memberOf :t3 }",
    ] {
        upd(&s, u);
        rt.round(&s);
    }
    let resumed = rt
        .live(
            &s,
            &q(TEAM_SIZES),
            None,
            &LiveOptions {
                after: Some(start),
                ..Default::default()
            },
        )
        .unwrap();
    let e = resumed.try_next().unwrap();
    assert!(
        matches!(e, LiveEvent::Diff { from, .. } if from == start),
        "{e:?}"
    );
    assert_eq!(
        live_rows(&e),
        (vec!["t2 1".into()], vec!["t2 2".into(), "t3 1".into()])
    );
    drop(resumed);
    drop(running);
    rt.round(&s);
    // without a running circuit: the query evaluated before the changes
    let resumed = rt
        .live(
            &s,
            &q(TEAM_SIZES),
            None,
            &LiveOptions {
                after: Some(start),
                ..Default::default()
            },
        )
        .unwrap();
    let e = resumed.try_next().unwrap();
    assert_eq!(
        live_rows(&e),
        (vec!["t2 1".into()], vec!["t2 2".into(), "t3 1".into()])
    );
    drop(resumed);
    // without the change log and after a compaction the changes are not known: a reset
    let dir = tempfile::tempdir().unwrap();
    let m = persistent(dir.path(), false);
    load(&m);
    let start = m.snapshot().commit;
    upd(&m, "INSERT DATA { :d org:memberOf :t2 }");
    m.compact().unwrap();
    let rt = runtime(&m, None);
    let l = rt
        .live(
            &m,
            &q(TEAM_SIZES),
            None,
            &LiveOptions {
                after: Some(start),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(matches!(
        l.try_next().unwrap(),
        LiveEvent::Snapshot { reset: true, .. }
    ));
}

/// ASK streams its value only when it changes.
#[test]
fn ask_streams_value_changes() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let l = rt
        .live(
            &s,
            &q("ASK { ?p :left true }"),
            None,
            &LiveOptions::default(),
        )
        .unwrap();
    assert!(matches!(
        l.try_next().unwrap(),
        LiveEvent::Snapshot {
            value: Some(false),
            ..
        }
    ));
    upd(&s, "INSERT DATA { :a :left true }");
    rt.round(&s);
    assert!(matches!(
        l.try_next().unwrap(),
        LiveEvent::Value { value: true, .. }
    ));
    upd(&s, "INSERT DATA { :b :left true }");
    rt.round(&s);
    assert!(l.try_next().is_none());
}

/// The limits of live queries.
#[test]
fn live_limits_hold() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = Runtime::open(
        None,
        s.dataset_id(),
        RuntimeOptions {
            max_live: 3,
            max_live_per_principal: 2,
            ..Default::default()
        },
    );
    rt.set_host(Arc::new(OpenHost));
    let o = LiveOptions {
        principal: Some("u".into()),
        ..Default::default()
    };
    let a = rt.live(&s, &q(TEAM_SIZES), None, &o).unwrap();
    let _b = rt.live(&s, &q(TEAM_SIZES), None, &o).unwrap();
    assert!(rt.live(&s, &q(TEAM_SIZES), None, &o).is_err());
    let _c = rt
        .live(&s, &q(TEAM_SIZES), None, &LiveOptions::default())
        .unwrap();
    assert!(
        rt.live(&s, &q(TEAM_SIZES), None, &LiveOptions::default())
            .is_err()
    );
    drop(a);
    assert!(rt.live(&s, &q(TEAM_SIZES), None, &o).is_ok());
}

/// A checkpoint of an unknown format is refused and left in place, and the view is
/// built from the data; a `view.json` of a newer format refuses changes to the views.
#[test]
fn unknown_formats_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let s = persistent(dir.path(), true);
    load(&s);
    {
        let rt = runtime(&s, Some(dir.path().to_path_buf()));
        rt.put(&s, "team-sizes", ViewDef::table(q(TEAM_SIZES)))
            .unwrap();
        rt.round(&s);
        rt.close(&s);
    }
    let state = dir.path().join("views/team-sizes/main");
    let files = super::persist::list(&state);
    assert_eq!(files.len(), 1);
    let mut bytes = std::fs::read(&files[0].1).unwrap();
    assert!(super::persist::decode(&bytes).is_ok());
    bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
    std::fs::write(&files[0].1, &bytes).unwrap();
    assert!(matches!(
        super::persist::decode(&bytes),
        Err(crate::Error::Unsupported(_))
    ));
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    rt.round(&s);
    assert_eq!(read(&rt, &s, "team-sizes", None).unwrap(), ["t1 2", "t2 1"]);
    assert!(files[0].1.exists(), "the unknown file is left in place");
    rt.close(&s);
    drop(rt);
    // a newer view.json
    let file = dir.path().join("views/team-sizes/view.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    v["format"] = 2.into();
    std::fs::write(&file, serde_json::to_vec(&v).unwrap()).unwrap();
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    let e = rt
        .put(&s, "other", ViewDef::table(q(TEAM_SIZES)))
        .unwrap_err();
    assert!(matches!(e, crate::Error::Unsupported(_)), "{e:?}");
    // Phase 2 options are refused
    let mut def = ViewDef::table(q(TEAM_SIZES));
    def.keep_commits = 3;
    assert!(def.check().is_err());
    let e = serde_json::from_str::<ViewDef>(r#"{"query":"x","colour":"red"}"#);
    assert!(e.is_err(), "unknown members are refused");
}

/// The worker thread runs rounds after commits, and reads wait for the view.
#[test]
fn the_worker_follows_commits() {
    let s = Arc::new(Store::in_memory(StoreOptions::default()));
    load(&s);
    let rt = runtime(&s, None);
    let weak = Arc::downgrade(&s);
    let h = rt
        .spawn_worker("views".into(), s.subscribe_commits(), move || {
            weak.upgrade()
        })
        .unwrap();
    rt.put(&s, "team-sizes", ViewDef::table(q(TEAM_SIZES)))
        .unwrap();
    let live = rt
        .live(&s, &q(TEAM_SIZES), None, &LiveOptions::default())
        .unwrap();
    let _ = live.next_timeout(Duration::from_secs(5)).expect("snapshot");
    upd(&s, "INSERT DATA { :d org:memberOf :t2 }");
    assert_eq!(read(&rt, &s, "team-sizes", None).unwrap(), ["t1 2", "t2 2"]);
    let e = live.next_timeout(Duration::from_secs(5)).expect("diff");
    assert!(matches!(e, LiveEvent::Diff { .. }));
    rt.close(&s);
    drop(rt);
    drop(live);
    h.join().unwrap();
}

/// `SERVICE view:read` reads a view's rows inside a query, with its commit, and a
/// query without views or naming an unknown column fails.
#[test]
fn service_view_read_reads_rows() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    rt.put(&s, "team-sizes", ViewDef::table(q(TEAM_SIZES)))
        .unwrap();
    rt.round(&s);
    let text = q(
        "PREFIX view: <urn:x-sparkles:view#> SELECT ?team ?n ?c WHERE { \
                  SERVICE view:read { [] view:name \"team-sizes\" ; view:bind ?team, ?n ; view:commit ?c } \
                  FILTER(?n > 1) }",
    );
    let o = QueryOptions {
        views: Some(rt.clone()),
        ..Default::default()
    };
    let r = sparql::query(s.snapshot(), &text, &o).unwrap();
    let rows = r.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(show(&rows[0][0]), "t1");
    assert_eq!(show(&rows[0][2]), s.snapshot().commit.to_string());
    assert!(sparql::query(s.snapshot(), &text, &QueryOptions::default()).is_err());
    let bad = q("PREFIX view: <urn:x-sparkles:view#> SELECT * WHERE { \
                 SERVICE view:read { [] view:name \"team-sizes\" ; view:bind ?nope } }");
    assert!(sparql::query(s.snapshot(), &bad, &o).is_err());
}
