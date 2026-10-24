//! Views and live queries on datasets with RDFS on read or materialized inferences
//! (spec F13 §4.5). They evaluate the whole query again with the dataset's inference,
//! so their rows are those of the one-off query after every change, and turning the
//! inference on or off switches them between the two modes without detaching them.

use oxrdf::Term;
use sparkles::Dataset;
use sparkles::incr::{LiveEvent, LiveOptions, LiveQuery, LiveRow, Mode, ViewDef};
use sparkles::io::RdfFormat;
use sparkles::reasoning::rdfs::NewSchema;
use sparkles::reasoning::{INFERRED_GRAPH, ReasoningRecord};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const PEOPLE: &str = "SELECT ?x WHERE { ?x a <urn:Person> }";
const COUNTS: &str = "SELECT ?t (COUNT(?x) AS ?n) WHERE { ?x a ?t } GROUP BY ?t";
const SUB: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const WAIT: Duration = Duration::from_secs(20);

fn show(row: &[Option<Term>]) -> String {
    row.iter()
        .map(|t| t.as_ref().map_or("UNDEF".to_string(), |t| t.to_string()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The rows of the one-off query, with the dataset's query defaults.
fn oracle(ds: &Dataset, q: &str) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for r in ds.query(q).unwrap().rows() {
        *m.entry(show(&r)).or_insert(0) += 1;
    }
    m
}

/// The view's rows at the head.
fn view(ds: &Dataset, name: &str) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for (r, w) in ds.views().read(name).unwrap().rows {
        *m.entry(show(&r)).or_insert(0) += w;
    }
    m
}

/// A live query's rows as its events build them.
struct Follower {
    live: LiveQuery,
    rows: BTreeMap<String, i64>,
    commit: u64,
}

impl Follower {
    fn new(ds: &Dataset, q: &str) -> Follower {
        let live = ds.live(q, &LiveOptions::default()).unwrap();
        let mut f = Follower {
            live,
            rows: BTreeMap::new(),
            commit: 0,
        };
        f.apply(f.live.next_timeout(WAIT).expect("a first event"));
        f
    }

    fn apply(&mut self, e: LiveEvent) {
        let add = |rows: &[LiveRow], sign: i64, m: &mut BTreeMap<String, i64>| {
            for r in rows {
                let k = show(&r.terms);
                let n = m.entry(k.clone()).or_insert(0);
                *n += sign * r.count as i64;
                if *n == 0 {
                    m.remove(&k);
                }
            }
        };
        match e {
            LiveEvent::Snapshot { commit, rows, .. } => {
                self.rows.clear();
                add(&rows, 1, &mut self.rows);
                self.commit = commit;
            }
            LiveEvent::Diff {
                commit,
                removed,
                added,
                ..
            } => {
                add(&removed, -1, &mut self.rows);
                add(&added, 1, &mut self.rows);
                self.commit = commit;
            }
            e => panic!("the live query ended: {e:?}"),
        }
    }

    /// Follow the events until the rows equal `want`.
    fn settle(&mut self, want: &BTreeMap<String, i64>) {
        let until = Instant::now() + WAIT;
        while &self.rows != want {
            let left = until.saturating_duration_since(Instant::now());
            match self.live.next_timeout(left) {
                Some(e) => self.apply(e),
                None => panic!(
                    "the live query has {:?} at commit {}, the query {want:?}",
                    self.rows, self.commit
                ),
            }
        }
    }
}

/// Every view and live query gives the one-off query's rows.
fn check(ds: &Dataset, views: &[(&str, &str)], live: &mut [(&str, Follower)]) {
    for (name, q) in views {
        assert_eq!(view(ds, name), oracle(ds, q), "view {name}");
    }
    for (q, f) in live.iter_mut() {
        f.settle(&oracle(ds, q));
    }
}

fn put(ds: &Dataset, name: &str, q: &str) {
    ds.views().put(name, ViewDef::table(q)).unwrap();
    ds.views().wait_built(name, WAIT).unwrap();
}

fn mode(ds: &Dataset, name: &str) -> (Option<Mode>, Option<String>) {
    let v = ds.views().get(name).unwrap();
    (v.evaluation, v.evaluation_reason)
}

/// With RDFS on read, views and live queries follow inserts, deletes and changes of
/// the schema as the one-off query does, and a commit outside their footprint skips
/// them.
#[test]
fn rdfs_on_read_views_follow_data_and_schema() {
    let ds = Dataset::memory();
    ds.load_str(
        &format!("<urn:Student> <{SUB}> <urn:Person> . <urn:a> a <urn:Student> ."),
        RdfFormat::Turtle,
    )
    .unwrap();
    ds.reasoning()
        .rdfs()
        .set(NewSchema::Graph("default".into()))
        .unwrap();
    put(&ds, "people", PEOPLE);
    put(&ds, "counts", COUNTS);
    assert_eq!(
        mode(&ds, "people"),
        (Some(Mode::Recompute), Some("RDFS on read".into()))
    );
    let views = [("people", PEOPLE), ("counts", COUNTS)];
    let mut live = [
        (PEOPLE, Follower::new(&ds, PEOPLE)),
        (COUNTS, Follower::new(&ds, COUNTS)),
    ];
    assert_eq!(view(&ds, "people").len(), 1);
    check(&ds, &views, &mut live);

    ds.update("INSERT DATA { <urn:b> a <urn:Student> }")
        .unwrap();
    check(&ds, &views, &mut live);
    // a class outside the hierarchy, then the schema change that puts it in
    ds.update("INSERT DATA { <urn:c> a <urn:Teacher> }")
        .unwrap();
    check(&ds, &views, &mut live);
    assert_eq!(view(&ds, "people").len(), 2);
    ds.update(&format!(
        "INSERT DATA {{ <urn:Teacher> <{SUB}> <urn:Person> }}"
    ))
    .unwrap();
    check(&ds, &views, &mut live);
    assert_eq!(view(&ds, "people").len(), 3);
    ds.update("DELETE DATA { <urn:a> a <urn:Student> }")
        .unwrap();
    check(&ds, &views, &mut live);
    ds.update(&format!(
        "DELETE DATA {{ <urn:Student> <{SUB}> <urn:Person> }}"
    ))
    .unwrap();
    check(&ds, &views, &mut live);
    assert_eq!(view(&ds, "people").len(), 1);

    // an unrelated commit skips the view of people
    let before = ds.views().get("people").unwrap();
    ds.update("INSERT DATA { <urn:z> <urn:knows> <urn:y> }")
        .unwrap();
    check(&ds, &views, &mut live);
    let after = ds.views().get("people").unwrap();
    assert_eq!(after.passes, before.passes, "{after:?}");
    assert!(after.skipped > before.skipped, "{after:?}");
}

/// With materialized inferences in the default graph, a change of the inferred graph
/// reaches views and live queries.
#[test]
fn materialized_inferences_reach_views() {
    let ds = Dataset::memory();
    ds.load_str("<urn:a> a <urn:Student> .", RdfFormat::Turtle)
        .unwrap();
    ds.load_str_into(
        "<urn:a> a <urn:Person> .",
        RdfFormat::Turtle,
        INFERRED_GRAPH,
    )
    .unwrap();
    ds.state()
        .set_reasoning(Some(ReasoningRecord {
            profile: "rdfs".into(),
            ..Default::default()
        }))
        .unwrap();
    put(&ds, "people", PEOPLE);
    put(&ds, "counts", COUNTS);
    assert_eq!(
        mode(&ds, "people"),
        (
            Some(Mode::Recompute),
            Some("materialized inferences".into())
        )
    );
    let views = [("people", PEOPLE), ("counts", COUNTS)];
    let mut live = [
        (PEOPLE, Follower::new(&ds, PEOPLE)),
        (COUNTS, Follower::new(&ds, COUNTS)),
    ];
    assert_eq!(view(&ds, "people").len(), 1);
    check(&ds, &views, &mut live);
    ds.update(&format!(
        "INSERT DATA {{ <urn:b> a <urn:Student> . GRAPH <{INFERRED_GRAPH}> {{ <urn:b> a <urn:Person> }} }}"
    ))
    .unwrap();
    check(&ds, &views, &mut live);
    assert_eq!(view(&ds, "people").len(), 2);
    ds.update(&format!(
        "DELETE DATA {{ GRAPH <{INFERRED_GRAPH}> {{ <urn:a> a <urn:Person> }} }}"
    ))
    .unwrap();
    check(&ds, &views, &mut live);
    assert_eq!(view(&ds, "people").len(), 1);
}

/// Turning RDFS on read and materialized inferences on and off keeps the views and the
/// live queries, which switch between incremental maintenance and evaluating again
/// with their rows kept correct.
#[test]
fn switching_inference_keeps_views() {
    let ds = Dataset::memory();
    ds.load_str(
        &format!(
            "<urn:Student> <{SUB}> <urn:Person> . <urn:a> a <urn:Student> . <urn:b> a <urn:Person> ."
        ),
        RdfFormat::Turtle,
    )
    .unwrap();
    ds.load_str_into(
        "<urn:c> a <urn:Person> .",
        RdfFormat::Turtle,
        INFERRED_GRAPH,
    )
    .unwrap();
    let avg = "SELECT (AVG(?n) AS ?m) WHERE { ?x <urn:n> ?n }";
    ds.update("INSERT DATA { <urn:a> <urn:n> 1 . <urn:b> <urn:n> 2 }")
        .unwrap();
    put(&ds, "people", PEOPLE);
    put(&ds, "counts", COUNTS);
    put(&ds, "avg", avg);
    assert_eq!(mode(&ds, "people"), (Some(Mode::Incremental), None));
    let views = [("people", PEOPLE), ("counts", COUNTS), ("avg", avg)];
    let mut live = [
        (PEOPLE, Follower::new(&ds, PEOPLE)),
        (COUNTS, Follower::new(&ds, COUNTS)),
    ];
    check(&ds, &views, &mut live);
    assert_eq!(view(&ds, "people").len(), 1);

    ds.reasoning()
        .rdfs()
        .set(NewSchema::Graph("default".into()))
        .unwrap();
    check(&ds, &views, &mut live);
    assert_eq!(view(&ds, "people").len(), 2);
    assert_eq!(mode(&ds, "people").0, Some(Mode::Recompute));
    ds.update("INSERT DATA { <urn:d> a <urn:Student> . <urn:d> <urn:n> 6 }")
        .unwrap();
    check(&ds, &views, &mut live);

    ds.state()
        .set_reasoning(Some(ReasoningRecord {
            profile: "rdfs".into(),
            ..Default::default()
        }))
        .unwrap();
    check(&ds, &views, &mut live);
    assert_eq!(view(&ds, "people").len(), 4);
    assert_eq!(
        mode(&ds, "people"),
        (
            Some(Mode::Recompute),
            Some("RDFS on read and materialized inferences".into())
        )
    );

    ds.reasoning().rdfs().reset().unwrap();
    ds.state().set_reasoning(None).unwrap();
    check(&ds, &views, &mut live);
    assert_eq!(view(&ds, "people").len(), 1);
    assert_eq!(mode(&ds, "people"), (Some(Mode::Incremental), None));
    ds.update("INSERT DATA { <urn:e> a <urn:Person> . <urn:e> <urn:n> 3 }")
        .unwrap();
    check(&ds, &views, &mut live);
    assert_eq!(ds.views().list().len(), 3);
    for v in ds.views().list() {
        assert!(v.commit.is_some(), "{v:?}");
    }
}

/// A view kept with RDFS on read is checkpointed and loaded again at open, and a view
/// whose checkpoint was written in the other mode is built again rather than loaded.
#[test]
fn inferred_views_survive_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let ds = Dataset::open(dir.path()).unwrap();
        ds.load_str(
            &format!("<urn:Student> <{SUB}> <urn:Person> . <urn:a> a <urn:Student> ."),
            RdfFormat::Turtle,
        )
        .unwrap();
        ds.reasoning()
            .rdfs()
            .set(NewSchema::Graph("default".into()))
            .unwrap();
        put(&ds, "people", PEOPLE);
        assert_eq!(view(&ds, "people"), oracle(&ds, PEOPLE));
    }
    {
        let ds = Dataset::open(dir.path()).unwrap();
        ds.views().wait_built("people", WAIT).unwrap();
        assert_eq!(mode(&ds, "people").0, Some(Mode::Recompute));
        assert_eq!(view(&ds, "people"), oracle(&ds, PEOPLE));
        ds.update("INSERT DATA { <urn:b> a <urn:Student> }")
            .unwrap();
        assert_eq!(view(&ds, "people"), oracle(&ds, PEOPLE));
        ds.reasoning().rdfs().reset().unwrap();
        assert_eq!(view(&ds, "people"), oracle(&ds, PEOPLE));
    }
    {
        let ds = Dataset::open(dir.path()).unwrap();
        ds.views().wait_built("people", WAIT).unwrap();
        assert_eq!(mode(&ds, "people").0, Some(Mode::Incremental));
        assert_eq!(view(&ds, "people"), oracle(&ds, PEOPLE));
        assert!(view(&ds, "people").is_empty());
    }
}
