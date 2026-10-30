//! Footprints, coverage and the guard (C22 §6, A15, A16).

use super::*;
use sparkles::access::{Limits, TripleRules};

const A: &str = "urn:g:a";
const B: &str = "urn:g:b";
const REPORTS: &str = "urn:x-sparkles:graphrag:reports";

fn graphs(names: &[&str]) -> Graphs {
    Graphs::Only(GraphRule::new(names.iter().copied(), &[]))
}

use sparkles::access::GraphRule;

fn protection(name: &str, pattern: Option<&str>) -> Arc<Protection> {
    Arc::new(Protection {
        name: name.into(),
        predicates: Some(vec!["http://ex/salary".into()]),
        classes: None,
        subclasses: true,
        graphs: None,
        pattern: pattern.map(str::to_string),
        prefixes: BTreeMap::new(),
        hide_inferences: false,
    })
}

fn caller(user: &str, groups: &[&str]) -> Caller {
    Caller {
        user: Some(user.into()),
        roles: vec!["staff".into()],
        groups: groups.iter().map(|g| g.to_string()).collect(),
    }
}

/// A view reading `read`, with `protections` in force everywhere (lifted nowhere).
fn view(read: &[&str], protections: &[Arc<Protection>], c: Caller) -> GraphAccess {
    let rules = TripleRules {
        rules: protections
            .iter()
            .map(|p| Rule {
                protection: p.clone(),
                read: Graphs::none(),
                write: Graphs::none(),
            })
            .collect(),
        caller: c,
        limits: Limits::default(),
    };
    if protections.is_empty() {
        GraphAccess::graphs(graphs(read), Graphs::none())
    } else {
        GraphAccess::with_triples(graphs(read), Graphs::none(), rules)
    }
}

fn footprint(v: Option<&GraphAccess>, read: &[&str], c: &Caller) -> Footprint {
    let graphs: Vec<String> = read.iter().map(|g| g.to_string()).collect();
    capture(&CaptureInput {
        dataset: "ds",
        branch: "main",
        source_commit: 7,
        principal: "reports",
        endpoint: "query",
        view: v,
        caller: c,
        graphs: &graphs,
        reports: &[],
    })
    .unwrap()
}

#[test]
fn unrestricted_readers_are_proven() {
    let c = caller("reports", &[]);
    let p = view(&[A, B], &[protection("pay", None)], c.clone());
    let fp = footprint(Some(&p), &[A, B], &c);
    assert_eq!(covers(None, &fp), Decision::Proven);
    let all = GraphAccess::graphs(Graphs::All, Graphs::All);
    assert_eq!(covers(Some(&all), &fp), Decision::Proven);
}

#[test]
fn an_identical_view_is_proven() {
    let c = caller("reports", &["g1"]);
    let p = view(
        &[A, B],
        &[protection("pay", Some("?s <http://ex/owner> ?user"))],
        c.clone(),
    );
    let fp = footprint(Some(&p), &[A, B], &c);
    assert_eq!(covers(Some(&p), &fp), Decision::Proven);
}

/// The principal later loses B; a reader of A only is still denied (A15).
#[test]
fn coverage_follows_the_footprint_not_current_grants() {
    let c = caller("reports", &[]);
    let at_build = view(&[A, B], &[], c.clone());
    let fp = footprint(Some(&at_build), &[A, B], &c);
    let reader = view(&[A], &[], caller("someone", &[]));
    assert_eq!(covers(Some(&reader), &fp), Decision::Denied);
    let wider = view(&[A, B, "urn:g:c"], &[], caller("someone", &[]));
    assert_eq!(covers(Some(&wider), &fp), Decision::Proven);
}

