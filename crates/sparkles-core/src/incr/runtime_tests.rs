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
    let snap = s.snapshot();
    let r = rt.read(&ViewRead {
        name,
        at: snap.commit,
        snap: Some(&snap),
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

/// Closing must fence store acquisition as well as a round already evaluating.
#[test]
fn close_waits_for_worker_store_acquisition() {
    let s = Arc::new(Store::in_memory(StoreOptions::default()));
    let rt = runtime(&s, None);
    let worker_store = s.clone();
    let (entered_tx, entered) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let release_rx = parking_lot::Mutex::new(release_rx);
    let worker = rt
        .spawn_worker("acquire".into(), s.subscribe_commits(), move || {
            entered_tx.send(()).unwrap();
            release_rx
                .lock()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            Some(worker_store.clone())
        })
        .unwrap();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    let closer_rt = rt.clone();
    let (closed_tx, closed) = std::sync::mpsc::channel();
    let closer = std::thread::spawn(move || {
        closer_rt.close(&s);
        closed_tx.send(()).unwrap();
    });
    let early = closed.recv_timeout(Duration::from_millis(100));
    release.send(()).unwrap();
    closer.join().unwrap();
    worker.join().unwrap();
    assert!(
        matches!(early, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
        "close returned while the worker could still acquire the store"
    );
    assert!(rt.is_closed());
}

/// The store wrapper may close its runtime when the worker releases it.
#[test]
fn a_worker_store_destructor_can_close_its_runtime() {
    struct ClosingStore {
        store: Arc<Store>,
        rt: Arc<Runtime>,
    }
    impl std::ops::Deref for ClosingStore {
        type Target = Store;
        fn deref(&self) -> &Store {
            &self.store
        }
    }
    impl Drop for ClosingStore {
        fn drop(&mut self) {
            self.rt.close(&self.store);
        }
    }
    let s = Arc::new(Store::in_memory(StoreOptions::default()));
    let rt = runtime(&s, None);
    let worker_rt = rt.clone();
    let worker = rt
        .spawn_worker("close-self".into(), s.subscribe_commits(), move || {
            Some(ClosingStore {
                store: s.clone(),
                rt: worker_rt.clone(),
            })
        })
        .unwrap();
    worker.join().unwrap();
    assert!(rt.is_closed());
}

/// A host's deferred close must finish even when its callback then panics.
#[test]
fn a_panicking_round_finishes_its_deferred_close() {
    struct ClosingHost {
        rt: std::sync::Weak<Runtime>,
        store: Arc<Store>,
    }
    impl Host for ClosingHost {
        fn access(&self, _: Option<&str>) -> crate::Result<Option<Arc<GraphAccess>>> {
            Ok(None)
        }
        fn stored_query(&self, _: &str) -> crate::Result<String> {
            unreachable!()
        }
        fn inference(&self) -> Option<super::inference::Inference> {
            self.rt.upgrade().unwrap().close(&self.store);
            panic!("worker fault after requesting close");
        }
    }
    let s = Arc::new(Store::in_memory(StoreOptions::default()));
    let rt = runtime(&s, None);
    rt.set_host(Arc::new(ClosingHost {
        rt: Arc::downgrade(&rt),
        store: s.clone(),
    }));
    let worker = rt
        .spawn_worker("close-panic".into(), s.subscribe_commits(), move || {
            Some(s.clone())
        })
        .unwrap();
    assert!(worker.join().is_err());
    assert!(rt.is_closed());
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

struct Fixed(Vec<String>);

impl crate::access::GuardDecision for Fixed {
    fn exempt(&self, _: &Arc<crate::store::Snapshot>) -> Vec<String> {
        self.0.clone()
    }
}

/// Guards that a request decides keep their graphs out of circuits, so two readers
/// whose deciders differ share one circuit and neither reads the guarded graph.
#[test]
fn guarded_graphs_stay_out_of_circuits() {
    use crate::access::Decider;
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    upd(&s, "INSERT DATA { GRAPH :r1 { :x :secret 1 } }");
    let guarded = |exempt: Vec<String>| {
        Arc::new(GraphAccess::graphs(
            Graphs::All.guarded_by("http://ex/r*", Decider(Arc::new(Fixed(exempt)))),
            Graphs::none(),
        ))
    };
    let allow = guarded(vec!["http://ex/r1".into()]);
    let deny = guarded(Vec::new());
    assert_eq!(
        super::runtime::access_fingerprint(Some(&allow)),
        super::runtime::access_fingerprint(Some(&deny))
    );
    let rt = runtime(&s, None);
    let query = q("SELECT ?s WHERE { GRAPH ?g { ?s :secret ?o } }");
    for a in [allow, deny] {
        let l = rt
            .live(&s, &query, Some(a), &LiveOptions::default())
            .unwrap();
        assert_eq!(live_rows(&l.try_next().unwrap()).1, Vec::<String>::new());
        upd(&s, "INSERT DATA { GRAPH :r1 { :y :secret 2 } }");
        rt.round(&s);
        assert!(l.try_next().is_none());
    }
}

/// A protection that depends on the data reacts to changes the query's patterns do
/// not name, and to changes in graphs the reader cannot read.
#[test]
fn data_dependent_protections_see_every_change() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let protection = Protection {
        name: "hide".into(),
        predicates: Some(vec!["http://ex/age".into()]),
        classes: Some(vec!["http://ex/Secret".into()]),
        subclasses: false,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    };
    let access = Arc::new(GraphAccess::with_triples(
        Graphs::Only(GraphRule::new(["default"], &[])),
        Graphs::none(),
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(protection),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    ));
    let rt = runtime(&s, None);
    let query = "SELECT ?p ?a WHERE { ?p :age ?a }";
    let l = rt
        .live(&s, &q(query), Some(access.clone()), &LiveOptions::default())
        .unwrap();
    let mut state = Vec::new();
    apply(&mut state, &l.try_next().unwrap());
    for u in [
        "INSERT DATA { :a a :Secret }",
        "INSERT DATA { GRAPH :hidden { :b a :Secret } }",
        "DELETE DATA { :a a :Secret }",
    ] {
        upd(&s, u);
        rt.round(&s);
        while let Some(e) = l.try_next() {
            apply(&mut state, &e);
        }
        state.sort();
        assert_eq!(state, oracle(&s, query, Some(access.clone())), "{u}");
    }
}

/// A view read at a commit its circuit has passed gives the rows of that commit.
#[test]
fn a_view_reads_at_the_query_commit() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    rt.put(&s, "team-sizes", ViewDef::table(q(TEAM_SIZES)))
        .unwrap();
    rt.round(&s);
    let at = |snap: &Arc<crate::store::Snapshot>| {
        rt.read(&ViewRead {
            name: "team-sizes",
            at: snap.commit,
            snap: Some(snap),
            allow_stale: false,
            caller: None,
            deadline: None,
        })
        .map(|r| (r.commit, rows_of(&r.rows)))
    };
    let first = s.snapshot();
    upd(&s, "INSERT DATA { :d org:memberOf :t2 }");
    rt.round(&s);
    // from the history
    assert_eq!(
        at(&first).unwrap(),
        (first.commit, vec!["t1 2".to_string(), "t2 1".into()])
    );
    // past the history: evaluated at the snapshot
    for i in 0..20 {
        upd(&s, &format!("INSERT DATA {{ :n{i} org:memberOf :t3 }}"));
        rt.round(&s);
    }
    assert_eq!(
        at(&first).unwrap(),
        (first.commit, vec!["t1 2".to_string(), "t2 1".into()])
    );
    // and without the snapshot it is refused
    let Err(e) = rt.read(&ViewRead {
        name: "team-sizes",
        at: first.commit,
        snap: None,
        allow_stale: false,
        caller: None,
        deadline: None,
    }) else {
        panic!("a read past the history without the snapshot");
    };
    assert!(format!("{e:?}").contains("view-ahead"), "{e:?}");
}

/// A diff starts from the subscriber's previous event, past commits that left the
/// result alone.
#[test]
fn a_diff_starts_from_the_previous_event() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let l = rt
        .live(&s, &q(TEAM_SIZES), None, &LiveOptions::default())
        .unwrap();
    let LiveEvent::Snapshot { commit: first, .. } = l.try_next().unwrap() else {
        panic!("a snapshot first");
    };
    upd(&s, "INSERT DATA { :a :age 31 }");
    rt.round(&s);
    upd(
        &s,
        "DELETE DATA { :a org:memberOf :t1 } ; INSERT DATA { :a org:memberOf :t1 }",
    );
    rt.round(&s);
    upd(&s, "INSERT DATA { :d org:memberOf :t2 }");
    rt.round(&s);
    let e = l.try_next().unwrap();
    assert!(
        matches!(e, LiveEvent::Diff { from, .. } if from == first),
        "{e:?}"
    );
}

/// Closing a live query wakes a thread waiting for its next event.
#[test]
fn closing_wakes_a_waiting_reader() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let l = Arc::new(
        rt.live(&s, &q(TEAM_SIZES), None, &LiveOptions::default())
            .unwrap(),
    );
    l.try_next().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let l2 = l.clone();
    let t = std::thread::spawn(move || tx.send(l2.next().is_none()).unwrap());
    std::thread::sleep(Duration::from_millis(50));
    l.close();
    assert_eq!(rx.recv_timeout(Duration::from_secs(10)), Ok(true));
    t.join().unwrap();
}

