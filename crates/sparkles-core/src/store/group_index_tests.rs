//! Group commit on datasets with a full-text, spatial or embedding index (X09 section
//! 17, Phase 4): admission and its fallback reasons, the order of index work around the
//! fence, installers that finish while a fence runs, and crashes between the fence and
//! the publication of each index kind.
#![cfg_attr(
    not(all(feature = "text", feature = "geo")),
    allow(dead_code, unused_imports)
)]
use super::*;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const GEO: &str = "http://www.opengis.net/ont/geosparql#";

fn options() -> StoreOptions {
    StoreOptions {
        group_commit: true,
        group_commit_idle_direct: false,
        ..Default::default()
    }
}

fn update(s: &Store, u: &str) -> Result<()> {
    crate::sparql::update::update(s, u, &Default::default()).map(|_| ())
}

fn label(s: &Store, subject: &str, text: &str) -> Result<()> {
    update(
        s,
        &format!("INSERT DATA {{ <urn:{subject}> <{LABEL}> \"{text}\" }}"),
    )
}

fn point(s: &Store, name: &str, x: f64, y: f64) -> Result<()> {
    update(
        s,
        &format!(
            "INSERT DATA {{ <urn:{name}> <{GEO}asWKT> \"POINT({x} {y})\"^^<{GEO}wktLiteral> }}"
        ),
    )
}

/// Pause the first thread that reaches failpoint `name` until the returned sender is used.
fn pause(s: &Store, name: &'static str) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (hit, observed) = mpsc::channel();
    let (resume, go) = mpsc::channel();
    let go = Mutex::new(go);
    let once = AtomicBool::new(false);
    s.set_failpoint(
        name,
        Some(Arc::new(move |_| {
            if !once.swap(true, Ordering::Relaxed) {
                hit.send(()).unwrap();
                go.lock()
                    .recv_timeout(Duration::from_secs(20))
                    .expect("release the paused fence");
            }
        })),
    );
    (observed, resume)
}

fn grouped(s: &Store) -> u64 {
    s.commit_metrics().sync[SyncPath::Group as usize].count()
}

fn fell_back(s: &Store, why: Fallback) -> u64 {
    s.commit_metrics().fallbacks[why as usize]
}

/// The subjects a full-text search for `word` finds in `snap`.
#[cfg(feature = "text")]
fn text_hits(snap: Arc<Snapshot>, word: &str) -> Result<BTreeSet<String>> {
    let q = format!(
        "PREFIX text:<http://jena.apache.org/text#> SELECT ?s {{ ?s text:query \"{word}\" }}"
    );
    let r = crate::sparql::query(snap, &q, &Default::default())?;
    Ok(r.rows()
        .into_iter()
        .filter_map(|row| match &row[0] {
            Some(oxrdf::Term::NamedNode(n)) => Some(n.as_str().to_string()),
            _ => None,
        })
        .collect())
}

/// The commit the full-text index's durable checkpoint names, read from its metadata.
#[cfg(feature = "text")]
fn text_payload_seq(root: &Path) -> Option<u64> {
    let meta = std::fs::read(root.join("text/meta.json")).ok()?;
    let meta: serde_json::Value = serde_json::from_slice(&meta).ok()?;
    let payload: serde_json::Value = serde_json::from_str(meta["payload"].as_str()?).ok()?;
    payload["seq"].as_u64()
}

/// Subjects of the points inside `w` = [min x, min y, max x, max y], by the spatial index,
/// and whether the search had to scan instead.
#[cfg(feature = "geo")]
fn window(snap: Arc<Snapshot>, w: [f64; 4]) -> (BTreeSet<String>, bool) {
    let ctx = crate::sparql::ctx::Ctx::new(snap.clone());
    let mut st = crate::geo::search::SearchStats::default();
    let preds: Vec<Id> = snap
        .lookup_iri(&format!("{GEO}asWKT"))
        .into_iter()
        .collect();
    let mut out = BTreeSet::new();
    crate::geo::search::window(
        &ctx,
        &preds,
        &[w],
        &crate::sparql::plan::GraphFilter::All,
        &mut st,
        &mut |hits| {
            for h in hits {
                out.insert(snap.term(h.s).unwrap().to_string());
            }
            Ok(())
        },
    )
    .unwrap();
    (out, st.fallback)
}

