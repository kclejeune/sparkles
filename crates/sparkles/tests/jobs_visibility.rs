//! The job ledger's hidden visibility (spec C26 §2.3) across the change that hides it:
//! views, live queries and the ontology cache never serve what was read before jobs were
//! enabled.

use sparkles::Dataset;
use sparkles::incr::{LiveEvent, ViewDef};
use std::sync::Arc;
use std::time::{Duration, Instant};

const LEDGER: &str = "urn:x-sparkles:jobs:ledger";

fn enable(ds: &Dataset) {
    let mut s = sparkles::jobs::Settings {
        enabled: true,
        ..Default::default()
    };
    s.definitions.insert(
        "collect".into(),
        sparkles::jobs::Definition::quads(vec!["urn:g".into()]),
    );
    ds.jobs().configure(&s).unwrap();
}

fn submit(ds: &Dataset) {
    ds.jobs()
        .submit(
            &sparkles::jobs::Submission::new("collect", serde_json::json!({}), "k"),
            &sparkles::jobs::Caller::admin(oxrdf::NamedNode::new_unchecked("urn:worker")),
        )
        .unwrap();
}

#[test]
fn a_ready_view_hides_newly_hidden_rows_before_the_worker_runs() {
    let ds = Dataset::memory();
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{LEDGER}> {{ <urn:Secret> <urn:p> 1 }} }}"
    ))
    .unwrap();
    let q = format!("SELECT ?s WHERE {{ GRAPH <{LEDGER}> {{ ?s ?p ?o }} }}");
    ds.views().put("v", ViewDef::table(q)).unwrap();
    ds.views().wait_built("v", Duration::from_secs(10)).unwrap();
    assert_eq!(ds.views().read("v").unwrap().rows.len(), 1);
    ds.views().settle(Duration::from_secs(2));
    let pause = ds
        .state()
        .views
        .pause_worker(Instant::now() + Duration::from_secs(2))
        .unwrap();
    let before = ds.snapshot().commit;
    enable(&ds);
    assert_eq!(ds.snapshot().commit, before);
    let r = ds.views().read("v");
    assert!(
        r.as_ref().is_err() || r.as_ref().is_ok_and(|r| r.rows.is_empty()),
        "{:?}",
        r.as_ref().map(|r| &r.rows)
    );
    drop(pause);
}

#[test]
fn enabling_jobs_without_a_commit_ends_a_waiting_live_query() {
    let ds = Dataset::memory();
    let live = ds
        .live(
            "SELECT ?s WHERE { GRAPH ?g { ?s ?p ?o } }",
            &Default::default(),
        )
        .unwrap();
    assert!(matches!(
        live.next_timeout(Duration::from_secs(2)),
        Some(LiveEvent::Snapshot { .. })
    ));
    ds.views().settle(Duration::from_secs(2));
    // the notifications already issued are consumed before only the registry changes
    assert!(live.next_timeout(Duration::from_millis(200)).is_none());
    let pause = ds
        .state()
        .views
        .pause_worker(Instant::now() + Duration::from_secs(2))
        .unwrap();
    enable(&ds);
    drop(pause);
    let e = live.next_timeout(Duration::from_secs(5));
    assert!(
        matches!(&e, Some(LiveEvent::Error { code, .. }) if code == "forbidden"),
        "{e:?}, closed={}",
        live.is_closed()
    );
}

#[test]
fn an_ended_live_query_wakes_its_reader_and_leaves_the_worker_usable() {
    let ds = Dataset::memory();
    let live = ds
        .live("SELECT ?s WHERE { ?s <urn:p> ?o }", &Default::default())
        .unwrap();
    live.next_timeout(Duration::from_secs(2)).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let e = live.next();
        let closed = live.next();
        tx.send((e, closed)).unwrap();
    });
    enable(&ds);
    ds.update("INSERT DATA { <urn:public> <urn:p> 1 }").unwrap();
    let (e, closed) = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the reader is woken");
    assert!(matches!(e, Some(LiveEvent::Error { code, .. }) if code == "forbidden"));
    assert!(closed.is_none());
    reader.join().unwrap();
    ds.views()
        .put("v", ViewDef::table("SELECT ?s WHERE { ?s <urn:p> ?o }"))
        .unwrap();
    ds.views().wait_built("v", Duration::from_secs(5)).unwrap();
    assert_eq!(ds.views().read("v").unwrap().rows.len(), 1);
}