/// A queue that overflowed recovers with a reset while passes publish, without a
/// deadlock between the queue and the circuit.
#[test]
fn an_overflowed_queue_recovers_while_passes_run() {
    let s = Arc::new(Store::in_memory(StoreOptions::default()));
    load(&s);
    let rt = runtime(&s, None);
    let l = Arc::new(
        rt.live(
            &s,
            &q("SELECT ?s ?o WHERE { ?s :n ?o }"),
            None,
            &LiveOptions::default(),
        )
        .unwrap(),
    );
    l.try_next().unwrap();
    let mut data = String::from("INSERT DATA { ");
    for i in 0..1500 {
        data.push_str(&format!(":s{i} :n {i} . "));
    }
    data.push('}');
    upd(&s, &data);
    rt.round(&s);
    let (tx, rx) = std::sync::mpsc::channel();
    let (s2, rt2) = (s.clone(), rt.clone());
    let writer = std::thread::spawn(move || {
        for i in 0..200 {
            upd(&s2, &format!("INSERT DATA {{ :w{i} :n {i} }}"));
            rt2.round(&s2);
        }
    });
    let l2 = l.clone();
    let reader = std::thread::spawn(move || {
        let mut resets = 0;
        while let Some(e) = l2.next_timeout(Duration::from_secs(2)) {
            if matches!(e, LiveEvent::Snapshot { reset: true, .. }) {
                resets += 1;
            }
        }
        tx.send(resets).unwrap();
    });
    let resets = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("no deadlock");
    assert!(resets >= 1);
    writer.join().unwrap();
    reader.join().unwrap();
}