#[cfg(feature = "text")]
#[test]
fn text_changes_reach_the_live_index_only_after_their_fence() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(Store::open(dir.path(), options()).unwrap());
    s.enable_text(Default::default()).unwrap();
    label(&s, "a", "alpha").unwrap();
    let ti = s.text.load_full().unwrap();
    let (observed, resume) = pause(&s, "group-synced");
    let writer = {
        let s = s.clone();
        thread::spawn(move || label(&s, "b", "bravo"))
    };
    observed.recv_timeout(Duration::from_secs(10)).unwrap();
    // The commit is synchronized and not yet published. The live index has not seen it,
    // and a checkpoint taken now names no commit above the published, durable head.
    let head = s.snapshot().commit;
    assert_eq!(ti.applied(), head);
    ti.checkpoint().unwrap();
    assert!(text_payload_seq(dir.path()).unwrap() <= head);
    assert!(text_hits(s.snapshot(), "bravo").unwrap().is_empty());
    resume.send(()).unwrap();
    writer.join().unwrap().unwrap();
    assert_eq!(
        text_hits(s.snapshot(), "bravo").unwrap(),
        BTreeSet::from(["urn:b".to_string()])
    );
    assert_eq!(ti.applied(), s.snapshot().commit);
    assert!(grouped(&s) >= 2);
    assert_eq!(s.commit_metrics().fallbacks.iter().sum::<u64>(), 0);
}

/// A transaction that starts from the speculative head and searches before its first
/// change waits until that head is published, and then sees exactly its state.
#[cfg(feature = "text")]
#[test]
fn a_search_from_a_speculative_head_waits_for_its_publication() {
    for fail in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        s.enable_text(Default::default()).unwrap();
        label(&s, "a", "alpha").unwrap();
        let (observed, resume) = pause(&s, "group-sealed");
        if fail {
            s.fail_next_group_sync();
        }
        let first = {
            let s = s.clone();
            thread::spawn(move || label(&s, "b", "bravo"))
        };
        observed.recv_timeout(Duration::from_secs(10)).unwrap();
        let (done, finished) = mpsc::channel();
        let second = {
            let s = s.clone();
            thread::spawn(move || {
                let r = update(
                    &s,
                    "PREFIX text:<http://jena.apache.org/text#> \
                     INSERT { ?s <urn:seen> true } WHERE { ?s text:query \"bravo\" }",
                );
                done.send(()).unwrap();
                r
            })
        };
        // the search holds the writer and waits for the fence of its base
        assert!(finished.recv_timeout(Duration::from_millis(200)).is_err());
        resume.send(()).unwrap();
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        if fail {
            assert!(matches!(first, Err(Error::Poisoned)), "{first:?}");
            assert!(second.is_err(), "{second:?}");
            continue;
        }
        first.unwrap();
        second.unwrap();
        let snap = s.snapshot();
        let seen = |n: &str| {
            let (Some(a), Some(p)) = (snap.lookup_iri(n), snap.lookup_iri("urn:seen")) else {
                return false;
            };
            let t = snap
                .lookup_term(&oxrdf::Literal::from(true).into())
                .unwrap();
            snap.contains(&[a, p, t, Id::DEFAULT_GRAPH]).unwrap()
        };
        assert!(seen("urn:b") && !seen("urn:a"));
    }
}