/// A host that resolves access as the library's does and holds the first query's
/// admission in its prefixes until resumed.
struct PausedPrefixes {
    state: sparkles::dataset::StateReference,
    ready: std::sync::mpsc::Sender<()>,
    resume: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl sparkles::incr::Host for PausedPrefixes {
    fn access(
        &self,
        _: Option<&str>,
    ) -> sparkles::Result<Option<Arc<sparkles::access::GraphAccess>>> {
        Ok(self.state.upgrade().and_then(|s| {
            sparkles::access::GraphAccess::hiding_graphs(None, &s.store.hidden_graphs())
        }))
    }
    fn epoch(&self) -> u64 {
        self.state
            .upgrade()
            .map_or(0, |s| sparkles::handles::views::visibility_epoch(&s.store))
    }
    fn stored_query(&self, _: &str) -> sparkles::Result<String> {
        unreachable!()
    }
    fn prefixes(&self) -> Vec<(String, String)> {
        let rx = self.resume.lock().unwrap().take();
        if let Some(rx) = rx {
            self.ready.send(()).unwrap();
            rx.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        Vec::new()
    }
}

#[test]
fn a_live_query_admitted_across_enablement_never_keeps_the_old_access() {
    let ds = Dataset::memory();
    let (tx, rx) = std::sync::mpsc::channel();
    let (resume, wait) = std::sync::mpsc::channel();
    ds.start_views(Arc::new(PausedPrefixes {
        state: ds.state_reference(),
        ready: tx,
        resume: std::sync::Mutex::new(Some(wait)),
    }));
    let d = ds.clone();
    let joining = std::thread::spawn(move || {
        d.live(
            &format!("SELECT ?s ?p ?o WHERE {{ GRAPH <{LEDGER}> {{ ?s ?p ?o }} }}"),
            &Default::default(),
        )
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    enable(&ds);
    submit(&ds);
    // the worker handles the new epoch before the paused query is admitted
    ds.views().settle(Duration::from_secs(3));
    resume.send(()).unwrap();
    match joining.join().unwrap() {
        Ok(live) => {
            let e = live.next_timeout(Duration::from_secs(3));
            assert!(
                !matches!(&e, Some(LiveEvent::Snapshot { rows, .. }) if !rows.is_empty()),
                "{e:?}"
            );
        }
        Err(e) => assert_eq!(e.code(), "forbidden"),
    }
}

#[test]
fn an_ended_live_query_wakes_every_async_waiter() {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake, Waker};
    struct H(AtomicU64);
    impl sparkles::incr::Host for H {
        fn access(
            &self,
            _: Option<&str>,
        ) -> sparkles::Result<Option<Arc<sparkles::access::GraphAccess>>> {
            Ok(None)
        }
        fn stored_query(&self, _: &str) -> sparkles::Result<String> {
            unreachable!()
        }
        fn epoch(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }
    struct W(AtomicUsize);
    impl Wake for W {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let ds = Dataset::memory();
    let rt = sparkles::incr::Runtime::open(None, ds.store().dataset_id(), Default::default());
    let host = Arc::new(H(AtomicU64::new(0)));
    rt.set_host(host.clone());
    let live = rt
        .live(
            ds.store(),
            "SELECT ?s WHERE { ?s ?p ?o }",
            None,
            &Default::default(),
        )
        .unwrap();
    live.next_timeout(Duration::from_secs(1)).unwrap();
    let mut a = std::pin::pin!(live.recv());
    let mut b = std::pin::pin!(live.recv());
    let (wa, wb) = (
        Arc::new(W(AtomicUsize::new(0))),
        Arc::new(W(AtomicUsize::new(0))),
    );
    let (w1, w2) = (Waker::from(wa.clone()), Waker::from(wb.clone()));
    assert!(matches!(
        a.as_mut().poll(&mut Context::from_waker(&w1)),
        Poll::Pending
    ));
    assert!(matches!(
        b.as_mut().poll(&mut Context::from_waker(&w2)),
        Poll::Pending
    ));
    host.0.store(1, Ordering::SeqCst);
    rt.round(ds.store());
    assert!(
        wa.0.load(Ordering::SeqCst) > 0 && wb.0.load(Ordering::SeqCst) > 0,
        "first={}, second={}",
        wa.0.load(Ordering::SeqCst),
        wb.0.load(Ordering::SeqCst)
    );
}

#[cfg(feature = "ontology")]
#[test]
fn an_ontology_opened_across_enablement_is_not_kept_for_the_new_visibility() {
    use sparkles::handles::ontology::OntologyRequest;
    use sparkles::reasoning::reasoner::inputs::GraphRef;
    let ds = Dataset::memory();
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{LEDGER}> {{ <urn:SecretClass> a <http://www.w3.org/2002/07/owl#Class> }} }}"
    ))
    .unwrap();
    let req = OntologyRequest {
        graphs: vec![GraphRef::Named(LEDGER.into())],
        inferences: true,
        ..Default::default()
    };
    // the opening waits on the reasoning record, between its view and its cache key
    let reasoning = ds.state().reasoning.write();
    let (d, request) = (ds.clone(), req.clone());
    let (begin, started) = std::sync::mpsc::channel();
    let opening = std::thread::spawn(move || {
        begin.send(()).unwrap();
        d.ontology()
            .open_at(&d.snapshot(), &request, &Default::default())
    });
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    enable(&ds);
    drop(reasoning);
    opening.join().unwrap().unwrap();
    let after = ds
        .ontology()
        .open_at(&ds.snapshot(), &req, &Default::default())
        .unwrap();
    assert_eq!(after.summary().triples, 0, "cached={}", after.cached);
}

#[cfg(feature = "shacl")]
#[test]
fn a_guard_naming_the_ledger_is_refused_as_for_a_graph_that_does_not_exist() {
    let ds = Dataset::memory();
    ds.update(&format!(
        "PREFIX sh: <http://www.w3.org/ns/shacl#> INSERT DATA {{ GRAPH <{LEDGER}> {{ <urn:SecretShape> a sh:NodeShape ; sh:targetClass <urn:Public> ; sh:property [ sh:path <urn:secret-path> ; sh:minCount 1 ] }} }}"
    ))
    .unwrap();
    enable(&ds);
    for cfg in [
        serde_json::json!({"mode": "reject", "dataGraph": "default", "shapes": {"graphs": [LEDGER]}}),
        serde_json::json!({"mode": "reject", "dataGraph": [LEDGER],
            "shapes": {"inline": "@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:S> a sh:NodeShape ."}}),
    ] {
        let r = ds
            .validation()
            .guard()
            .set_shacl(serde_json::from_value(cfg).unwrap());
        assert!(matches!(r, Err(sparkles::Error::NotFound(_))), "{r:?}");
    }
    assert!(ds.validation().guard().get().is_none());
}

/// A SHACL shape over the union that every run of the ledger breaks.
#[cfg(feature = "shacl")]
fn shacl_over_runs() -> serde_json::Value {
    serde_json::json!({"mode": "reject", "dataGraph": "union", "shapes": {"inline":
        "@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:NoJobs> a sh:NodeShape ; sh:targetClass <urn:x-sparkles:jobs:Run> ; sh:property [ sh:path <urn:missing> ; sh:minCount 1 ] ."}})
}

/// A ShEx shape map over the union that every run of the ledger breaks.
#[cfg(feature = "shex")]
fn shex_over_runs() -> serde_json::Value {
    serde_json::json!({"language": "shex", "mode": "reject", "dataGraph": "union",
        "schema": {"inline": "<urn:S> { <urn:missing> . }", "format": "shexc"},
        "shapeMap": "{FOCUS a <urn:x-sparkles:jobs:Run>}@<urn:S>"})
}

/// The guard still validates the data it can read: a public node that breaks the shape
/// is refused.
#[cfg(any(feature = "shacl", feature = "shex"))]
fn a_public_run_is_refused(ds: &Dataset) {
    let r = ds.update("INSERT DATA { <urn:public> a <urn:x-sparkles:jobs:Run> }");
    assert!(matches!(r, Err(sparkles::Error::Rejected(_))), "{r:?}");
}

#[cfg(feature = "shacl")]
#[test]
fn installing_a_shacl_guard_never_validates_the_ledger() {
    let ds = Dataset::memory();
    enable(&ds);
    submit(&ds);
    let r = ds
        .validation()
        .guard()
        .set_shacl(serde_json::from_value(shacl_over_runs()).unwrap())
        .unwrap();
    assert!(
        matches!(r, sparkles::handles::GuardOutcome::Installed(_)),
        "{r:?}"
    );
    // the next run commits under the guard, which never reads the ledger
    ds.jobs()
        .submit(
            &sparkles::jobs::Submission::new("collect", serde_json::json!({}), "k2"),
            &sparkles::jobs::Caller::admin(oxrdf::NamedNode::new_unchecked("urn:worker")),
        )
        .unwrap();
    a_public_run_is_refused(&ds);
}

#[cfg(feature = "shacl")]
#[test]
fn a_shacl_guard_installed_before_jobs_leaves_the_ledger_out() {
    let ds = Dataset::memory();
    ds.validation()
        .guard()
        .set_shacl(serde_json::from_value(shacl_over_runs()).unwrap())
        .unwrap();
    enable(&ds);
    submit(&ds);
    let status = ds.validation().guard().get().unwrap().json()["status"].clone();
    assert!(
        !status.to_string().contains("urn:x-sparkles:jobs:run"),
        "{status}"
    );
    a_public_run_is_refused(&ds);
}

#[cfg(feature = "shex")]
#[test]
fn installing_a_shex_guard_never_validates_the_ledger() {
    let ds = Dataset::memory();
    enable(&ds);
    submit(&ds);
    let r = ds
        .validation()
        .guard()
        .set_shex(
            serde_json::from_value(shex_over_runs()).unwrap(),
            &sparkles_shex::NoImports,
        )
        .unwrap();
    assert!(
        matches!(r, sparkles::handles::GuardOutcome::Installed(_)),
        "{r:?}"
    );
    ds.jobs()
        .submit(
            &sparkles::jobs::Submission::new("collect", serde_json::json!({}), "k2"),
            &sparkles::jobs::Caller::admin(oxrdf::NamedNode::new_unchecked("urn:worker")),
        )
        .unwrap();
    a_public_run_is_refused(&ds);
}

#[cfg(feature = "shex")]
#[test]
fn a_shex_guard_installed_before_jobs_leaves_the_ledger_out() {
    let ds = Dataset::memory();
    ds.validation()
        .guard()
        .set_shex(
            serde_json::from_value(shex_over_runs()).unwrap(),
            &sparkles_shex::NoImports,
        )
        .unwrap();
    enable(&ds);
    submit(&ds);
    a_public_run_is_refused(&ds);
}

/// A persistent dataset keeps the guard's state of the head under the graphs it hides,
/// so a state kept before jobs were enabled is not trusted after a reopen.
#[cfg(feature = "shacl")]
#[test]
fn a_reopened_shacl_guard_keeps_the_ledger_out() {
    let dir = tempfile::tempdir().unwrap();
    {
        let ds = Dataset::open(dir.path()).unwrap();
        ds.validation()
            .guard()
            .set_shacl(serde_json::from_value(shacl_over_runs()).unwrap())
            .unwrap();
        enable(&ds);
        submit(&ds);
    }
    let ds = Dataset::open(dir.path()).unwrap();
    ds.jobs()
        .submit(
            &sparkles::jobs::Submission::new("collect", serde_json::json!({}), "k2"),
            &sparkles::jobs::Caller::admin(oxrdf::NamedNode::new_unchecked("urn:worker")),
        )
        .unwrap();
    a_public_run_is_refused(&ds);
}

/// Shapes in the future ledger graph that a guard can name.
const LEDGER_SHAPES: &str = "PREFIX sh: <http://www.w3.org/ns/shacl#> INSERT DATA { GRAPH <urn:x-sparkles:jobs:ledger> { <urn:SecretShape> a sh:NodeShape ; sh:targetClass <urn:Public> ; sh:property [ sh:path <urn:secret-path> ; sh:minCount 1 ] } }";

/// A ShEx schema in the future ledger graph that a guard can name.
const LEDGER_SCHEMA: &str = "INSERT DATA { GRAPH <urn:x-sparkles:jobs:ledger> { <urn:schema> a <http://www.w3.org/ns/shex#Schema> ; <http://www.w3.org/ns/shex#shapes> <urn:S> . <urn:S> a <http://www.w3.org/ns/shex#Shape> ; <http://www.w3.org/ns/shex#expression> <urn:tc> . <urn:tc> a <http://www.w3.org/ns/shex#TripleConstraint> ; <http://www.w3.org/ns/shex#predicate> <urn:secret-path> ; <http://www.w3.org/ns/shex#min> 1 ; <http://www.w3.org/ns/shex#max> 1 } }";

#[cfg(feature = "shacl")]
fn ledger_shacl() -> serde_json::Value {
    serde_json::json!({"mode": "reject", "dataGraph": "default", "shapes": {"graphs": [LEDGER]}})
}

#[cfg(feature = "shex")]
fn ledger_shex() -> serde_json::Value {
    serde_json::json!({"language": "shex", "mode": "reject", "dataGraph": "default",
        "schema": {"graphs": [LEDGER]}, "shapeMap": "{FOCUS a <urn:Public>}@<urn:S>"})
}

/// Wait until `n` writers wait for the store's writer lock.
#[allow(dead_code)]
fn wait_writers(ds: &Dataset, n: usize) {
    let until = Instant::now() + Duration::from_secs(5);
    while ds.store().writers_waiting() < n {
        assert!(Instant::now() < until, "the writers did not arrive");
        std::thread::yield_now();
    }
}

/// A ShEx import resolver that waits for a signal, holding the guard configuration that
/// imports it under the writer lock.
#[cfg(feature = "shex")]
struct PausedImport {
    ready: std::sync::mpsc::Sender<()>,
    resume: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

#[cfg(feature = "shex")]
impl sparkles_shex::Resolver for PausedImport {
    fn import(&self, _: &str) -> anyhow::Result<Option<sparkles_shex::Schema>> {
        self.ready.send(()).unwrap();
        self.resume
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        Ok(Some(sparkles_shex::Schema::parse_shexc(
            "<urn:Imported> {}",
            None,
        )?))
    }
}

/// A guard configured while jobs are being enabled is the guard that stays installed.
#[cfg(all(feature = "shacl", feature = "shex"))]
#[test]
fn enablement_never_puts_back_a_guard_replaced_meanwhile() {
    use std::sync::mpsc;
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    ds.validation()
        .guard()
        .set_shacl(serde_json::from_value(shacl_over_runs()).unwrap())
        .unwrap();
    let (ready, started) = mpsc::channel();
    let (resume, wait) = mpsc::channel();
    let d = ds.clone();
    let setter = std::thread::spawn(move || {
        d.validation().guard().set_shex(
            serde_json::from_value(serde_json::json!({"language": "shex", "mode": "reject",
                "dataGraph": "union",
                "schema": {"inline": "IMPORT <urn:import> <urn:NewShape> { <urn:missing> . }", "format": "shexc"},
                "shapeMap": "{FOCUS a <urn:New>}@<urn:NewShape>"}))
            .unwrap(),
            &PausedImport {
                ready,
                resume: std::sync::Mutex::new(wait),
            },
        )
    });
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    let d = ds.clone();
    let enabler = std::thread::spawn(move || enable(&d));
    wait_writers(&ds, 1);
    resume.send(()).unwrap();
    setter.join().unwrap().unwrap();
    enabler.join().unwrap();
    let r = ds.update("INSERT DATA { <urn:node> a <urn:New> }");
    assert!(matches!(r, Err(sparkles::Error::Rejected(_))), "{r:?}");
}

/// A guard configured while enablement waits for the writer is checked against the
/// registry it finds once it holds the writer.
#[cfg(feature = "shacl")]
#[test]
fn a_guard_configuration_queued_behind_enablement_refuses_the_ledger() {
    let ds = Dataset::memory();
    ds.update(LEDGER_SHAPES).unwrap();
    ds.update("INSERT DATA { <urn:focus> a <urn:Public> }")
        .unwrap();
    let txn = ds.store().write();
    let d = ds.clone();
    let enabler = std::thread::spawn(move || enable(&d));
    wait_writers(&ds, 1);
    let d = ds.clone();
    let setter = std::thread::spawn(move || {
        d.validation()
            .guard()
            .set_shacl(serde_json::from_value(ledger_shacl()).unwrap())
    });
    wait_writers(&ds, 2);
    drop(txn);
    enabler.join().unwrap();
    let r = setter.join().unwrap();
    assert!(matches!(r, Err(sparkles::Error::NotFound(_))), "{r:?}");
    assert!(!format!("{r:?}").contains("secret-path"), "{r:?}");
}

/// Jobs are not enabled under a guard that reads the ledger graph, and no write quotes
/// the shapes kept there.
#[cfg(feature = "shacl")]
#[test]
fn jobs_are_refused_under_a_shacl_guard_whose_shapes_are_in_the_ledger() {
    let ds = Dataset::memory();
    ds.update(LEDGER_SHAPES).unwrap();
    ds.validation()
        .guard()
        .set_shacl(serde_json::from_value(ledger_shacl()).unwrap())
        .unwrap();
    let mut s = sparkles::jobs::Settings {
        enabled: true,
        ..Default::default()
    };
    s.definitions.insert(
        "collect".into(),
        sparkles::jobs::Definition::quads(vec!["urn:g".into()]),
    );
    let r = ds.jobs().configure(&s);
    assert!(matches!(r, Err(sparkles::Error::Conflict(_))), "{r:?}");
    assert!(ds.store().hidden_graphs().is_empty());
}

#[cfg(feature = "shex")]
#[test]
fn jobs_are_refused_under_a_shex_guard_whose_schema_is_in_the_ledger() {
    let ds = Dataset::memory();
    ds.update(LEDGER_SCHEMA).unwrap();
    ds.validation()
        .guard()
        .set_shex(
            serde_json::from_value(ledger_shex()).unwrap(),
            &sparkles_shex::NoImports,
        )
        .unwrap();
    let mut s = sparkles::jobs::Settings {
        enabled: true,
        ..Default::default()
    };
    s.definitions.insert(
        "collect".into(),
        sparkles::jobs::Definition::quads(vec!["urn:g".into()]),
    );
    let r = ds.jobs().configure(&s);
    assert!(matches!(r, Err(sparkles::Error::Conflict(_))), "{r:?}");
}

/// A guard that meets a hidden shapes source, here a configuration file written by hand
/// after jobs were enabled, rejects every write without naming what the source holds.
#[cfg(feature = "shacl")]
#[test]
fn a_guard_whose_shapes_source_is_hidden_rejects_without_quoting_it() {
    let dir = tempfile::tempdir().unwrap();
    {
        let ds = Dataset::open(dir.path()).unwrap();
        ds.update(LEDGER_SHAPES).unwrap();
        enable(&ds);
    }
    std::fs::write(
        dir.path().join(sparkles::guard::config::CONFIG_FILE),
        serde_json::to_vec(&serde_json::json!({"format": 2, "language": "shacl",
            "mode": "warn", "dataGraph": "default", "shapes": {"graphs": [LEDGER]}}))
        .unwrap(),
    )
    .unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    let r = ds.update("INSERT DATA { <urn:public> a <urn:Public> }");
    assert!(matches!(r, Err(sparkles::Error::Rejected(_))), "{r:?}");
    let text = format!("{r:?}");
    assert!(
        !text.contains("secret-path") && !text.contains("SecretShape"),
        "{text}"
    );
}

/// A job commit that a guard installed just before enablement checks leaves the ledger
/// out, whenever the guard was installed.
#[cfg(all(feature = "shacl", feature = "failpoints"))]
#[test]
fn a_guard_installed_while_enablement_waits_never_validates_job_records() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    let ds = Dataset::memory();
    let (tx, rx) = mpsc::channel();
    let (resume, wait) = mpsc::channel::<()>();
    let wait = std::sync::Mutex::new(wait);
    let once = AtomicBool::new(false);
    ds.store().set_failpoint(
        "writer-acquiring",
        Some(Arc::new(move |_| {
            if std::thread::current().name() == Some("enabler")
                && !once.swap(true, Ordering::SeqCst)
            {
                tx.send(()).unwrap();
                wait.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
            }
        })),
    );
    let d = ds.clone();
    let enabler = std::thread::Builder::new()
        .name("enabler".into())
        .spawn(move || enable(&d))
        .unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    ds.validation()
        .guard()
        .set_shacl(serde_json::from_value(shacl_over_runs()).unwrap())
        .unwrap();
    // The dataset's guard slot stays taken while the job commits, so nothing can
    // install another guard between the registry change and the job.
    let held = ds.state().validation.write();
    resume.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while ds.store().hidden_graphs().is_empty() {
        assert!(Instant::now() < until, "jobs were not enabled");
        std::thread::yield_now();
    }
    let r = ds.jobs().submit(
        &sparkles::jobs::Submission::new("collect", serde_json::json!({}), "k"),
        &sparkles::jobs::Caller::admin(oxrdf::NamedNode::new_unchecked("urn:worker")),
    );
    drop(held);
    enabler.join().unwrap();
    ds.store().set_failpoint("writer-acquiring", None);
    assert!(r.is_ok(), "{r:?}");
    a_public_run_is_refused(&ds);
}

/// Enabling jobs ends a live query that waits idle on an open branch.
#[test]
fn enabling_jobs_ends_an_idle_live_query_on_an_open_branch() {
    let ds = Dataset::memory();
    ds.create_branch("work", &Default::default()).unwrap();
    let branch = ds.branch("work").unwrap();
    let live = branch
        .live(
            "SELECT ?s WHERE { GRAPH ?g { ?s ?p ?o } }",
            &Default::default(),
        )
        .unwrap();
    live.next_timeout(Duration::from_secs(2)).unwrap();
    branch.views().settle(Duration::from_secs(2));
    assert!(live.next_timeout(Duration::from_millis(200)).is_none());
    enable(&ds);
    let event = live.next_timeout(Duration::from_secs(5));
    assert!(
        matches!(&event, Some(LiveEvent::Error { code, .. }) if code == "forbidden"),
        "{event:?}"
    );
}

/// The guard of a branch that was open when jobs were enabled leaves the ledger out.
#[cfg(feature = "shacl")]
#[test]
fn an_open_branch_shacl_guard_leaves_the_ledger_out_once_jobs_are_enabled() {
    let ds = Dataset::memory();
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{LEDGER}> {{ <urn:hidden> a <urn:x-sparkles:jobs:Run> }} }}"
    ))
    .unwrap();
    let mut cfg = shacl_over_runs();
    cfg["mode"] = "warn".into();
    ds.validation()
        .guard()
        .set_shacl(serde_json::from_value(cfg).unwrap())
        .unwrap();
    ds.create_branch("work", &Default::default()).unwrap();
    let branch = ds.branch("work").unwrap();
    enable(&ds);
    let written = branch
        .update("INSERT DATA { <urn:public> <urn:p> 1 }")
        .unwrap();
    assert!(
        !format!("{written:?}").contains("urn:hidden"),
        "{written:?}"
    );
}

#[cfg(feature = "shex")]
#[test]
fn an_open_branch_shex_guard_leaves_the_ledger_out_once_jobs_are_enabled() {
    let ds = Dataset::memory();
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{LEDGER}> {{ <urn:hidden> a <urn:x-sparkles:jobs:Run> }} }}"
    ))
    .unwrap();
    let mut cfg = shex_over_runs();
    cfg["mode"] = "warn".into();
    ds.validation()
        .guard()
        .set_shex(
            serde_json::from_value(cfg).unwrap(),
            &sparkles_shex::NoImports,
        )
        .unwrap();
    ds.create_branch("work", &Default::default()).unwrap();
    let branch = ds.branch("work").unwrap();
    enable(&ds);
    let written = branch
        .update("INSERT DATA { <urn:public> <urn:p> 1 }")
        .unwrap();
    assert!(
        !format!("{written:?}").contains("urn:hidden"),
        "{written:?}"
    );
}
