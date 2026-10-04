//! The handles of `Dataset` (spec P06 §4.5 to §4.12).

use sparkles::Dataset;
use sparkles::handles::{CommitRef, HistoryUpdate};
use sparkles::history::{At, Retention, SnapshotOptions};
use sparkles::io::RdfFormat;
use sparkles::task::{Cancel, Control, Progress};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DATA: &str =
    "<http://ex.org/a> <http://ex.org/p> 1 .\n<http://ex.org/b> <http://ex.org/p> 2 .\n";

fn persistent() -> (tempfile::TempDir, Dataset) {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path().join("db")).unwrap();
    ds.load_str(DATA, RdfFormat::Turtle).unwrap();
    (dir, ds)
}

#[test]
fn snapshots_and_history() {
    let (_dir, ds) = persistent();
    let head = ds.head_commit().seq;
    let (s, created) = ds
        .snapshots()
        .create("before", &At::Head, &SnapshotOptions::default())
        .unwrap();
    assert!(created && s.seq == head);
    let (_, again) = ds
        .snapshots()
        .create("before", &At::Head, &SnapshotOptions::default())
        .unwrap();
    assert!(!again);
    assert_eq!(ds.snapshots().get("before").unwrap().seq, head);
    assert!(ds.snapshots().get("missing").is_none());
    assert_eq!(ds.snapshots().list().len(), 1);

    ds.update("INSERT DATA { <http://ex.org/c> <http://ex.org/p> 3 }")
        .unwrap();
    let h = ds.history();
    let c = h.commit(&CommitRef::Head).unwrap().unwrap();
    assert_eq!(c.commit.seq, head + 1);
    assert!(h.commit(&CommitRef::Seq(head + 5)).unwrap().is_none());
    assert_eq!("head".parse::<CommitRef>().unwrap(), CommitRef::Head);
    assert!("nonsense".parse::<CommitRef>().is_err());
    let d = h
        .diff(
            &At::Snapshot("before".into()),
            &At::Head,
            &Default::default(),
        )
        .unwrap();
    assert_eq!(serde_json::to_value(&c).unwrap()["seq"], head + 1);
    let _ = d;
    assert!(ds.snapshots().delete("before").unwrap());
    assert!(!ds.snapshots().delete("before").unwrap());

    // a commit made on another thread wakes the waiter
    let seq = ds.head_commit().seq;
    assert_eq!(h.wait_for_commit(seq, Duration::from_millis(20)), None);
    let ds2 = ds.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        ds2.update("INSERT DATA { <http://ex.org/d> <http://ex.org/p> 4 }")
            .unwrap();
    });
    assert_eq!(
        h.wait_for_commit(seq, Duration::from_secs(10)),
        Some(seq + 1)
    );
    t.join().unwrap();
}