#[cfg(feature = "text")]
#[test]
fn a_failed_text_update_at_publication_leaves_the_commit_and_marks_the_index_stale() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), options()).unwrap();
    s.enable_text(Default::default()).unwrap();
    label(&s, "a", "alpha").unwrap();
    s.fail_next_text_commit();
    label(&s, "b", "bravo").unwrap();
    assert_eq!(s.snapshot().commit, 2);
    assert_eq!(s.text_status().unwrap().state, "stale");
    assert!(text_hits(s.snapshot(), "bravo").is_err());
    // the index stays stale for later grouped commits, which keep their receipts
    label(&s, "c", "charlie").unwrap();
    assert_eq!(s.text_status().unwrap().state, "stale");
    drop(s);
    // the next open catches the index up from the log or rebuilds it
    let s = Store::open(dir.path(), options()).unwrap();
    if let Some(job) = s.text_recovery.load_full() {
        job.wait().unwrap();
    }
    for (word, subject) in [("alpha", "urn:a"), ("bravo", "urn:b"), ("charlie", "urn:c")] {
        assert_eq!(
            text_hits(s.snapshot(), word).unwrap(),
            BTreeSet::from([subject.to_string()]),
            "{word}"
        );
    }
}

#[cfg(feature = "text")]
#[test]
fn an_online_text_rebuild_keeps_commits_out_of_groups_until_it_installs() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), options()).unwrap();
    s.enable_text(Default::default()).unwrap();
    label(&s, "a", "alpha").unwrap();
    let before = grouped(&s);
    let ti = s.text.load_full().unwrap();
    {
        let guard = ti.lock_rebuild();
        {
            let _w = s.guarded_writer();
            ti.start_journal(&guard);
        }
        label(&s, "b", "bravo").unwrap();
        assert_eq!(fell_back(&s, Fallback::TextRebuilding), 1);
        assert_eq!(grouped(&s), before);
    }
    label(&s, "c", "charlie").unwrap();
    assert_eq!(fell_back(&s, Fallback::TextRebuilding), 1);
    assert_eq!(grouped(&s), before + 1);
    // the rebuild itself still covers the commit made during it
    s.rebuild_text().unwrap();
    assert_eq!(
        text_hits(s.snapshot(), "bravo").unwrap(),
        BTreeSet::from(["urn:b".to_string()])
    );
}

#[cfg(feature = "geo")]
#[test]
fn spatial_views_ride_in_speculative_snapshots_of_concurrent_groups() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(Store::open(dir.path(), options()).unwrap());
    point(&s, "seed", 0.0, 0.0).unwrap();
    s.enable_geo(Default::default()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    // every published snapshot carries a ready view of its own commit
    let reader = {
        let (s, stop) = (s.clone(), stop.clone());
        thread::spawn(move || {
            let mut n = 0;
            while !stop.load(Ordering::Relaxed) {
                let snap = s.snapshot();
                let v = snap.geo.as_ref().expect("a view");
                assert_eq!(v.commit, snap.commit);
                assert!(matches!(v.state(), crate::geo::IndexState::Ready));
                n += 1;
            }
            n
        })
    };
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let s = s.clone();
            thread::spawn(move || {
                for k in 0..40 {
                    point(
                        &s,
                        &format!("p{t}-{k}"),
                        t as f64 * 10.0 + k as f64 * 0.1,
                        k as f64,
                    )
                    .unwrap();
                }
            })
        })
        .collect();
    for h in threads {
        h.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    assert!(reader.join().unwrap() > 0);
    let m = s.commit_metrics();
    assert_eq!(m.fallbacks.iter().sum::<u64>(), 0, "{:?}", m.fallbacks);
    assert!(m.sync[SyncPath::Group as usize].count() > 0);
    let snap = s.snapshot();
    for (t, w) in [
        (0usize, [-1.0, -1.0, 4.0, 50.0]),
        (3, [29.5, 9.5, 34.0, 20.5]),
    ] {
        let (hits, scanned) = window(snap.clone(), w);
        assert!(!scanned);
        let expected: BTreeSet<String> = (0..8)
            .flat_map(|t| (0..40).map(move |k| (t, k)))
            .filter(|&(t, k)| {
                let (x, y) = (t as f64 * 10.0 + k as f64 * 0.1, k as f64);
                x >= w[0] && y >= w[1] && x <= w[2] && y <= w[3]
            })
            .map(|(t, k)| format!("<urn:p{t}-{k}>"))
            .chain((t == 0).then(|| "<urn:seed>".to_string()))
            .collect();
        assert_eq!(hits, expected, "{w:?}");
    }
}