/// A host whose dataset's inference a test switches.
struct Inferring(parking_lot::Mutex<Option<super::Inference>>);

impl Host for Inferring {
    fn access(&self, _: Option<&str>) -> crate::Result<Option<Arc<GraphAccess>>> {
        Ok(None)
    }
    fn stored_query(&self, _: &str) -> crate::Result<String> {
        Err(crate::Error::NotFound("none".into()))
    }
    fn inference(&self) -> Option<super::Inference> {
        self.0.lock().clone()
    }
}

fn inferred_oracle(s: &Store, query: &str, inf: Option<&super::Inference>) -> Vec<String> {
    let r = sparql::query(
        s.snapshot(),
        &q(query),
        &QueryOptions {
            no_cache: true,
            rdfs: inf.and_then(|i| i.rdfs.clone()),
            default_graph_extra: inf
                .map(|i| i.default_graph_extra.clone())
                .unwrap_or_default(),
            ..Default::default()
        },
    )
    .expect("oracle");
    let rows: Vec<(Vec<Option<Term>>, i64)> = r.rows().into_iter().map(|r| (r, 1)).collect();
    rows_of(&rows)
}

/// Apply a live query's events to `rows`.
fn follow(l: &super::live::LiveQuery, rows: &mut Vec<String>) {
    while let Some(e) = l.try_next() {
        match &e {
            LiveEvent::Snapshot { .. } => *rows = live_rows(&e).1,
            LiveEvent::Diff { .. } => {
                let (removed, added) = live_rows(&e);
                for r in removed {
                    let i = rows.iter().position(|x| *x == r).expect("a removed row");
                    rows.remove(i);
                }
                rows.extend(added);
                rows.sort();
            }
            e => panic!("the live query ended: {e:?}"),
        }
    }
}

