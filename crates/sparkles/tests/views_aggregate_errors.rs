//! Views and live queries under the dataset's reading of errors in aggregates (roadmap
//! item 1064). They run with the dataset's reading, a change of the setting evaluates
//! them again, a checkpoint of another reading is not loaded, and a query whose own
//! reading differs evaluates the view's query instead of reading the view's state.

use oxrdf::Term;
use sparkles::Dataset;
use sparkles::incr::{LiveEvent, LiveOptions, LiveQuery, LiveRow, ViewDef};
use sparkles::io::RdfFormat;
use sparkles::sparql::{AggregateErrors, QueryOptions};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const AGGS: &str = "SELECT ?g (MIN(?v) AS ?min) (MAX(?v) AS ?max) (AVG(?v) AS ?avg) \
    (GROUP_CONCAT(?v) AS ?all) \
    WHERE { ?s <urn:g> ?g OPTIONAL { ?s <urn:v> ?v } } GROUP BY ?g";
const WAIT: Duration = Duration::from_secs(20);

fn show(row: &[Option<Term>]) -> String {
    row.iter()
        .map(|t| t.as_ref().map_or("UNDEF".to_string(), |t| t.to_string()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn rows(r: sparkles::sparql::QueryResult) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for r in r.rows().unwrap() {
        *m.entry(show(r.values())).or_insert(0) += 1;
    }
    m
}

/// The rows of the one-off query with the dataset's query defaults.
fn oracle(ds: &Dataset, q: &str) -> BTreeMap<String, i64> {
    rows(ds.query(q).unwrap())
}

/// The rows of the one-off query under `m`.
fn under(ds: &Dataset, q: &str, m: AggregateErrors) -> BTreeMap<String, i64> {
    let opts = QueryOptions {
        aggregate_errors: Some(m),
        ..Default::default()
    };
    rows(ds.query_with(q, &opts).unwrap())
}

fn view(ds: &Dataset, name: &str) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for (r, w) in ds.views().read(name).unwrap().rows {
        *m.entry(show(&r)).or_insert(0) += w;
    }
    m
}

/// The view's rows once they equal the one-off query's, which a change of the setting
/// reaches on the views' next round.
fn settle_view(ds: &Dataset, name: &str, q: &str) {
    let until = Instant::now() + WAIT;
    loop {
        let (v, o) = (view(ds, name), oracle(ds, q));
        if v == o {
            return;
        }
        assert!(
            Instant::now() < until,
            "view {name} has {v:?}, the query {o:?}"
        );
        ds.views().settle(Duration::from_millis(50));
    }
}

struct Follower {
    live: LiveQuery,
    rows: BTreeMap<String, i64>,
}

impl Follower {
    fn new(ds: &Dataset, q: &str) -> Follower {
        let live = ds.live(q, &LiveOptions::default()).unwrap();
        let mut f = Follower {
            live,
            rows: BTreeMap::new(),
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
            LiveEvent::Snapshot { rows, .. } => {
                self.rows.clear();
                add(&rows, 1, &mut self.rows);
            }
            LiveEvent::Diff { removed, added, .. } => {
                add(&removed, -1, &mut self.rows);
                add(&added, 1, &mut self.rows);
            }
            e => panic!("the live query ended: {e:?}"),
        }
    }

    fn settle(&mut self, want: &BTreeMap<String, i64>) {
        let until = Instant::now() + WAIT;
        while &self.rows != want {
            let left = until.saturating_duration_since(Instant::now());
            match self.live.next_timeout(left) {
                Some(e) => self.apply(e),
                None => panic!("the live query has {:?}, the query {want:?}", self.rows),
            }
        }
    }
}

const DATA: &str = "<urn:a> <urn:g> \"a\" ; <urn:v> 1 . <urn:b> <urn:g> \"a\" ; <urn:v> 3 . \
    <urn:c> <urn:g> \"a\" . <urn:d> <urn:g> \"b\" ; <urn:v> 2 .";

/// A view and a live query follow inserts and deletes of values and of unbound rows
/// under each reading, and a change of the dataset's reading evaluates them again.
#[test]
fn views_follow_the_dataset_reading() {
    let ds = Dataset::memory();
    ds.load_str(DATA, RdfFormat::Turtle).unwrap();
    ds.views().put("aggs", ViewDef::table(AGGS)).unwrap();
    ds.views().wait_built("aggs", WAIT).unwrap();
    let mut live = Follower::new(&ds, AGGS);
    for m in [
        AggregateErrors::Jena,
        AggregateErrors::Sparkles,
        AggregateErrors::Spec,
        AggregateErrors::Jena,
    ] {
        ds.settings().aggregate_errors().set(m).unwrap();
        assert_eq!(oracle(&ds, AGGS), under(&ds, AGGS, m), "{m}");
        settle_view(&ds, "aggs", AGGS);
        live.settle(&oracle(&ds, AGGS));
        // the unbound row goes and comes back, and the group "b" gets one
        for u in [
            "DELETE DATA { <urn:c> <urn:g> \"a\" }",
            "INSERT DATA { <urn:e> <urn:g> \"b\" }",
            "INSERT DATA { <urn:c> <urn:g> \"a\" }",
            "DELETE DATA { <urn:e> <urn:g> \"b\" }",
            "INSERT DATA { <urn:f> <urn:g> \"c\" }",
            "DELETE DATA { <urn:f> <urn:g> \"c\" }",
        ] {
            ds.update(u).unwrap();
            settle_view(&ds, "aggs", AGGS);
            live.settle(&oracle(&ds, AGGS));
        }
    }
    // the readings differ on these rows, so the switches above were seen
    assert_ne!(
        under(&ds, AGGS, AggregateErrors::Jena),
        under(&ds, AGGS, AggregateErrors::Sparkles)
    );
}

/// A query under another reading than the view's evaluates the view's query at its
/// snapshot, notes it in the plan and leaves the view's state as it was.
#[test]
fn a_read_under_another_reading_evaluates_the_view_query() {
    let ds = Dataset::memory();
    ds.load_str(DATA, RdfFormat::Turtle).unwrap();
    ds.views().put("aggs", ViewDef::table(AGGS)).unwrap();
    ds.views().wait_built("aggs", WAIT).unwrap();
    let read = "PREFIX view: <urn:x-sparkles:view#> \
        SELECT ?g ?min ?max ?avg ?all WHERE { SERVICE view:read { \
        [] view:name \"aggs\" ; view:bind ?g, ?min, ?max, ?avg, ?all } }";
    let jena = view(&ds, "aggs");
    assert_eq!(jena, under(&ds, AGGS, AggregateErrors::Jena));
    for m in [AggregateErrors::Sparkles, AggregateErrors::Spec] {
        let opts = QueryOptions {
            aggregate_errors: Some(m),
            ..Default::default()
        };
        // EXPLAIN notes it without running the query
        let (_, plan) = ds.explain(read, &opts).unwrap();
        let noted = plan.warnings.iter().any(|w| w.code == "view-evaluated");
        assert!(noted, "{m}: EXPLAIN {:?}", plan.warnings);
        let r = ds.query_with(read, &opts).unwrap();
        let warned = r
            .plan()
            .warnings
            .iter()
            .filter(|w| w.code == "view-evaluated");
        assert_eq!(warned.count(), 1, "{m}: {:?}", r.plan().warnings);
        assert_eq!(rows(r), under(&ds, AGGS, m), "{m}");
        assert_eq!(
            view(&ds, "aggs"),
            jena,
            "{m}: the view's state is unchanged"
        );
    }
    // the dataset's own reading reads the view
    let (_, plan) = ds.explain(read, &QueryOptions::default()).unwrap();
    assert!(plan.warnings.iter().all(|w| w.code != "view-evaluated"));
    let r = ds.query(read).unwrap();
    assert!(r.plan().warnings.iter().all(|w| w.code != "view-evaluated"));
    assert_eq!(rows(r), jena);
}

/// A view's checkpoint keeps the reading it was built under: reopened under another
/// reading, the view is built again from the data instead of loading it.
#[test]
fn a_checkpoint_of_another_reading_is_not_loaded() {
    let dir = tempfile::tempdir().unwrap();
    {
        let ds = Dataset::open(dir.path()).unwrap();
        ds.load_str(DATA, RdfFormat::Turtle).unwrap();
        ds.views().put("aggs", ViewDef::table(AGGS)).unwrap();
        ds.views().wait_built("aggs", WAIT).unwrap();
        assert_eq!(view(&ds, "aggs"), under(&ds, AGGS, AggregateErrors::Jena));
        // a clean close checkpoints the view
    }
    // the setting changes while the dataset is closed
    std::fs::write(
        dir.path().join(sparkles::store::AGGREGATE_ERRORS_FILE),
        r#"{"format": 1, "aggregateErrors": "sparkles"}"#,
    )
    .unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    assert_eq!(
        ds.settings().aggregate_errors().get(),
        AggregateErrors::Sparkles
    );
    ds.views().wait_built("aggs", WAIT).unwrap();
    assert_eq!(
        view(&ds, "aggs"),
        under(&ds, AGGS, AggregateErrors::Sparkles)
    );
    assert_ne!(
        under(&ds, AGGS, AggregateErrors::Sparkles),
        under(&ds, AGGS, AggregateErrors::Jena)
    );
}