#[cfg(feature = "geo")]
#[test]
fn a_spatial_build_installs_its_view_only_after_the_fence() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(Store::open(dir.path(), options()).unwrap());
    point(&s, "seed", 0.0, 0.0).unwrap();
    s.enable_geo(Default::default()).unwrap();
    let epoch = s.snapshot().geo.as_ref().unwrap().epoch;
    // a rebuild of a ready index keeps serving it, so commits keep grouping meanwhile
    s.pause_geo_build(true);
    s.rebuild_geo().unwrap();
    let (observed, resume) = pause(&s, "group-synced");
    let writer = {
        let s = s.clone();
        thread::spawn(move || point(&s, "late", 1.0, 1.0))
    };
    observed.recv_timeout(Duration::from_secs(10)).unwrap();
    s.pause_geo_build(false);
    // the build waits for the fence rather than install under a commit in flight
    thread::sleep(Duration::from_millis(200));
    assert_eq!(s.snapshot().geo.as_ref().unwrap().epoch, epoch);
    resume.send(()).unwrap();
    writer.join().unwrap().unwrap();
    s.wait_geo();
    let until = Instant::now() + Duration::from_secs(10);
    while s.snapshot().geo.as_ref().unwrap().epoch == epoch {
        assert!(Instant::now() < until, "the build never installed");
        thread::sleep(Duration::from_millis(5));
    }
    let snap = s.snapshot();
    let v = snap.geo.as_ref().unwrap();
    assert_eq!((v.epoch, v.commit), (epoch + 1, snap.commit));
    let (hits, scanned) = window(snap.clone(), [0.5, 0.5, 1.5, 1.5]);
    assert!(!scanned);
    assert_eq!(hits, BTreeSet::from(["<urn:late>".to_string()]));
    // a commit after the install starts from the installed view
    point(&s, "later", 2.0, 2.0).unwrap();
    let snap = s.snapshot();
    assert_eq!(snap.geo.as_ref().unwrap().epoch, epoch + 1);
    assert_eq!(window(snap, [1.5, 1.5, 2.5, 2.5]).0.len(), 1);
}

#[cfg(feature = "geo")]
#[test]
fn a_building_or_failed_spatial_index_keeps_commits_out_of_groups() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), options()).unwrap();
    point(&s, "seed", 0.0, 0.0).unwrap();
    s.pause_geo_build(true);
    s.enable_geo(Default::default()).unwrap();
    let before = grouped(&s);
    point(&s, "during", 1.0, 1.0).unwrap();
    assert_eq!(fell_back(&s, Fallback::GeoBuilding), 1);
    assert_eq!(grouped(&s), before);
    s.pause_geo_build(false);
    s.wait_geo();
    point(&s, "after", 2.0, 2.0).unwrap();
    assert_eq!(grouped(&s), before + 1);
    let (hits, _) = window(s.snapshot(), [-1.0, -1.0, 3.0, 3.0]);
    assert_eq!(hits.len(), 3, "{hits:?}");
    // a failed update of the view in a grouped commit leaves the commit and fails the view
    s.fail_next_geo_commit();
    point(&s, "failing", 3.0, 3.0).unwrap();
    assert!(matches!(
        s.snapshot().geo.as_ref().unwrap().state(),
        crate::geo::IndexState::Failed
    ));
    point(&s, "while-failed", 4.0, 4.0).unwrap();
    assert_eq!(fell_back(&s, Fallback::GeoBuilding), 2);
    s.rebuild_geo().unwrap();
    point(&s, "rebuilt", 5.0, 5.0).unwrap();
    assert_eq!(fell_back(&s, Fallback::GeoBuilding), 2);
    let (hits, scanned) = window(s.snapshot(), [-1.0, -1.0, 6.0, 6.0]);
    assert!(!scanned);
    assert_eq!(hits.len(), 6, "{hits:?}");
}