/// With RDFS on read or materialized inferences, a view and a live query evaluate the
/// whole query again with them, and turning them on and off switches the circuits
/// between the two modes without detaching the view or ending the live query.
#[test]
fn inference_switches_views_to_recompute() {
    use crate::sparql::rdfs::{RdfsOnRead, SchemaSource};
    const PEOPLE: &str = "SELECT ?x WHERE { ?x a foaf:Person }";
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    upd(
        &s,
        "INSERT DATA { :Student rdfs:subClassOf foaf:Person . :a a :Student . :b a foaf:Person . \
         GRAPH <urn:x-sparkles:inferred> { :c a foaf:Person } }",
    );
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    let host = Arc::new(Inferring(parking_lot::Mutex::new(None)));
    rt.set_host(host.clone());
    rt.put(&s, "people", ViewDef::table(q(PEOPLE))).unwrap();
    rt.put(&s, "sizes", ViewDef::table(q(TEAM_SIZES))).unwrap();
    rt.round(&s);
    let l = rt
        .live(&s, &q(PEOPLE), None, &LiveOptions::default())
        .unwrap();
    let mut live = Vec::new();
    let check = |live: &mut Vec<String>, inf: Option<&super::Inference>| {
        rt.round(&s);
        assert_eq!(
            read(&rt, &s, "people", None).unwrap(),
            inferred_oracle(&s, PEOPLE, inf)
        );
        assert_eq!(
            read(&rt, &s, "sizes", None).unwrap(),
            inferred_oracle(&s, TEAM_SIZES, inf)
        );
        follow(&l, live);
        assert_eq!(*live, inferred_oracle(&s, PEOPLE, inf));
    };
    check(&mut live, None);
    assert_eq!(live, ["b"]);
    let mode = |name: &str| {
        let v = rt.get(&s, name).unwrap();
        (v.evaluation, v.evaluation_reason)
    };
    assert_eq!(mode("people"), (Some(super::Mode::Incremental), None));

    let rdfs = super::Inference::new(
        Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
        Vec::new(),
    );
    *host.0.lock() = rdfs.clone();
    check(&mut live, rdfs.as_ref());
    assert_eq!(live, ["a", "b"]);
    assert_eq!(
        mode("people"),
        (Some(super::Mode::Recompute), Some("RDFS on read".into()))
    );
    upd(&s, "INSERT DATA { :d a :Student }");
    check(&mut live, rdfs.as_ref());
    upd(&s, "DELETE DATA { :Student rdfs:subClassOf foaf:Person }");
    check(&mut live, rdfs.as_ref());
    assert_eq!(live, ["b"]);

    let both = super::Inference::new(
        rdfs.as_ref().and_then(|i| i.rdfs.clone()),
        vec!["urn:x-sparkles:inferred".into()],
    );
    *host.0.lock() = both.clone();
    check(&mut live, both.as_ref());
    assert_eq!(live, ["b", "c"]);
    upd(&s, "INSERT DATA { :Student rdfs:subClassOf foaf:Person }");
    check(&mut live, both.as_ref());
    assert_eq!(live, ["a", "b", "c", "d"]);

    *host.0.lock() = None;
    check(&mut live, None);
    assert_eq!(live, ["b"]);
    assert_eq!(mode("people"), (Some(super::Mode::Incremental), None));
    upd(&s, "INSERT DATA { :e a foaf:Person }");
    check(&mut live, None);
    assert_eq!(live, ["b", "e"]);
}