#[test]
fn structural_inclusion_admits_fewer_protections() {
    let c = caller("reports", &[]);
    let pay = protection("pay", None);
    let p = view(&[A], std::slice::from_ref(&pay), c.clone());
    let fp = footprint(Some(&p), &[A], &c);
    // the reader lifts the protection the principal had in force
    let reader = view(&[A], &[], caller("hr", &[]));
    assert_eq!(covers(Some(&reader), &fp), Decision::Proven);
    // a protection in force for the reader that the principal lifted is unproven
    let fp_open = footprint(Some(&view(&[A], &[], c.clone())), &[A], &c);
    let restricted = view(&[A], &[pay], caller("staff", &[]));
    assert_eq!(covers(Some(&restricted), &fp_open), Decision::Unproven);
}

#[test]
fn a_changed_definition_with_the_same_name_is_unproven() {
    let c = caller("reports", &[]);
    let fp = footprint(
        Some(&view(&[A], &[protection("pay", None)], c.clone())),
        &[A],
        &c,
    );
    let mut other = (*protection("pay", None)).clone();
    other.predicates = Some(vec!["http://ex/bonus".into()]);
    let reader = view(&[A], &[Arc::new(other)], c.clone());
    assert_eq!(covers(Some(&reader), &fp), Decision::Unproven);
}

/// Same grants and roles, different `?group` values (A16).
#[test]
fn caller_variables_a_pattern_uses_must_match() {
    let c = caller("reports", &["finance"]);
    let docs = protection("docs", Some("?s <http://ex/group> ?group"));
    let fp = footprint(
        Some(&view(&[A], std::slice::from_ref(&docs), c.clone())),
        &[A],
        &c,
    );
    let same = view(
        &[A],
        std::slice::from_ref(&docs),
        caller("ana", &["finance"]),
    );
    assert_eq!(
        covers(Some(&same), &fp),
        Decision::Proven,
        "?user is not used"
    );
    let other = view(&[A], std::slice::from_ref(&docs), caller("bob", &["sales"]));
    assert_eq!(covers(Some(&other), &fp), Decision::Unproven);
    // $-variables count as well
    let by_user = protection("mine", Some("?s <http://ex/owner> $user"));
    let fp = footprint(
        Some(&view(&[A], std::slice::from_ref(&by_user), c.clone())),
        &[A],
        &c,
    );
    let other = view(&[A], &[by_user], caller("bob", &["finance"]));
    assert_eq!(covers(Some(&other), &fp), Decision::Unproven);
}

#[test]
fn variable_detection_needs_a_whole_name() {
    assert!(uses_var("?s ex:o ?user .", "user"));
    assert!(uses_var("FILTER(?user)", "user"));
    assert!(!uses_var("?username", "user"));
    assert!(!uses_var("?s ex:user ?o", "user"));
    assert!(uses_var("$group", "group"));
}

#[test]
fn capture_refuses_what_it_cannot_describe() {
    let c = caller("reports", &[]);
    let v = view(&[A], &[], c.clone());
    let graphs = vec![B.to_string()];
    let e = capture(&CaptureInput {
        dataset: "ds",
        branch: "main",
        source_commit: 1,
        principal: "reports",
        endpoint: "query",
        view: Some(&v),
        caller: &c,
        graphs: &graphs,
        reports: &[],
    })
    .unwrap_err();
    assert_eq!(e, CaptureError::Unreadable(B.into()));
    let graphs = vec![REPORTS.to_string()];
    let e = capture(&CaptureInput {
        dataset: "ds",
        branch: "main",
        source_commit: 1,
        principal: "reports",
        endpoint: "query",
        view: None,
        caller: &c,
        graphs: &graphs,
        reports: &[],
    })
    .unwrap_err();
    assert!(matches!(e, CaptureError::Unrepresentable(_)));
}

use sparkles::io::{RdfFormat, Source};
use sparkles::store::{Store, StoreOptions};

fn store(trig: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        trig.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    s
}

/// A quad as TriG.
fn nq(q: &oxrdf::Quad) -> String {
    format!(
        "{} {{ {} {} {} . }}\n",
        q.graph_name, q.subject, q.predicate, q.object
    )
}