/// Embedding work is scheduled when a commit is published, never for a commit above the
/// published head.
#[test]
fn embedding_work_is_scheduled_only_at_publication() {
    use crate::vector::VectorIndexConfig;
    use crate::vector::embed::EmbeddingConfig;
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(Store::open(dir.path(), options()).unwrap());
    let mut c = VectorIndexConfig::new("http://example.org/emb", 8);
    c.embedding =
        Some(EmbeddingConfig::new("http://127.0.0.1:9/v1", "mock-model").from_predicates(&[LABEL]));
    s.create_vector_index("docs", c).unwrap();
    let queued_seqs = |s: &Store| -> Vec<u64> {
        s.embed
            .works
            .lock()
            .values()
            .flat_map(|w| w.queue.iter().map(|(_, seq)| *seq).collect::<Vec<_>>())
            .collect()
    };
    let (observed, resume) = pause(&s, "group-synced");
    let writer = {
        let s = s.clone();
        thread::spawn(move || label(&s, "a", "alpha"))
    };
    observed.recv_timeout(Duration::from_secs(10)).unwrap();
    let head = s.snapshot().commit;
    assert!(
        queued_seqs(&s).iter().all(|&q| q <= head),
        "{:?}",
        queued_seqs(&s)
    );
    resume.send(()).unwrap();
    writer.join().unwrap().unwrap();
    let head = s.snapshot().commit;
    assert!(queued_seqs(&s).contains(&head), "{:?}", queued_seqs(&s));
    assert_eq!(s.commit_metrics().fallbacks.iter().sum::<u64>(), 0);
    // many writers: no queued pair ever names a commit above the published head
    let stop = Arc::new(AtomicBool::new(false));
    let checker = {
        let (s, stop) = (s.clone(), stop.clone());
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let works = s.embed.works.lock();
                let head = s.current.load().commit;
                for w in works.values() {
                    for (_, seq) in &w.queue {
                        assert!(*seq <= head, "pair of {seq} above head {head}");
                    }
                }
                drop(works);
                thread::yield_now();
            }
        })
    };
    let threads: Vec<_> = (0..6)
        .map(|t| {
            let s = s.clone();
            thread::spawn(move || {
                for k in 0..30 {
                    label(&s, &format!("e{t}-{k}"), &format!("text {t} {k}")).unwrap();
                }
            })
        })
        .collect();
    for h in threads {
        h.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    checker.join().unwrap();
    assert_eq!(queued_seqs(&s).len(), 1 + 6 * 30);
    assert!(grouped(&s) > 0);
}