/// A live query resumed at the commit of a switch of the inference gets the whole
/// result, since the switch changed the rows without a commit.
#[test]
fn a_resume_across_a_switch_resets() {
    use crate::sparql::rdfs::{RdfsOnRead, SchemaSource};
    const PEOPLE: &str = "SELECT ?x WHERE { ?x a foaf:Person }";
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA { :Student rdfs:subClassOf foaf:Person . :a a :Student . :b a foaf:Person }",
    );
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    let host = Arc::new(Inferring(parking_lot::Mutex::new(None)));
    rt.set_host(host.clone());
    let l = rt
        .live(&s, &q(PEOPLE), None, &LiveOptions::default())
        .unwrap();
    let Some(LiveEvent::Snapshot { commit, .. }) = l.try_next() else {
        panic!("a snapshot first");
    };
    *host.0.lock() = super::Inference::new(
        Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
        Vec::new(),
    );
    rt.round(&s);
    let again = rt
        .live(
            &s,
            &q(PEOPLE),
            None,
            &LiveOptions {
                after: Some(commit),
                ..Default::default()
            },
        )
        .unwrap();
    let e = again.try_next().unwrap();
    assert!(
        matches!(e, LiveEvent::Snapshot { reset: true, .. }),
        "{e:?}"
    );
    assert_eq!(live_rows(&e).1, ["a", "b"]);
    drop(l);
}

fn rdfs_graph() -> Option<super::Inference> {
    use crate::sparql::rdfs::{RdfsOnRead, SchemaSource};
    super::Inference::new(
        Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
        Vec::new(),
    )
}

/// A live query resumed at a commit before a switch gets the whole result even when its
/// circuit was collected and created again after the switch.
#[test]
fn a_resume_after_collection_resets() {
    const PEOPLE: &str = "SELECT ?x WHERE { ?x a foaf:Person }";
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA { :Student rdfs:subClassOf foaf:Person . :a a :Student . :b a foaf:Person }",
    );
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    let host = Arc::new(Inferring(parking_lot::Mutex::new(None)));
    rt.set_host(host.clone());
    let l = rt
        .live(&s, &q(PEOPLE), None, &LiveOptions::default())
        .unwrap();
    let Some(LiveEvent::Snapshot { commit, .. }) = l.try_next() else {
        panic!("a snapshot first");
    };
    drop(l);
    rt.round(&s);
    *host.0.lock() = rdfs_graph();
    rt.round(&s);
    let again = rt
        .live(
            &s,
            &q(PEOPLE),
            None,
            &LiveOptions {
                after: Some(commit),
                ..Default::default()
            },
        )
        .unwrap();
    let e = again.try_next().unwrap();
    assert!(
        matches!(e, LiveEvent::Snapshot { reset: true, .. }),
        "{e:?}"
    );
    assert_eq!(live_rows(&e).1, ["a", "b"]);
}

/// A view read back to the commit of a switch, after it moved on, has the rows of the
/// new mode.
#[test]
fn a_read_back_after_a_switch_uses_the_new_mode() {
    const PEOPLE: &str = "SELECT ?x WHERE { ?x a foaf:Person }";
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA { :Student rdfs:subClassOf foaf:Person . :a a :Student . :b a foaf:Person }",
    );
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    let host = Arc::new(Inferring(parking_lot::Mutex::new(None)));
    rt.set_host(host.clone());
    rt.put(&s, "people", ViewDef::table(q(PEOPLE))).unwrap();
    rt.round(&s);
    *host.0.lock() = rdfs_graph();
    rt.round(&s);
    let at = s.snapshot();
    upd(&s, "INSERT DATA { :c a foaf:Person }");
    rt.round(&s);
    let r = rt
        .read(&ViewRead {
            name: "people",
            at: at.commit,
            snap: Some(&at),
            allow_stale: false,
            caller: None,
            deadline: None,
        })
        .unwrap();
    assert_eq!(rows_of(&r.rows), ["a", "b"]);
}