/// The TriG of a reports graph whose report links to `fp`, with `fp` stored.
fn with_footprint(fp: &Footprint) -> String {
    let mut out = format!(
        "<{REPORTS}> {{ <http://ex/r> <{}> <{}> . <http://ex/r> <http://ex/summary> \"s\" . }}\n",
        gr("disclosure"),
        fp.iri()
    );
    for q in fp.quads(REPORTS) {
        out.push_str(&nq(&q));
    }
    out
}

fn reader(read: &[&str]) -> GraphAccess {
    GraphAccess::graphs(graphs(read), Graphs::none())
}

#[test]
fn footprints_are_named_by_their_digest_and_versioned() {
    let c = caller("reports", &[]);
    let fp = footprint(None, &[A], &c);
    assert!(fp.iri().starts_with(FOOTPRINT_PREFIX));
    assert_eq!(fp.iri().len(), FOOTPRINT_PREFIX.len() + 64);
    assert_eq!(Footprint::parse(&fp.to_json()).unwrap(), fp);
    let newer = fp.to_json().replacen("\"version\":1", "\"version\":2", 1);
    assert!(Footprint::parse(&newer).unwrap_err().contains("version 2"));
    assert!(Footprint::parse("{}").is_err());
}

#[test]
fn a_snapshot_proves_what_its_footprints_cover() {
    let c = caller("reports", &[]);
    let fp = footprint(Some(&view(&[A], &[], c.clone())), &[A], &c);
    let s = store(&with_footprint(&fp));
    let g = Guard::new();
    let idx = g.index(&s.snapshot());
    assert_eq!(
        idx.proven(&reader(&[A, REPORTS])),
        vec![REPORTS.to_string()]
    );
    assert!(idx.proven(&reader(&[B, REPORTS])).is_empty(), "denied");
    // the disclosure graph is never proven
    assert!(
        !idx.proven(&reader(&[A, REPORTS]))
            .contains(&DISCLOSURE_GRAPH.to_string())
    );
}

#[test]
fn reports_without_a_valid_footprint_are_unproven() {
    let c = caller("reports", &[]);
    let fp = footprint(Some(&view(&[A], &[], c.clone())), &[A], &c);
    let r = reader(&[A, REPORTS]);
    let g = Guard::new();
    // a report linking to a footprint the snapshot lacks
    let s = store(&format!(
        "<{REPORTS}> {{ <http://ex/r> <{}> <{}> . }}",
        gr("disclosure"),
        fp.iri()
    ));
    assert!(g.index(&s.snapshot()).proven(&r).is_empty());
    // a footprint whose JSON does not match its name
    let mut other = fp.clone();
    other.source_commit = 99;
    let forged = with_footprint(&fp).replace(
        &fp.to_json().replace('"', "\\\""),
        &other.to_json().replace('"', "\\\""),
    );
    assert_ne!(forged, with_footprint(&fp));
    assert!(g.index(&store(&forged).snapshot()).proven(&r).is_empty());
    // a footprint of a newer version
    let newer = with_footprint(&fp).replacen("\\\"version\\\":1", "\\\"version\\\":2", 1);
    assert_ne!(newer, with_footprint(&fp));
    assert!(g.index(&store(&newer).snapshot()).proven(&r).is_empty());
    // a second footprint the reader does not cover
    let wide = footprint(Some(&view(&[A, B], &[], c.clone())), &[A, B], &c);
    let mut two = with_footprint(&fp);
    for q in wide.quads(REPORTS) {
        two.push_str(&nq(&q));
    }
    assert!(g.index(&store(&two).snapshot()).proven(&r).is_empty());
    assert_eq!(
        g.index(&store(&two).snapshot())
            .proven(&reader(&[A, B, REPORTS])),
        vec![REPORTS.to_string()]
    );
}