/// Many writers on a dataset with all three indexes: every published snapshot's text
/// and spatial views name its own commit, and after the run every index agrees with the
/// data, before and after reopening.
#[cfg(all(feature = "text", feature = "geo"))]
#[test]
fn many_writers_on_a_dataset_with_every_index() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(Store::open(dir.path(), options()).unwrap());
    s.enable_text(Default::default()).unwrap();
    point(&s, "seed", 0.0, 0.0).unwrap();
    s.enable_geo(Default::default()).unwrap();
    let mut c = crate::vector::VectorIndexConfig::new("http://example.org/emb", 8);
    c.embedding = Some(
        crate::vector::embed::EmbeddingConfig::new("http://127.0.0.1:9/v1", "mock-model")
            .from_predicates(&[LABEL]),
    );
    s.create_vector_index("docs", c).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let (s, stop) = (s.clone(), stop.clone());
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let snap = s.snapshot();
                assert_eq!(snap.text.as_ref().unwrap().seq, snap.commit);
                assert_eq!(snap.geo.as_ref().unwrap().commit, snap.commit);
                // a search of the published head never waits and finds a consistent state
                let hits = text_hits(snap.clone(), "shared").unwrap();
                let labelled = snap.lookup_iri(LABEL).map_or(0, |p| {
                    let mut n = 0;
                    snap.scan(crate::index::Perm::Pso, &[p.0], |c| {
                        n += match c {
                            Chunk::Block(_, a, b) => b - a,
                            Chunk::Row(_) => 1,
                        };
                        Ok(true)
                    })
                    .unwrap();
                    n
                });
                assert_eq!(hits.len(), labelled);
            }
        })
    };
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let s = s.clone();
            thread::spawn(move || {
                for k in 0..25 {
                    let n = format!("w{t}-{k}");
                    update(
                        &s,
                        &format!(
                            "INSERT DATA {{ <urn:{n}> <{LABEL}> \"shared {n}\" . \
                             <urn:{n}> <{GEO}asWKT> \"POINT({t} {k})\"^^<{GEO}wktLiteral> }}"
                        ),
                    )
                    .unwrap();
                    if k % 5 == 4 {
                        let gone = format!("w{t}-{}", k - 2);
                        update(&s, &format!("DELETE WHERE {{ <urn:{gone}> ?p ?o }}")).unwrap();
                    }
                }
            })
        })
        .collect();
    for h in threads {
        h.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    reader.join().unwrap();
    assert_eq!(s.commit_metrics().fallbacks.iter().sum::<u64>(), 0);
    assert!(grouped(&s) > 0);
    let expect = |s: &Store| {
        let snap = s.snapshot();
        let live: BTreeSet<String> = (0..8)
            .flat_map(|t| (0..25).map(move |k| (t, k)))
            .filter(|&(_, k)| !(k % 5 == 2 && k + 2 < 25))
            .map(|(t, k)| format!("urn:w{t}-{k}"))
            .collect();
        assert_eq!(text_hits(snap.clone(), "shared").unwrap(), live);
        let (hits, scanned) = window(snap, [-0.5, -0.5, 8.0, 25.0]);
        assert!(!scanned);
        let spatial: BTreeSet<String> = hits
            .into_iter()
            .map(|h| h.trim_matches(['<', '>']).to_string())
            .filter(|h| h != "urn:seed")
            .collect();
        assert_eq!(spatial, live);
    };
    expect(&s);
    let ti = s.text.load_full().unwrap();
    assert_eq!(ti.applied(), s.snapshot().commit);
    drop(ti);
    drop(s);
    let s = Store::open(dir.path(), options()).unwrap();
    s.wait_geo();
    expect(&s);
}

// ---------------------------------------------------------------- crash ordering

const CRASH_ROOT: &str = "SPARKLES_GROUP_INDEX_CRASH_ROOT";
const CRASH_POINT: &str = "SPARKLES_GROUP_INDEX_CRASH_POINT";
const CRASH_INDEX: &str = "SPARKLES_GROUP_INDEX_CRASH_INDEX";