/// The evaluation after a switch ends the live queries whose budget the new state
/// passes.
#[test]
fn a_switch_rebuild_enforces_live_budgets() {
    const PEOPLE: &str = "SELECT ?x WHERE { ?x a foaf:Person }";
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from("INSERT DATA { GRAPH <urn:x-sparkles:inferred> { ");
    for i in 0..2000 {
        data.push_str(&format!(":p{i} a foaf:Person . "));
    }
    data.push_str("} }");
    upd(&s, &data);
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    let host = Arc::new(Inferring(parking_lot::Mutex::new(None)));
    rt.set_host(host.clone());
    rt.put(&s, "people", ViewDef::table(q(PEOPLE))).unwrap();
    rt.round(&s);
    let l = rt
        .live(
            &s,
            &q(PEOPLE),
            None,
            &LiveOptions {
                max_memory_bytes: Some(16 * 1024),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(matches!(l.try_next(), Some(LiveEvent::Snapshot { .. })));
    *host.0.lock() = super::Inference::new(None, vec!["urn:x-sparkles:inferred".into()]);
    rt.round(&s);
    let e = l.try_next();
    assert!(
        matches!(&e, Some(LiveEvent::Error { code, .. }) if code == "over-budget"),
        "{e:?}"
    );
}

/// Switches of the inference from other threads while rounds run never leave a view
/// with rows of both modes.
#[test]
fn switches_racing_rounds_keep_views_exact() {
    const PEOPLE: &str = "SELECT ?x WHERE { ?x a foaf:Person }";
    let s = Arc::new(Store::in_memory(StoreOptions::default()));
    upd(
        &s,
        "INSERT DATA { :Student rdfs:subClassOf foaf:Person . :a a :Student . :b a foaf:Person }",
    );
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    let host = Arc::new(Inferring(parking_lot::Mutex::new(None)));
    rt.set_host(host.clone());
    rt.put(&s, "people", ViewDef::table(q(PEOPLE))).unwrap();
    rt.round(&s);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let switcher = {
        let (rt, s, host, stop) = (rt.clone(), s.clone(), host.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut on = false;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                on = !on;
                *host.0.lock() = if on { rdfs_graph() } else { None };
                let _ = rt.get(&s, "people");
            }
        })
    };
    for i in 0..300 {
        upd(&s, &format!("INSERT DATA {{ :n{i} a foaf:Person }}"));
        rt.round(&s);
        let rows = rt.get(&s, "people").unwrap().rows;
        // at most every person once, whatever the mode
        assert!(rows <= i as u64 + 3, "{rows} rows after {i} commits");
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    switcher.join().unwrap();
}

/// A circuit over inferred data skips a commit outside its footprint, and runs on a
/// change of a schema predicate or of the inferred graph.
#[test]
fn inferred_footprints_skip_unrelated_commits() {
    use crate::sparql::rdfs::{RdfsOnRead, SchemaSource};
    const PEOPLE: &str = "SELECT ?x WHERE { ?x a foaf:Person }";
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA { :Student rdfs:subClassOf foaf:Person . :a a :Student }",
    );
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    let inf = super::Inference::new(
        Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
        vec!["urn:x-sparkles:inferred".into()],
    );
    rt.set_host(Arc::new(Inferring(parking_lot::Mutex::new(inf.clone()))));
    rt.put(&s, "people", ViewDef::table(q(PEOPLE))).unwrap();
    rt.round(&s);
    let passes = |rt: &Runtime| {
        let v = rt.get(&s, "people").unwrap();
        (v.passes, v.skipped)
    };
    let (p0, k0) = passes(&rt);
    // unrelated: another predicate, a class outside the hierarchy
    upd(&s, "INSERT DATA { :z :knows :y . :t a :Teacher }");
    rt.round(&s);
    assert_eq!(passes(&rt), (p0, k0 + 1));
    // a schema change runs the pass, and brings :t in
    upd(
        &s,
        "INSERT DATA { :Teacher rdfs:subClassOf :Staff . :Staff rdfs:subClassOf foaf:Person }",
    );
    rt.round(&s);
    assert_eq!(passes(&rt), (p0 + 1, k0 + 1));
    assert_eq!(read(&rt, &s, "people", None).unwrap(), ["a", "t"]);
    // a subclass of the new hierarchy is in the footprint now
    upd(&s, "INSERT DATA { :u a :Staff }");
    rt.round(&s);
    assert_eq!(passes(&rt), (p0 + 2, k0 + 1));
    // the inferred graph counts as the default graph
    upd(
        &s,
        "INSERT DATA { GRAPH <urn:x-sparkles:inferred> { :v a foaf:Person } }",
    );
    rt.round(&s);
    assert_eq!(passes(&rt), (p0 + 3, k0 + 1));
    // another named graph does not
    upd(&s, "INSERT DATA { GRAPH :other { :w a foaf:Person } }");
    rt.round(&s);
    assert_eq!(passes(&rt), (p0 + 3, k0 + 2));
    assert_eq!(
        read(&rt, &s, "people", None).unwrap(),
        inferred_oracle(&s, PEOPLE, inf.as_ref())
    );
}