#[test]
fn guarded_graphs_stay_hidden_until_bound() {
    let c = caller("reports", &[]);
    let fp = footprint(Some(&view(&[A], &[], c.clone())), &[A], &c);
    let g = Arc::new(Guard::new());
    let r = reader(&[A, REPORTS]);
    let v = Arc::new(apply(r.clone(), Some(&g)));
    assert!(!v.read.allows_iri(REPORTS), "unbound");
    assert!(v.read.allows_iri(A));
    // bound to a snapshot without the footprint: hidden
    let empty = store("<urn:g:a> { <http://ex/x> <http://ex/p> 1 . }");
    assert!(!v.bound(&empty.snapshot()).read.allows_iri(REPORTS));
    // the same view, bound to a later snapshot with the footprint: proven
    let s = store(&with_footprint(&fp));
    let b = v.bound(&s.snapshot());
    assert!(b.read.allows_iri(REPORTS));
    assert!(
        !b.write.allows_iri(REPORTS),
        "never written through a restricted view"
    );
    // without a guard nothing is proven
    let none = Arc::new(apply(r, None));
    assert!(!none.bound(&s.snapshot()).read.allows_iri(REPORTS));
    // an unrestricted view is left as it is
    let all = GraphAccess::graphs(Graphs::All, Graphs::All);
    assert_eq!(apply(all.clone(), Some(&g)), all);
}