#[test]
fn settings_have_get_set_and_reset() {
    let (_dir, ds) = persistent();
    let quota = ds.settings().quota();
    let base = quota.get();
    quota.set(1 << 20).unwrap();
    assert_ne!(
        serde_json::to_value(quota.get()).unwrap(),
        serde_json::to_value(&base).unwrap()
    );
    quota.reset().unwrap();
    assert_eq!(
        serde_json::to_value(quota.get()).unwrap(),
        serde_json::to_value(&base).unwrap()
    );

    let retention = ds.settings().retention();
    retention
        .set(HistoryUpdate {
            retention: Some(Retention {
                keep_commits: Some(5),
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(retention.get().retention.keep_commits, Some(5));
    retention.reset().unwrap();
    assert_eq!(retention.get().retention.keep_commits, None);

    let describe = ds.settings().describe();
    describe.set(describe.get()).unwrap();
    describe.reset().unwrap();
    let compaction = ds.settings().compaction();
    compaction.set(compaction.get()).unwrap();
    compaction.reset().unwrap();
}

#[test]
fn compaction_reports_progress_and_cancels() {
    let (_dir, ds) = persistent();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let ctl = Control {
        progress: Progress::new(move |f, _| s2.lock().unwrap().push(f)),
        ..Control::none()
    };
    ds.compact_with(&Default::default(), &ctl).unwrap();
    let seen = seen.lock().unwrap().clone();
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
    assert_eq!(seen.last(), Some(&1.0));

    let cancel = Cancel::new();
    cancel.cancel();
    let ctl = Control {
        cancel,
        ..Control::none()
    };
    let before = ds.len();
    let e = ds.compact_with(&Default::default(), &ctl).unwrap_err();
    assert_eq!(e.code(), "cancelled");
    assert_eq!(ds.len(), before);
}

#[test]
fn stored_queries_run_with_parameters() {
    let ds = Dataset::memory();
    ds.load_str(DATA, RdfFormat::Turtle).unwrap();
    let def: sparkles::stored::Definition = serde_json::from_value(serde_json::json!({
        "query": "SELECT ?v WHERE { ?s <http://ex.org/p> ?v }",
        "parameters": {"s": {"type": "iri", "required": true}},
    }))
    .unwrap();
    ds.queries().put("by-s", def, Default::default()).unwrap();
    assert_eq!(ds.queries().list().len(), 1);
    let mut params = BTreeMap::new();
    params.insert("s".to_string(), serde_json::json!("http://ex.org/b"));
    let r = ds
        .queries()
        .run("by-s", &params, &Default::default())
        .unwrap();
    assert_eq!(r.table.len(), 1);
    let e = ds
        .queries()
        .run("by-s", &BTreeMap::new(), &Default::default())
        .err()
        .unwrap();
    assert_eq!(e.code(), "invalid");
    let e = ds
        .queries()
        .run("missing", &params, &Default::default())
        .err()
        .unwrap();
    assert_eq!(e.code(), "not-found");
    assert!(ds.queries().delete("by-s", None).unwrap());
}

#[test]
fn rdfs_setting_and_dataset_calls() {
    let (_dir, ds) = persistent();
    let rdfs = ds.reasoning().rdfs();
    assert!(rdfs.get().is_none());
    rdfs.set(sparkles::reasoning::rdfs::NewSchema::Graph(
        "default".into(),
    ))
    .unwrap();
    assert!(rdfs.get().is_some());
    rdfs.reset().unwrap();
    assert!(rdfs.get().is_none());

    ds.set_prefix("ex", "http://ex.org/").unwrap();
    assert!(ds.remove_prefix("ex").unwrap());
    assert!(!ds.remove_prefix("ex").unwrap());
    let (plan, _) = ds
        .explain("SELECT * WHERE { ?s ?p ?o }", &Default::default())
        .unwrap();
    assert!(!plan.is_empty());
    let mut out = Vec::new();
    assert_eq!(
        ds.dump_graph(
            oxrdf::GraphNameRef::DefaultGraph,
            &mut out,
            RdfFormat::NTriples
        )
        .unwrap(),
        2
    );
    ds.clear_cache();
    let r = ds
        .replace(
            sparkles::store::ReplaceTarget::Default,
            &[sparkles::io::Source::from_bytes(
                b"<http://ex.org/z> <http://ex.org/p> 9 .".to_vec(),
                RdfFormat::Turtle,
                None,
            )],
        )
        .unwrap();
    assert!(r.committed);
    assert_eq!(ds.len(), 1);
    assert_eq!(ds.clear().unwrap(), 1);
    assert!(ds.is_empty());
    assert!(ds.validation().guard().get().is_none());
}

#[cfg(feature = "backup")]
#[test]
fn backups_in_a_file_system_repository() {
    use sparkles::backup::{OpenEnv, RepoConfig};
    let (dir, ds) = persistent();
    let url = format!("file://{}", dir.path().join("repo").display());
    let cfg = RepoConfig::from_url("local", &url).unwrap();
    let repo = sparkles::backup::open(&cfg, &OpenEnv::default()).unwrap();
    sparkles::backup::blocking(&repo).test().unwrap();
    let backups = ds.backups(&repo);
    assert!(backups.list(&Default::default()).unwrap().is_empty());
    assert!(backups.get("missing").unwrap().is_none());
    assert!(!backups.delete("missing").unwrap());
    assert!(
        sparkles::backup::blocking(&repo)
            .locks()
            .unwrap()
            .is_empty()
    );
}

#[cfg(feature = "text")]
#[test]
fn text_search_marks_the_matches() {
    use sparkles::handles::TextSearch;
    let ds = Dataset::memory();
    ds.load_str(
        "<http://ex.org/a> <http://www.w3.org/2000/01/rdf-schema#label> \"red <fox> jumps\" .
         <http://ex.org/b> <http://www.w3.org/2000/01/rdf-schema#label> \"blue whale\" .",
        RdfFormat::Turtle,
    )
    .unwrap();
    let text = ds.indexes().text();
    assert!(!text.enabled());
    text.enable(Default::default()).unwrap();
    assert!(text.enabled());
    let hits = text
        .search(&TextSearch {
            query: "fox".into(),
            ..Default::default()
        })
        .unwrap();
    assert!(!hits.limited);
    assert_eq!(hits.hits.len(), 1);
    let h = &hits.hits[0];
    assert_eq!(h.subject.as_ref().unwrap().to_string(), "<http://ex.org/a>");
    assert_eq!(h.literal.as_ref().unwrap().value(), "red <fox> jumps");
    let snippet = h.snippet.as_deref().unwrap();
    assert!(
        snippet.contains("<mark>") && snippet.contains("&lt;"),
        "{snippet}"
    );
    let j = serde_json::to_value(&hits).unwrap();
    assert_eq!(j["hits"][0]["s"]["value"], "http://ex.org/a");
    let plain = text
        .search(&TextSearch {
            query: "whale".into(),
            highlight: false,
            limit: 1,
            ..Default::default()
        })
        .unwrap();
    assert!(plain.limited && plain.hits[0].snippet.is_none());
}

#[test]
fn schema_reports_are_kept_paged_and_compared() {
    use sparkles::handles::{Computed, ReportRequest};
    let (_dir, ds) = persistent();
    ds.update(
        "INSERT DATA { <http://ex.org/a> a <http://ex.org/A> . <http://ex.org/b> a <http://ex.org/B> . <http://ex.org/c> a <http://ex.org/C> }",
    )
    .unwrap();
    let schema = ds.schema();
    let first = schema
        .report(&ReportRequest {
            limit: 2,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(first.computed, Computed::Full);
    let page = first.classes();
    assert_eq!((page.items.len(), page.total), (2, 3));
    let next = page.next.clone().unwrap();
    assert_eq!(
        serde_json::to_value(first.summary("ds")).unwrap()["dataset"],
        "ds"
    );
    // the next page reads the kept report
    let second = schema
        .report(&ReportRequest {
            limit: 2,
            cursor: Some(next.clone()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(second.computed, Computed::Cached);
    let rest = second.classes();
    assert_eq!(rest.items.len(), 1);
    assert!(rest.next.is_none());
    // a cursor of another selection, and a malformed one, are refused
    let other = ReportRequest {
        cursor: Some(next.clone()),
        options: sparkles::schema::SchemaOptions {
            subject_classes: true,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(matches!(
        schema.report(&other),
        Err(sparkles::Error::Invalid(_))
    ));
    let bad = ReportRequest {
        cursor: Some("nonsense".into()),
        ..Default::default()
    };
    assert!(matches!(
        schema.report(&bad),
        Err(sparkles::Error::Invalid(_))
    ));
    // after a write, a report brought up to date from the changes
    ds.update("INSERT DATA { <http://ex.org/d> a <http://ex.org/D> }")
        .unwrap();
    let later = schema.report(&ReportRequest::default()).unwrap();
    assert!(
        matches!(later.computed, Computed::Updated(_)),
        "{:?}",
        later.computed
    );
    assert_eq!(later.report.classes.len(), 4);
    // the old cursor's report is gone
    let stale = ReportRequest {
        limit: 2,
        cursor: Some(next),
        ..Default::default()
    };
    assert!(matches!(
        schema.report(&stale),
        Err(sparkles::Error::Conflict(_))
    ));
    let (diff, _) = schema
        .diff(
            &At::Commit(ds.head_commit().seq - 1),
            &ReportRequest::default(),
        )
        .unwrap();
    assert_eq!(diff.classes.added.len(), 1);
    // a missing graph keeps the schema error as the source
    let missing = ReportRequest {
        options: sparkles::schema::SchemaOptions {
            graph: sparkles::schema::GraphSelection::parse("http://ex.org/none").unwrap(),
            ..Default::default()
        },
        ..Default::default()
    };
    let e = schema.report(&missing).unwrap_err();
    assert_eq!(e.code(), "no-such-graph");
    assert!(sparkles::handles::schema_error_of(&e).is_some());
    let layer = schema.constraints(&Default::default()).unwrap();
    assert!(layer.is_empty());
}