/// A view read back to an earlier commit keeps the view's own default graph, whatever
/// the reading query asks for.
#[test]
fn a_view_read_back_keeps_its_default_graph() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, "INSERT DATA { :a :v 1 . GRAPH :g { :b :v 2 } }");
    let rt = runtime(&s, None);
    rt.put(&s, "v", ViewDef::table(q("SELECT ?s WHERE { ?s :v ?o }")))
        .unwrap();
    rt.round(&s);
    let first = s.snapshot();
    let mut union = (*first).clone();
    union.union_default_graph = !first.union_default_graph;
    let union = Arc::new(union);
    // more changes than the view's history keeps
    for i in 0..20 {
        upd(&s, &format!("INSERT DATA {{ :n{i} :v 1 }}"));
        rt.round(&s);
    }
    rt.round(&s);
    let r = rt
        .read(&ViewRead {
            name: "v",
            at: first.commit,
            snap: Some(&union),
            allow_stale: false,
            caller: None,
            deadline: None,
        })
        .map(|r| rows_of(&r.rows));
    assert_eq!(r.ok(), Some(vec!["a".to_string()]));
}

/// An error that ends a stream survives the recovery of an overflowed queue.
#[test]
fn an_overflow_keeps_the_ending_error() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let l = rt
        .live(
            &s,
            &q("SELECT ?s ?o WHERE { ?s :n ?o }"),
            None,
            &LiveOptions {
                max_memory_bytes: Some(100_000),
                ..Default::default()
            },
        )
        .unwrap();
    l.try_next().unwrap();
    let mut data = String::from("INSERT DATA { ");
    for i in 0..5000 {
        data.push_str(&format!(":s{i} :n {i} . "));
    }
    data.push('}');
    upd(&s, &data);
    rt.round(&s);
    let mut last = None;
    while let Some(e) = l.try_next() {
        last = Some(e);
    }
    assert!(
        matches!(&last, Some(LiveEvent::Error { code, .. }) if code == "over-budget"),
        "{last:?}"
    );
}

/// Closing wakes every asynchronous reader.
#[test]
fn closing_wakes_every_async_reader() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let l = Arc::new(
        rt.live(&s, &q(TEAM_SIZES), None, &LiveOptions::default())
            .unwrap(),
    );
    l.try_next().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let l2 = l.clone();
    let t = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let both = rt.block_on(async {
            let a = tokio::spawn({
                let l = l2.clone();
                async move { l.recv().await.is_none() }
            });
            let b = tokio::spawn({
                let l = l2.clone();
                async move { l.recv().await.is_none() }
            });
            (a.await.unwrap(), b.await.unwrap())
        });
        tx.send(both).unwrap();
    });
    std::thread::sleep(Duration::from_millis(100));
    l.close();
    assert_eq!(rx.recv_timeout(Duration::from_secs(10)), Ok((true, true)));
    t.join().unwrap();
}