/// A view resolved before a report and its footprint were committed is decided against
/// the snapshot it reads, so it never reads the new report under the old decision.
#[test]
fn a_view_resolved_before_a_commit_sees_the_new_footprint() {
    let c = caller("reports", &[]);
    let old = footprint(Some(&view(&[A], &[], c.clone())), &[A], &c);
    let s = store(&with_footprint(&old));
    let g = Arc::new(Guard::new());
    let v = Arc::new(apply(reader(&[A, REPORTS]), Some(&g)));
    assert!(v.bound(&s.snapshot()).read.allows_iri(REPORTS));
    // a build that also read B commits its report and footprint
    let new = footprint(Some(&view(&[A, B], &[], c.clone())), &[A, B], &c);
    let mut more = format!(
        "<{REPORTS}> {{ <http://ex/r2> <{}> <{}> . }}\n",
        gr("disclosure"),
        new.iri()
    );
    for q in new.quads(REPORTS) {
        more.push_str(&nq(&q));
    }
    s.load(&[Source::from_bytes(more.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();
    assert!(!v.bound(&s.snapshot()).read.allows_iri(REPORTS));
}

fn controlled_exemptions(
    g: &Arc<Guard>,
    snap: &Arc<Snapshot>,
    v: &GraphAccess,
    p: &sparkles::access::Preparation,
) -> sparkles::Result<Vec<String>> {
    let output = Decide {
        guard: g.clone(),
        view: v.clone(),
    }
    .exempt_controlled(snap, p)?;
    let mut values: Vec<_> = output.iter().map(str::to_string).collect();
    values.sort();
    Ok(values)
}

#[test]
fn controlled_provider_matches_real_guard_for_cold_and_cached_footprints_and_callers() {
    use sparkles::access::Preparation;
    use sparkles::task::Control;
    let c = caller("reports", &["north", "south"]);
    let p = protection(
        "salary",
        Some("?s <http://ex/owner> ?user . ?s <http://ex/group> ?group"),
    );
    let fp = footprint(
        Some(&view(&[A], std::slice::from_ref(&p), c.clone())),
        &[A],
        &c,
    );
    let wide = footprint(Some(&view(&[A, B], &[], c.clone())), &[A, B], &c);
    let mut two = with_footprint(&fp);
    for q in wide.quads(REPORTS) {
        two.push_str(&nq(&q));
    }
    let forged = with_footprint(&fp).replacen("\\\"version\\\":1", "\\\"version\\\":2", 1);
    let mut newer = fp.clone();
    newer.version = FOOTPRINT_VERSION + 1;
    let missing = format!(
        "<{REPORTS}> {{ <urn:r> <{}> <urn:missing> . }}",
        gr("disclosure")
    );
    let mut duplicate_literal = with_footprint(&fp);
    duplicate_literal.push_str(&format!(
        "<{DISCLOSURE_GRAPH}> {{ <{}> <{}> \"other\" . }}",
        fp.iri(),
        gr("footprint")
    ));
    let mut matching = c.clone();
    matching.groups = vec!["south".into(), "north".into(), "north".into()];
    let readers = [
        reader(&[A, REPORTS]),
        reader(&[A, B, REPORTS]),
        reader(&[B, REPORTS]),
        view(&[A, REPORTS], std::slice::from_ref(&p), matching),
        view(
            &[A, REPORTS],
            std::slice::from_ref(&p),
            caller("different", &["north", "south"]),
        ),
        GraphAccess::all(),
    ];
    for trig in [
        with_footprint(&fp),
        two,
        forged,
        with_footprint(&newer),
        missing,
        duplicate_literal,
    ] {
        let s = store(&trig);
        let snap = s.snapshot();
        for v in &readers {
            let mut expected = Guard::new().index(&snap).proven(v);
            expected.sort();
            for warm in [false, true] {
                let guard = Arc::new(Guard::new());
                if warm {
                    guard.index(&snap);
                }
                let actual = controlled_exemptions(
                    &guard,
                    &snap,
                    v,
                    &Preparation::new(Control::none(), 32 << 20).unwrap(),
                )
                .unwrap();
                assert_eq!(actual, expected, "warm={warm}");
            }
        }
        assert_eq!(s.snapshot().commit, snap.commit);
    }
}

#[test]
fn controlled_guarded_access_uses_actual_provider_without_changing_write_permissions() {
    use sparkles::access::Preparation;
    use sparkles::task::Control;
    let c = caller("reports", &[]);
    let fp = footprint(Some(&view(&[A], &[], c.clone())), &[A], &c);
    let s = store(&with_footprint(&fp));
    let g = Arc::new(Guard::new());
    let access = apply(
        GraphAccess::graphs(graphs(&[A, REPORTS]), graphs(&[A, REPORTS])),
        Some(&g),
    );
    let old = Arc::new(access.clone()).bound(&s.snapshot());
    let prepared = access
        .masked_controlled(
            &s.snapshot(),
            &Preparation::new(Control::none(), 16 << 20).unwrap(),
        )
        .unwrap();
    assert_eq!(prepared.access(), old.as_ref());
    assert!(prepared.access().read.allows_iri(REPORTS));
    assert!(prepared.access().write.allows_iri(A));
    assert!(!prepared.access().write.allows_iri(REPORTS));
    assert!(std::ptr::eq(prepared.snapshot(), s.snapshot().as_ref()));
}

#[test]
fn controlled_provider_cache_caps_and_real_lock_wait_observe_controls_before_release() {
    use sparkles::access::Preparation;
    use sparkles::task::{Control, Progress};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    let c = caller("reports", &[]);
    let fp = footprint(Some(&view(&[A], &[], c.clone())), &[A], &c);
    let s = store(&with_footprint(&fp));
    let snap = s.snapshot();
    let v = reader(&[A, REPORTS]);
    let guard = Arc::new(Guard::new());
    guard.index(&snap);
    let low = controlled_exemptions(
        &guard,
        &snap,
        &v,
        &Preparation::new(Control::none(), 256).unwrap(),
    );
    assert!(
        matches!(low, Err(sparkles::Error::BudgetExceeded(ref budget)) if budget.limit == 256 && budget.requested == 257)
    );
    for mode in 0..3 {
        let held = guard.indexes.lock();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let control = Control {
            progress: Progress::new(move |_, message| {
                if message == "waiting for view preparation" {
                    let _ = entered_tx.try_send(());
                }
            }),
            deadline: (mode == 1).then(|| Instant::now() + Duration::from_millis(300)),
            ..Control::none()
        };
        let deadline = control.deadline;
        let cancel = control.cancel.clone();
        let g = guard.clone();
        let snap = snap.clone();
        let view = v.clone();
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let prep = Preparation::new(control, 16 << 20)
                .unwrap()
                .with_no_wait(mode == 2);
            done_tx
                .send(controlled_exemptions(&g, &snap, &view, &prep))
                .unwrap();
        });
        let entered = if mode != 2 {
            entered_rx.recv_timeout(Duration::from_secs(2)).is_ok()
        } else {
            true
        };
        if mode == 0 {
            cancel.cancel();
        }
        if let Some(deadline) = deadline {
            while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                std::thread::park_timeout(remaining);
            }
        }
        let result = done_rx.recv_timeout(Duration::from_secs(2));
        drop(held);
        worker.join().unwrap();
        assert!(entered);
        let result =
            result.expect("provider did not settle while its real index mutex stayed held");
        match mode {
            0 => assert!(matches!(result, Err(sparkles::Error::Cancelled))),
            1 => assert!(matches!(result, Err(sparkles::Error::Timeout))),
            _ => assert!(
                matches!(result, Err(sparkles::Error::Invalid(ref message)) if message.contains("no_wait"))
            ),
        }
    }
}