/// The child of [`crash_between_the_fence_and_index_publication`]: one acknowledged
/// commit, then a commit interrupted by process exit at a failpoint of the group. At the
/// failpoint the full-text index is checkpointed first, as its tick may do at any moment,
/// and the child checks that the checkpoint names no commit above the published head.
#[cfg(all(feature = "text", feature = "geo"))]
#[test]
fn crash_child() {
    let Ok(root) = std::env::var(CRASH_ROOT) else {
        return;
    };
    let root = PathBuf::from(root);
    let index = std::env::var(CRASH_INDEX).unwrap();
    let s = Store::open(&root, options()).unwrap();
    match index.as_str() {
        "text" => {
            s.enable_text(Default::default()).unwrap();
        }
        "geo" => {
            s.enable_geo(Default::default()).unwrap();
        }
        "embed" => {
            let mut c = crate::vector::VectorIndexConfig::new("http://example.org/emb", 8);
            c.embedding = Some(
                crate::vector::embed::EmbeddingConfig::new("http://127.0.0.1:9/v1", "m")
                    .from_predicates(&[LABEL]),
            );
            s.create_vector_index("docs", c).unwrap();
        }
        i => panic!("unknown index {i}"),
    }
    let both = |s: &Store, n: &str, x: f64| {
        update(
            s,
            &format!(
                "INSERT DATA {{ <urn:{n}> <{LABEL}> \"word{n}\" . \
                 <urn:{n}> <{GEO}asWKT> \"POINT({x} {x})\"^^<{GEO}wktLiteral> }}"
            ),
        )
        .unwrap();
    };
    both(&s, "acked", 1.0);
    let point = match std::env::var(CRASH_POINT).unwrap().as_str() {
        "appended" => "group-appended",
        "sealed" => "group-sealed",
        "synced" => "group-synced",
        "published" => "group-published",
        p => panic!("unknown point {p}"),
    };
    let root2 = root.clone();
    s.set_failpoint(
        point,
        Some(Arc::new(move |s: &Store| {
            if let Some(ti) = s.text.load_full() {
                ti.checkpoint().unwrap();
                let head = s.current.load().commit;
                if text_payload_seq(&root2).is_some_and(|seq| seq > head) {
                    std::process::exit(78);
                }
            }
            std::process::exit(77);
        })),
    );
    both(&s, "lost", 2.0);
    panic!("the crash point was not reached");
}

#[cfg(all(feature = "text", feature = "geo"))]
#[test]
fn crash_between_the_fence_and_index_publication() {
    for index in ["text", "geo", "embed"] {
        for point in ["appended", "sealed", "synced", "published"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("db");
            let what = format!("{index} at {point}");
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "store::group_index_tests::crash_child",
                    "--nocapture",
                ])
                .env(CRASH_ROOT, &root)
                .env(CRASH_POINT, point)
                .env(CRASH_INDEX, index)
                .output()
                .unwrap();
            assert_eq!(
                out.status.code(),
                Some(77),
                "{what}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            // the durable state of the full-text index never runs ahead of the log
            if index == "text"
                && let Some(seq) = text_payload_seq(&root)
            {
                let wal = crate::store::Store::open(&root, StoreOptions::default())
                    .unwrap()
                    .snapshot()
                    .commit;
                assert!(
                    seq <= wal,
                    "{what}: checkpoint {seq} above the log's head {wal}"
                );
            }
            let s = Store::open(&root, options()).unwrap();
            if let Some(job) = s.text_recovery.load_full() {
                job.wait().unwrap();
            }
            s.wait_geo();
            let snap = s.snapshot();
            let head = snap.commit;
            assert!((1..=2).contains(&head), "{what}: head {head}");
            // process exit keeps the page cache, so a commit that was synchronized is back
            if matches!(point, "synced" | "published") {
                assert_eq!(head, 2, "{what}");
            }
            let present: Vec<&str> = if head == 2 {
                vec!["acked", "lost"]
            } else {
                vec!["acked"]
            };
            match index {
                "text" => {
                    assert_eq!(s.text_status().unwrap().state, "ready", "{what}");
                    for n in ["acked", "lost"] {
                        let hits = text_hits(snap.clone(), &format!("word{n}")).unwrap();
                        assert_eq!(!hits.is_empty(), present.contains(&n), "{what}: {n}");
                    }
                }
                "geo" => {
                    let (hits, scanned) = window(snap.clone(), [0.0, 0.0, 3.0, 3.0]);
                    assert!(!scanned, "{what}");
                    assert_eq!(hits.len(), present.len(), "{what}: {hits:?}");
                }
                _ => {
                    // the worker starts again from a pass over the recovered state
                    let st = s.embedding_status("docs").unwrap();
                    assert!(st.applied_seq <= head, "{what}");
                }
            }
            drop(snap);
            // the recovered store takes the next commit, grouped
            label(&s, "next", "next").unwrap();
            assert_eq!(s.snapshot().commit, head + 1, "{what}");
        }
    }
}