#[test]
fn controlled_disclosure_row_temporaries_release_before_the_next_row() {
    use sparkles::access::Preparation;
    use sparkles::task::Control;
    let c = caller("reports", &[]);
    let fp = footprint(Some(&view(&[A], &[], c.clone())), &[A], &c);
    let mut data = with_footprint(&fp);
    for row in 0..12_000 {
        data.push_str(&format!(
            "<{REPORTS}> {{ <urn:report:{row}> <{}> <{}> . }}\n",
            gr("disclosure"),
            fp.iri()
        ));
    }
    let s = store(&data);
    s.compact().unwrap();
    let g = Arc::new(Guard::new());
    let result = controlled_exemptions(
        &g,
        &s.snapshot(),
        &reader(&[A, REPORTS]),
        &Preparation::new(Control::none(), 2 << 20).unwrap(),
    );
    assert_eq!(result.unwrap(), vec![REPORTS.to_string()]);
}

#[test]
#[ignore = "local controlled provider latency diagnostic"]
fn controlled_provider_preparation_cost_diagnostic() {
    use sparkles::access::Preparation;
    use sparkles::task::Control;
    use std::time::Instant;
    let c = caller("reports", &[]);
    let fp = footprint(Some(&view(&[A], &[], c.clone())), &[A], &c);
    for rows in [100, 2000] {
        let mut data = with_footprint(&fp);
        for row in 0..rows {
            data.push_str(&format!(
                "<{REPORTS}> {{ <urn:r{row}> <{}> <{}> . }}\n",
                gr("disclosure"),
                fp.iri()
            ));
        }
        let s = store(&data);
        s.compact().unwrap();
        let snapshot = s.snapshot();
        let access = reader(&[A, REPORTS]);
        for warm in [false, true] {
            for round in 0..5 {
                let old = Guard::new();
                let controlled = Arc::new(Guard::new());
                if warm {
                    old.index(&snapshot);
                    controlled.index(&snapshot);
                }
                let start = Instant::now();
                let mut expected = old.index(&snapshot).proven(&access);
                let old_ns = start.elapsed().as_nanos();
                expected.sort();
                let p = Preparation::new(Control::none(), 16 << 20).unwrap();
                let start = Instant::now();
                let actual = controlled_exemptions(&controlled, &snapshot, &access, &p).unwrap();
                let controlled_ns = start.elapsed().as_nanos();
                assert_eq!(actual, expected);
                eprintln!(
                    "provider cost rows={rows} warm={warm} round={round} legacy_ns={old_ns} controlled_ns={controlled_ns}"
                );
            }
        }
    }
}
