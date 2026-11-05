use super::super::classify::{ClassifyRequest, REFUSE_ADMISSION};
use super::*;
use crate::Dataset;
use crate::access::{GraphAccess, GraphRule, Graphs};
use crate::history::At;
use std::sync::mpsc;
use std::time::Duration;

const BASE: &str = "PREFIX : <http://ex.org/> PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>";
fn ds(body: &str) -> Dataset {
    let ds = Dataset::memory();
    ds.update(&format!("{BASE} INSERT DATA {{ {body} }}"))
        .unwrap();
    ds
}
fn persistent(body: &str) -> (tempfile::TempDir, Dataset) {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path().join("db")).unwrap();
    ds.update(&format!("{BASE} INSERT DATA {{ {body} }}"))
        .unwrap();
    (dir, ds)
}
fn req() -> OntologyRequest {
    OntologyRequest::default()
}
fn opts() -> QueryOptions {
    QueryOptions::default()
}
fn classify(ds: &Dataset) -> Arc<KeptClassification> {
    ds.ontology()
        .classify(&req(), &ClassifyRequest::default(), &opts())
        .unwrap()
        .publication
}
fn read(ds: &Dataset, r: &OntologyRequest, q: &KeptQuery) -> KeptRead {
    ds.ontology().kept_classification(r, q, &opts()).unwrap()
}
fn result(ds: &Dataset) -> Option<Arc<KeptClassification>> {
    ds.state().classify_cache.lock().publication.clone()
}
fn empty_slot(ds: &Dataset) {
    let slot = ds.state().classify_cache.lock();
    assert!(slot.entry.is_none());
    assert!(slot.publication.is_none());
    assert!(slot.active.is_none());
}

#[test]
fn a_kept_miss_does_not_map_initialize_or_admit() {
    let ds = ds(":A a owl:Class .");
    REFUSE_ADMISSION.with(|f| f.set(true));
    let read = read(&ds, &req(), &KeptQuery::default());
    assert_eq!(read.status, Status::Unavailable);
    assert!(read.outcome.is_none());
    assert!(REFUSE_ADMISSION.with(|f| f.replace(false)));
    empty_slot(&ds);
    assert!(ds.state().ontology_cache.lock().is_empty());
}

#[test]
fn producer_identity_survives_compatible_reuse_and_later_dl_inputs() {
    let (_dir, ds) = persistent(
        ":A a owl:Class ; rdfs:label \"old\" . :B a owl:Class . :A owl:equivalentClass :B .",
    );
    let c = ClassifyRequest {
        partial: true,
        expansion: Expansion::Eager,
        threads: 1,
        ..Default::default()
    };
    let first = ds.ontology().classify(&req(), &c, &opts()).unwrap();
    let id = first.publication.id;
    let commit = first.opened.snapshot.commit;
    let again = ds
        .ontology()
        .classify(&req(), &ClassifyRequest::default(), &opts())
        .unwrap();
    assert!(again.reused);
    assert_eq!(again.publication.id, id);
    assert!(again.publication.source.partial_requested);
    assert_eq!(again.publication.source.expansion, "eager");
    ds.update(&format!(
        "{BASE} DELETE {{ :A rdfs:label \"old\" }} INSERT {{ :A rdfs:label \"new\" }} WHERE {{}}"
    ))
    .unwrap();
    let mut q = super::super::dl_query::DlQueryRequest::new(sparkles_owl::ClassExpression::Class(
        sparkles_owl::Class::new("http://ex.org/A"),
    ));
    q.answers.instances = false;
    ds.ontology().dl_query(&req(), &q, &opts()).unwrap();
    assert_eq!(
        read(&ds, &req(), &KeptQuery::default()).status,
        Status::Mismatch
    );
    let r = OntologyRequest {
        at: Some(At::Commit(commit)),
        ..req()
    };
    let q = KeptQuery {
        about: Some("http://ex.org/B".into()),
        part: Part::Equivalents,
        result_id: Some(id),
        ..Default::default()
    };
    let old = read(&ds, &r, &q);
    assert_eq!(old.status, Status::Available);
    assert_eq!(old.page.unwrap().items[0]["label"], "old");
    assert_eq!(result(&ds).unwrap().source.commit, commit);
    assert!(
        !result(&ds)
            .unwrap()
            .content_fingerprint
            .contains("dl-query")
    );
}

#[test]
fn key_normalization_preserves_order_and_default_but_not_branch_identity() {
    let ds = ds(":A a owl:Class .");
    let h = ds.ontology();
    let snap = ds.store().snapshot();
    let a = InputKey::of(&h, &req(), &snap);
    assert_eq!(
        a,
        InputKey::of(
            &h,
            &OntologyRequest {
                graphs: vec![],
                ..req()
            },
            &snap
        )
    );
    let g = |s: &str| sparkles_reasoner::inputs::GraphRef::Named(s.into());
    let a = OntologyRequest {
        graphs: vec![g("urn:b"), g("urn:a"), g("urn:b")],
        ..req()
    };
    let key = InputKey::of(&h, &a, &snap);
    assert_eq!(key.graphs, ["urn:b", "urn:a"]);
    assert_ne!(
        key,
        InputKey::of(
            &h,
            &OntologyRequest {
                graphs: vec![g("urn:a"), g("urn:b")],
                ..req()
            },
            &snap
        )
    );
    let dir = tempfile::tempdir().unwrap();
    let family = Dataset::open(dir.path().join("db")).unwrap();
    family
        .update(&format!("{BASE} INSERT DATA {{ :A a owl:Class }}"))
        .unwrap();
    family
        .create_branch("left", &crate::branch::BranchOptions::default())
        .unwrap();
    family
        .create_branch("right", &crate::branch::BranchOptions::default())
        .unwrap();
    let left = family.branch("left").unwrap();
    let right = family.branch("right").unwrap();
    left.update(&format!("{BASE} INSERT DATA {{ :L a owl:Class }}"))
        .unwrap();
    right
        .update(&format!("{BASE} INSERT DATA {{ :R a owl:Class }}"))
        .unwrap();
    let l = classify(&left);
    let r = classify(&right);
    assert_eq!(l.source.commit, r.source.commit);
    assert_eq!(l.source.input.family_id, r.source.input.family_id);
    assert_ne!(l.source.input.bound_store_id, r.source.input.bound_store_id);
    assert_ne!(l.source.input_fingerprint, r.source.input_fingerprint);
    let id = l.source.input.bound_store_id;
    family.rename_branch("left", "renamed").unwrap();
    assert_eq!(family.branch("renamed").unwrap().store().branch_id(), id);
    assert_eq!(
        read(
            &right,
            &req(),
            &KeptQuery {
                result_id: Some(l.id),
                ..Default::default()
            }
        )
        .status,
        Status::Mismatch
    );
}

#[test]
fn an_old_pin_stays_available_while_a_new_exact_flight_is_running() {
    let (_dir, ds) = persistent(":A a owl:Class .");
    let old = classify(&ds);
    let n = old.source.commit;
    ds.update(&format!("{BASE} INSERT DATA {{ :B a owl:Class }}"))
        .unwrap();
    let h = ds.ontology();
    let o = h.open(&req(), &opts()).unwrap();
    let (start_tx, start_rx) = mpsc::channel();
    let (end_tx, end_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            h.with_kept(&o, &req(), &opts(), "dlQuery", |e| {
                start_tx.send(()).unwrap();
                end_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                e.run_over(&o, vec![], |_, _, _| Ok(()))?;
                Ok(((), true))
            })
        });
        start_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let r = OntologyRequest {
            at: Some(At::Commit(n)),
            ..req()
        };
        let pinned = ds.ontology().kept_classification(
            &r,
            &KeptQuery {
                result_id: Some(old.id),
                ..Default::default()
            },
            &opts(),
        );
        let now = ds
            .ontology()
            .kept_classification(&req(), &KeptQuery::default(), &opts());
        let unrelated = ds.ontology().kept_classification(
            &OntologyRequest {
                mode: sparkles_owl::read::Mode::Strict,
                ..req()
            },
            &KeptQuery::default(),
            &opts(),
        );
        end_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
        let pinned = pinned.unwrap();
        let now = now.unwrap();
        let unrelated = unrelated.unwrap();
        assert_eq!(pinned.status, Status::Available);
        assert!(pinned.activity.is_none());
        assert_eq!(now.status, Status::Running);
        assert!(now.outcome.is_none());
        assert_eq!(unrelated.status, Status::Unavailable);
        assert!(unrelated.activity.is_none());
    });
}

#[test]
fn restricted_status_never_discloses_cache_or_flight() {
    let ds = ds(":A a owl:Class . GRAPH <urn:hidden> { :Hidden a owl:Class }");
    let view = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["default"], &[])),
        Graphs::All,
    ));
    let options = QueryOptions {
        graphs: Some(view),
        ..opts()
    };
    let get = || {
        serde_json::to_value(
            ds.ontology()
                .kept_classification(&req(), &KeptQuery::default(), &options)
                .unwrap(),
        )
        .unwrap()
    };
    let before = get();
    let _p = classify(&ds);
    assert_eq!(get(), before);
    let h = ds.ontology();
    let o = h.open(&req(), &opts()).unwrap();
    h.with_kept(&o, &req(), &opts(), "classify", |_| {
        assert_eq!(get(), before);
        Ok(((), true))
    })
    .unwrap();
    assert_eq!(before["reason"], "unsupportedScope");
    assert!(before["result"].is_null());
}

#[test]
fn refusal_before_mutation_preserves_and_later_admission_works() {
    let ds = ds(":A a owl:Class .");
    let p = classify(&ds);
    ds.update(&format!("{BASE} INSERT DATA {{ :B a owl:Class }}"))
        .unwrap();
    REFUSE_ADMISSION.with(|f| f.set(true));
    assert!(matches!(
        ds.ontology()
            .classify(&req(), &ClassifyRequest::default(), &opts()),
        Err(Error::Timeout)
    ));
    assert!(Arc::ptr_eq(&p, &result(&ds).unwrap()));
    assert!(ds.state().classify_cache.lock().active.is_none());
    assert_ne!(classify(&ds).id, p.id);
}

#[test]
fn mutation_errors_and_unwinds_discard_both_owners_but_not_a_read_arc() {
    for panic in [false, true] {
        let ds = ds(":A a owl:Class .");
        let held = classify(&ds);
        let weak = Arc::downgrade(&held);
        let h = ds.ontology();
        let o = h.open(&req(), &opts()).unwrap();
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            h.with_kept(&o, &req(), &opts(), "dlQuery", |e| {
                e.run_over(&o, vec![], |s, axioms, changes| {
                    let par = super::super::classify::admit(1, &opts())?;
                    super::super::classify::saturate(s, axioms, changes, par, &opts())?;
                    if panic {
                        panic!("controlled mutation unwind");
                    }
                    Err::<(), _>(Error::invalid("controlled mutation error"))
                })?;
                Ok(((), true))
            })
        }));
        if panic {
            assert!(attempt.is_err());
        } else {
            assert!(attempt.unwrap().is_err());
        }
        empty_slot(&ds);
        assert!(weak.upgrade().is_some());
        let next = classify(&ds);
        assert!(!Arc::ptr_eq(&held, &next));
        drop(held);
        assert!(weak.upgrade().is_none());
    }
}

#[test]
fn retention_counts_auxiliary_bytes_once_and_taxonomy_eviction_releases_owners() {
    let ds = ds(":A a owl:Class .");
    let p = classify(&ds);
    let h = ds.ontology();
    let o = h.open(&req(), &opts()).unwrap();
    h.with_kept(&o, &req(), &opts(), "dlQuery", |e| {
        let base = p
            .classification
            .stats
            .memory
            .saturating_add(entry_aux_bytes(e));
        let mut fake = KeptClassification {
            id: p.id,
            source: p.source.clone(),
            classification: p.classification.clone(),
            excluded: p.excluded.clone(),
            content_fingerprint: p.content_fingerprint.clone(),
            auxiliary_bytes: CACHE_MAX_BYTES - base,
        };
        assert_eq!(classification_retained_bytes(e, &fake), CACHE_MAX_BYTES);
        fake.auxiliary_bytes += 1;
        assert!(classification_retained_bytes(e, &fake) > CACHE_MAX_BYTES);
        let tail = entry_aux_bytes(e) + p.classification.stats.taxonomy_memory + p.auxiliary_bytes;
        assert!(retain_dl(e, CACHE_MAX_BYTES - tail));
        assert!(e.result.is_some());
        assert!(retain_dl(e, CACHE_MAX_BYTES - tail + 1));
        assert!(e.result.is_none());
        Ok(((), true))
    })
    .unwrap();
    assert!(result(&ds).is_none());
    assert!(ds.state().classify_cache.lock().entry.is_some());
    assert!(!classify(&ds).classification.taxonomy.nodes().is_empty());
}

#[test]
fn publication_replacement_releases_the_old_pair_after_the_reader_finishes() {
    let ds = ds(":A a owl:Class .");
    let held = classify(&ds);
    let weak = Arc::downgrade(&held);
    ds.update(&format!("{BASE} INSERT DATA {{ :B a owl:Class }}"))
        .unwrap();
    let new = classify(&ds);
    assert!(!Arc::ptr_eq(&new, &held));
    assert!(weak.upgrade().is_some());
    drop(held);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        read(
            &ds,
            &req(),
            &KeptQuery {
                result_id: Some(new.id),
                ..Default::default()
            }
        )
        .status,
        Status::Available
    );
}

#[test]
fn equivalence_pages_are_bounded_pinned_and_sorted_even_with_bottom_builtins() {
    let mut body = String::from(":A a owl:Class .");
    for i in 0..241 {
        body.push_str(&format!(" :A owl:equivalentClass :C{i:03} ."));
    }
    let (_dir, ds) = persistent(&body);
    let p = classify(&ds);
    let r = OntologyRequest {
        at: Some(At::Commit(p.source.commit)),
        ..req()
    };
    let mut q = KeptQuery {
        part: Part::Equivalents,
        about: Some("http://ex.org/A".into()),
        limit: 7,
        result_id: Some(p.id),
        ..Default::default()
    };
    let mut items = Vec::new();
    loop {
        let page = read(&ds, &r, &q).page.unwrap();
        assert_eq!(page.total, 241);
        assert!(page.items.len() <= 7);
        items.extend(
            page.items
                .into_iter()
                .map(|i| i["iri"].as_str().unwrap().to_string()),
        );
        if let Some(next) = page.next_offset {
            q.offset = next;
        } else {
            break;
        }
    }
    assert_eq!(items.len(), 241);
    assert!(items.windows(2).all(|p| p[0] < p[1]));
    ds.update(&format!("{BASE} INSERT DATA {{ :Z a owl:Class }}"))
        .unwrap();
    let replacement = classify(&ds);
    assert_ne!(replacement.id, p.id);
    assert_eq!(read(&ds, &r, &q).status, Status::Mismatch);
    let inconsistent = self::ds(":A a owl:Class . owl:Thing rdfs:subClassOf owl:Nothing .");
    let p = classify(&inconsistent);
    let classes: Vec<_> = ordered_classes(&p.classification.taxonomy.bottom().classes)
        .map(|c| c.as_str())
        .collect();
    assert!(classes.windows(2).all(|p| p[0] < p[1]));
    assert_eq!(p.outcome().consistency, "inconsistent");
}

#[test]
fn bounded_display_records_exact_taxonomy_visits_and_returned_name_requests() {
    let mut body = ":A a owl:Class .".to_string();
    for i in 0..641 {
        body.push_str(&format!(" :A owl:equivalentClass :C{i:03} ."));
    }
    let ds = ds(&body);
    let c = ds
        .ontology()
        .classify(&req(), &ClassifyRequest::default(), &opts())
        .unwrap();
    let r = OntologyRequest {
        at: Some(At::Commit(c.opened.snapshot.commit)),
        ..req()
    };
    let mut measurements = Vec::new();
    let _ = diagnostic::take();
    let status = read(&ds, &r, &KeptQuery::default());
    let work = diagnostic::take();
    assert_eq!((work.equivalence_candidates, work.name_requests), (0, 0));
    measurements.push(json!({"part":"status","rows":0,"jsonBytes":serde_json::to_vec(&super::super::wire::kept_body("fixture", &status).unwrap()).unwrap().len(),"work":work}));
    for offset in [0, 100, 600] {
        let reply = read(
            &ds,
            &r,
            &KeptQuery {
                part: Part::Equivalents,
                about: Some("http://ex.org/A".into()),
                offset,
                limit: 7,
                result_id: Some(c.publication.id),
                ..Default::default()
            },
        );
        let work = diagnostic::take();
        let page = reply.page.as_ref().unwrap();
        assert_eq!((page.total, page.items.len()), (641, 7));
        assert_eq!(work.name_requests, 7);
        // Self is examined once; lexical offsets still scan earlier members.
        assert_eq!(work.equivalence_candidates, offset + 8);
        measurements.push(json!({"part":"equivalents","offset":offset,"total":page.total,"rows":page.items.len(),"jsonBytes":serde_json::to_vec(&super::super::wire::kept_body("fixture", &reply).unwrap()).unwrap().len(),"work":work}));
    }
    let frame = super::super::wire::classify_body(
        "fixture",
        &c,
        &r,
        &super::super::wire::renderer(&ds),
        &super::super::wire::ClassifyQuery {
            projection: Some(super::super::wire::ClassifyProjection::Frame),
            about: Some("http://ex.org/A".into()),
            limit: 7,
            ..Default::default()
        },
    )
    .unwrap();
    let work = diagnostic::take();
    assert_eq!(work.name_requests, 8);
    assert_eq!(work.equivalence_candidates, 8);
    assert_eq!(work.parent_candidates, 1);
    assert_eq!(work.source_rows, 2);
    assert_eq!(
        frame["frame"]["equivalents"]["items"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    measurements.push(json!({"part":"explicitFrame","jsonBytes":serde_json::to_vec(&frame).unwrap().len(),"work":work}));
    let legacy = super::super::wire::classify_body(
        "fixture",
        &c,
        &r,
        &super::super::wire::renderer(&ds),
        &super::super::wire::ClassifyQuery {
            about: Some("http://ex.org/A".into()),
            limit: 7,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(legacy["class"]["classes"].as_array().unwrap().len(), 642);
    measurements.push(json!({"part":"legacyExplicitClass","classMembersIncludingSelf":642,"jsonBytes":serde_json::to_vec(&legacy).unwrap().len(),"note":"legacy output is preserved; this records response cardinality, not server work or latency"}));
    println!(
        "kept-display-diagnostic {}",
        json!({"fixtureEquivalents":641,"limit":7,"scope":"taxonomy/source candidate visits and name requests only; excludes RDF/index work and latency","measurements":measurements})
    );
}

fn stopped_frame_control(partial: bool, composite: bool) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let ds = ds(":A a owl:Class ; owl:equivalentClass :E . :E a owl:Class .");
    let stopped = Arc::new(AtomicBool::new(false));
    let options = if composite {
        QueryOptions {
            cancellation: Some(crate::task::Cancellation::from_flags([stopped.clone()])),
            ..opts()
        }
    } else {
        QueryOptions {
            cancel: Some(stopped.clone()),
            ..opts()
        }
    };
    // This hook gives a deterministic terminal partial object. The separate,
    // unchanged deadline/budget tests execute actual stopped classification.
    super::super::classify::FORCE_PARTIAL.with(|f| f.set(partial));
    let classified = ds
        .ontology()
        .classify(
            &req(),
            &ClassifyRequest {
                partial: true,
                ..Default::default()
            },
            &options,
        )
        .unwrap();
    assert_eq!(classified.classification.complete, !partial);
    stopped.store(true, Ordering::Relaxed);
    let response = super::super::wire::classify_body(
        "fixture",
        &classified,
        &req(),
        &super::super::wire::renderer(&ds),
        &super::super::wire::ClassifyQuery {
            projection: Some(super::super::wire::ClassifyProjection::Frame),
            about: Some("http://ex.org/A".into()),
            limit: 1,
            ..Default::default()
        },
    );
    if partial {
        let response = response.unwrap();
        assert_eq!(response["frame"]["outcome"]["partial"], true);
        assert_eq!(response["frame"]["outcome"]["consistency"], "unknown");
        assert_eq!(
            response["frame"]["equivalents"]["items"][0]["iri"],
            "http://ex.org/E"
        );
        assert_eq!(response["frame"]["retention"]["kept"], false);
        empty_slot(&ds);
    } else {
        assert!(matches!(response, Err(Error::Cancelled)));
    }
    assert!(
        stopped.load(Ordering::Relaxed),
        "serialization must not reset a caller's shared stop flag"
    );
}
#[test]
fn stopped_partial_projection_flag_finishes_the_actual_partial_result() {
    stopped_frame_control(true, false);
}
#[test]
fn stopped_partial_projection_composite_finishes_the_actual_partial_result() {
    stopped_frame_control(true, true);
}
#[test]
fn stopped_partial_projection_flag_does_not_bypass_complete_result_cancellation() {
    stopped_frame_control(false, false);
}
#[test]
fn stopped_partial_projection_composite_does_not_bypass_complete_result_cancellation() {
    stopped_frame_control(false, true);
}

#[test]
fn idle_legacy_reuse_does_not_waive_new_read_or_projection_deadlines() {
    let ds = ds(":A a owl:Class .");
    let first = classify(&ds);
    let late = QueryOptions {
        deadline: Some(std::time::Instant::now()),
        ..opts()
    };
    let c = ds
        .ontology()
        .classify(&req(), &ClassifyRequest::default(), &late)
        .unwrap();
    assert!(c.reused);
    assert_eq!(c.publication.id, first.id);
    assert!(matches!(
        ds.ontology()
            .kept_classification(&req(), &KeptQuery::default(), &late),
        Err(Error::Timeout)
    ));
    let body = super::super::wire::classify_body(
        "fixture",
        &c,
        &req(),
        &super::super::wire::renderer(&ds),
        &super::super::wire::ClassifyQuery {
            projection: Some(super::super::wire::ClassifyProjection::Frame),
            about: Some("http://ex.org/A".into()),
            limit: 100,
            ..Default::default()
        },
    );
    assert!(matches!(body, Err(Error::Timeout)));
}

#[test]
fn raw_ontology_selectors_do_not_treat_a_null_body_as_an_empty_object() {
    for op in ["classify", "keptClassification"] {
        assert!(
            super::super::wire::decode_arguments(op, b"null").is_err(),
            "{op}"
        );
    }
}

#[test]
fn sources_include_typed_warning_tail_and_reconstruct_the_status_digest() {
    let ds =
        ds(":o a owl:Ontology ; owl:imports <urn:missing:1>, <urn:missing:2> . :A a owl:Class .");
    let p = classify(&ds);
    assert!(!p.source.warnings.is_empty());
    let r = OntologyRequest {
        at: Some(At::Commit(p.source.commit)),
        ..req()
    };
    let mut q = KeptQuery {
        part: Part::Sources,
        limit: 2,
        result_id: Some(p.id),
        ..Default::default()
    };
    let mut rows = Vec::new();
    loop {
        let page = read(&ds, &r, &q).page.unwrap();
        assert!(page.items.len() <= 2);
        rows.extend(page.items);
        if let Some(next) = page.next_offset {
            q.offset = next;
        } else {
            break;
        }
    }
    assert_eq!(rows.len(), p.source.source_count());
    assert_eq!(
        rows.iter().filter(|r| r["kind"] == "warning").count(),
        p.source.warnings.len()
    );
    assert_eq!(rows.last().unwrap()["kind"], "warning");
    let mut hash = start_hash(b"sparkles-classification-sources-v1\n");
    for row in rows {
        hash_json(&mut hash, &row).unwrap();
    }
    assert_eq!(finish_hash(hash), p.source.source_fingerprint);
}

#[test]
fn strict_core_queries_cancel_and_reject_unsafe_continuations() {
    let ds = ds(":A a owl:Class .");
    for q in [
        KeptQuery {
            limit: 0,
            ..Default::default()
        },
        KeptQuery {
            limit: 1001,
            ..Default::default()
        },
        KeptQuery {
            part: Part::Parents,
            ..Default::default()
        },
        KeptQuery {
            offset: 1,
            ..Default::default()
        },
    ] {
        assert!(
            ds.ontology()
                .kept_classification(&req(), &q, &opts())
                .is_err()
        );
    }
    let p = classify(&ds);
    let cancelled = QueryOptions {
        cancel: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
        ..opts()
    };
    assert!(matches!(
        ds.ontology()
            .kept_classification(&req(), &KeptQuery::default(), &cancelled),
        Err(Error::Cancelled)
    ));
    let r = read(
        &ds,
        &req(),
        &KeptQuery {
            about: Some("urn:absent".into()),
            ..Default::default()
        },
    );
    assert_eq!(r.status, Status::Available);
    assert!(!r.entity.unwrap().present);
    assert_eq!(r.result.unwrap().result_id, p.id);
}

#[test]
fn a_publication_does_not_resurrect_pruned_memory_history() {
    let ds = ds(":A a owl:Class .");
    let p = classify(&ds);
    ds.update(&format!("{BASE} INSERT DATA {{ :B a owl:Class }}"))
        .unwrap();
    let old = OntologyRequest {
        at: Some(At::Commit(p.source.commit)),
        ..req()
    };
    assert!(matches!(
        ds.ontology()
            .kept_classification(&old, &KeptQuery::default(), &opts()),
        Err(Error::HistoryGone(_))
    ));
}

#[test]
fn cancellation_while_waiting_for_flight_leaves_the_proven_pair_untouched() {
    let ds = ds(":A a owl:Class .");
    let p = classify(&ds);
    let h = ds.ontology();
    let o = h.open(&req(), &opts()).unwrap();
    let held = ds.state().classify_flight.lock();
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let flag = cancel.clone();
        let worker = scope.spawn(move || {
            super::super::classify::FLIGHT_WAIT_NOTIFY.with(|n| *n.borrow_mut() = Some(tx));
            let options = QueryOptions {
                cancel: Some(flag),
                ..opts()
            };
            h.with_kept(&o, &req(), &options, "classify", |_| Ok(((), true)))
        });
        let waiting = rx.recv_timeout(Duration::from_secs(5));
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let outcome = worker.join().unwrap();
        assert!(waiting.is_ok());
        assert!(matches!(outcome, Err(Error::Cancelled)));
    });
    drop(held);
    assert!(Arc::ptr_eq(&result(&ds).unwrap(), &p));
    assert!(ds.state().classify_cache.lock().active.is_none());
    assert_eq!(classify(&ds).id, p.id);
}

#[test]
fn stopped_partial_and_oversized_outcomes_drop_both_retained_owners() {
    struct CapRestore(usize);
    impl Drop for CapRestore {
        fn drop(&mut self) {
            TEST_RETENTION_CAP.with(|c| c.set(self.0));
        }
    }
    for partial in [false, true] {
        let ds = ds(":A a owl:Class .");
        let held = classify(&ds);
        let weak = Arc::downgrade(&held);
        ds.update(&format!("{BASE} INSERT DATA {{ :B a owl:Class }}"))
            .unwrap();
        let cap = TEST_RETENTION_CAP.with(|c| c.replace(if partial { CACHE_MAX_BYTES } else { 0 }));
        let restore = CapRestore(cap);
        super::super::classify::FORCE_PARTIAL.with(|f| f.set(partial));
        let terminal = classify(&ds);
        assert_eq!(!terminal.classification.complete, partial);
        empty_slot(&ds);
        assert_eq!(
            read(&ds, &req(), &KeptQuery::default()).status,
            Status::Unavailable
        );
        drop(restore);
        drop(held);
        assert!(weak.upgrade().is_none());
        assert!(classify(&ds).classification.complete);
        assert!(result(&ds).is_some());
    }
}

#[test]
fn replacing_the_input_scope_evicts_the_old_pair_and_never_discloses_it() {
    let ds = ds(":A a owl:Class .");
    let held = classify(&ds);
    let weak = Arc::downgrade(&held);
    let strict = OntologyRequest {
        mode: sparkles_owl::read::Mode::Strict,
        ..req()
    };
    let second = ds
        .ontology()
        .classify(&strict, &ClassifyRequest::default(), &opts())
        .unwrap();
    assert_ne!(second.publication.id, held.id);
    let unrelated = read(&ds, &req(), &KeptQuery::default());
    assert_eq!(unrelated.status, Status::Unavailable);
    assert!(unrelated.result.is_none());
    drop(held);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        read(&ds, &strict, &KeptQuery::default()).status,
        Status::Available
    );
}

#[test]
fn wire_contract_kept_miss_is_read_only_and_has_unknown_outcome() {
    let ds = ds(":A a owl:Class .");
    REFUSE_ADMISSION.with(|f| f.set(true));
    let answer = super::super::wire::call(&ds, "keptClassification", &json!({}), &opts()).unwrap();
    assert_eq!(answer["status"], "unavailable");
    assert!(answer["outcome"].is_null());
    assert!(REFUSE_ADMISSION.with(|f| f.replace(false)));
    empty_slot(&ds);
}

#[test]
fn wire_contract_frame_accepts_harmless_native_defaults_and_caps_each_relation() {
    let ds =
        ds(":A a owl:Class . :B a owl:Class . :C a owl:Class . :A owl:equivalentClass :B, :C .");
    let answer = super::super::wire::call(&ds, "classify", &json!({"projection":"frame","about":"http://ex.org/A","nodes":false,"offset":0,"target":null,"limit":1}), &opts()).unwrap();
    assert!(answer.get("class").is_none());
    assert!(answer.get("nodes").is_none());
    assert_eq!(
        answer["frame"]["equivalents"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(answer["frame"]["equivalents"]["total"], 2);
    assert_eq!(answer["frame"]["equivalents"]["truncated"], true);
    assert!(answer["frame"]["equivalents"].get("next").is_none());
}

#[test]
fn wire_contract_legacy_paging_has_actual_source_and_content_identity() {
    let ds = ds(":A a owl:Class . :B a owl:Class . :A rdfs:subClassOf :B .");
    let answer =
        super::super::wire::call(&ds, "classify", &json!({"nodes":true,"limit":1}), &opts())
            .unwrap();
    assert!(
        answer["classificationSource"]["inputFingerprint"]
            .as_str()
            .is_some_and(|s| s.starts_with("sha256:"))
    );
    assert!(
        answer["classificationSource"]["contentFingerprint"]
            .as_str()
            .is_some_and(|s| s.starts_with("sha256:"))
    );
    assert_eq!(answer["classificationSource"]["commit"], answer["commit"]);
    assert!(answer["nodes"].is_array());
}

#[test]
fn wire_frame_rejects_invalid_selectors_before_admission_mapping_or_writes() {
    let ds = ds(":A a owl:Class .");
    let commit = ds.store().snapshot().commit;
    let base = json!({"projection":"frame","about":"http://ex.org/A","limit":1});
    for (key, value) in [
        ("projection", json!("other")),
        ("about", J::Null),
        ("about", json!("relative")),
        ("limit", json!(0)),
        ("limit", json!(1001)),
        ("nodes", json!(true)),
        ("offset", json!(1)),
        ("target", json!("urn:write")),
    ] {
        let mut a = base.clone();
        a[key] = value;
        REFUSE_ADMISSION.with(|f| f.set(true));
        let outcome = super::super::wire::call(&ds, "classify", &a, &opts());
        assert!(
            REFUSE_ADMISSION.with(|f| f.replace(false)),
            "admission consumed for {a}"
        );
        assert!(matches!(outcome, Err(Error::Invalid(_))), "{a}");
        empty_slot(&ds);
        assert!(ds.state().ontology_cache.lock().is_empty());
        assert_eq!(ds.store().snapshot().commit, commit);
    }
    for defaults in [
        json!({"nodes":false,"offset":0,"target":null,"lang":null}),
        json!({"nodes":null,"offset":null,"target":null}),
    ] {
        let mut a = base.clone();
        a.as_object_mut()
            .unwrap()
            .extend(defaults.as_object().unwrap().clone());
        assert!(
            super::super::wire::call(&ds, "classify", &a, &opts()).unwrap()["frame"].is_object()
        );
    }
    let legacy = super::super::wire::call(
        &ds,
        "classify",
        &json!({"about":"http://ex.org/A"}),
        &opts(),
    )
    .unwrap();
    assert!(legacy["class"]["classes"].is_array());
    assert!(legacy["class"]["superClasses"].is_array());
}

#[test]
fn fingerprints_compare_actual_content_and_policies_without_run_ids_or_timing() {
    let ds = ds(":A a owl:Class . :B a owl:Class . :A rdfs:subClassOf :B .");
    let mut c = ClassifyRequest {
        assertions: true,
        threads: 1,
        ..Default::default()
    };
    let a = ds.ontology().classify(&req(), &c, &opts()).unwrap();
    c.threads = 2;
    c.expansion = Expansion::Eager;
    c.partial = true;
    let b = ds.ontology().classify(&req(), &c, &opts()).unwrap();
    assert!(!a.retained && !b.retained);
    assert_ne!(a.publication.id, b.publication.id);
    assert_eq!(
        a.publication.source.input_fingerprint,
        b.publication.source.input_fingerprint
    );
    assert_eq!(
        a.publication.content_fingerprint,
        b.publication.content_fingerprint
    );
    let mut timing = a.classification.as_ref().clone();
    timing.stats.threads += 1;
    timing.stats.saturation += Duration::from_secs(99);
    assert_eq!(
        content_fingerprint(&timing, &a.excluded, &opts()).unwrap(),
        a.publication.content_fingerprint
    );
    timing.complete = false;
    timing.consistency = sparkles_owl::classify::Consistency::Unknown;
    assert_ne!(
        content_fingerprint(&timing, &a.excluded, &opts()).unwrap(),
        a.publication.content_fingerprint
    );
    let other = self::ds(":A a owl:Class . :B a owl:Class . :B rdfs:subClassOf :A .");
    let mut taxonomy = a.classification.as_ref().clone();
    taxonomy.taxonomy = classify(&other).classification.taxonomy.clone();
    assert_ne!(
        content_fingerprint(&taxonomy, &a.excluded, &opts()).unwrap(),
        a.publication.content_fingerprint
    );
}

#[test]
fn isolated_frame_records_actual_flags_and_never_advertises_a_shared_pin() {
    let ds = ds(":A a owl:Class ; rdfs:label \"Scoped\"@en .");
    for (assertions, el_only) in [(true, false), (false, true)] {
        let c = ds
            .ontology()
            .classify(
                &req(),
                &ClassifyRequest {
                    assertions,
                    el_only,
                    ..Default::default()
                },
                &opts(),
            )
            .unwrap();
        let q = super::super::wire::ClassifyQuery {
            projection: Some(super::super::wire::ClassifyProjection::Frame),
            about: Some("http://ex.org/A".into()),
            limit: 1,
            ..Default::default()
        };
        let frame = super::super::wire::classify_body(
            "test",
            &c,
            &req(),
            &super::super::wire::renderer(&ds),
            &q,
        )
        .unwrap();
        assert_eq!(frame["frame"]["result"]["assertions"], assertions);
        assert_eq!(frame["frame"]["result"]["elOnly"], el_only);
        assert_eq!(frame["frame"]["retention"]["kept"], false);
        assert!(frame["frame"]["retention"]["resultId"].is_null());
        assert_eq!(
            read(&ds, &req(), &KeptQuery::default()).status,
            Status::Unavailable
        );
    }
}

#[test]
fn scoped_frame_keeps_authorized_clauses_and_names_without_borrowing_shared_state() {
    let ds = ds("GRAPH :open { :o a owl:Ontology ; owl:imports :other .
        :A a owl:Class ; owl:equivalentClass :E . :E a owl:Class ; rdfs:label \"Visible\"@en . }
        GRAPH :other { :other a owl:Ontology . :B a owl:Class . :A rdfs:subClassOf :B .
        :E rdfs:label \"Hidden\"@en . }");
    let req = OntologyRequest {
        graphs: vec![sparkles_reasoner::inputs::GraphRef::Named(
            "http://ex.org/open".into(),
        )],
        ..req()
    };
    let shared = ds
        .ontology()
        .classify(&req, &ClassifyRequest::default(), &opts())
        .unwrap();
    let allowed = Graphs::Only(GraphRule::new(["http://ex.org/open"], &[]));
    for scoped in [
        crate::handles::ontology::tests::hiding("http://www.w3.org/2002/07/owl#imports"),
        QueryOptions {
            graphs: Some(Arc::new(GraphAccess::graphs(allowed.clone(), allowed))),
            ..opts()
        },
    ] {
        let before = ds
            .ontology()
            .kept_classification(&req, &KeptQuery::default(), &scoped)
            .unwrap();
        assert_eq!(before.reason, Some("unsupportedScope"));
        let c = ds
            .ontology()
            .classify(&req, &ClassifyRequest::default(), &scoped)
            .unwrap();
        assert_eq!(c.opened.graphs.len(), 1);
        let frame = super::super::wire::classify_body(
            "test",
            &c,
            &req,
            &super::super::wire::renderer(&ds),
            &super::super::wire::ClassifyQuery {
                projection: Some(super::super::wire::ClassifyProjection::Frame),
                about: Some("http://ex.org/A".into()),
                languages: vec!["en".into()],
                limit: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            frame["frame"]["equivalents"]["items"][0]["iri"],
            "http://ex.org/E"
        );
        assert_eq!(
            frame["frame"]["equivalents"]["items"][0]["label"],
            "Visible"
        );
        assert_eq!(
            frame["frame"]["parents"]["items"][0]["iri"],
            sparkles_owl::vocab::OWL_THING
        );
        assert_eq!(
            frame["frame"]["result"]["sourceCounts"]["resolvedGraphs"],
            1
        );
        assert_eq!(frame["frame"]["retention"]["kept"], false);
        assert!(frame["frame"]["retention"]["resultId"].is_null());
        assert_eq!(
            read(&ds, &req, &KeptQuery::default())
                .result
                .unwrap()
                .result_id,
            shared.publication.id
        );
    }
    // Triple-masked names must use the already authorized snapshot as well.
    let masked =
        crate::handles::ontology::tests::hiding("http://www.w3.org/2000/01/rdf-schema#label");
    let c = ds
        .ontology()
        .classify(&req, &ClassifyRequest::default(), &masked)
        .unwrap();
    let frame = super::super::wire::classify_body(
        "test",
        &c,
        &req,
        &super::super::wire::renderer(&ds),
        &super::super::wire::ClassifyQuery {
            projection: Some(super::super::wire::ClassifyProjection::Frame),
            about: Some("http://ex.org/A".into()),
            limit: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(frame["frame"]["equivalents"]["items"][0]["label"].is_null());
}

#[test]
fn raw_and_structured_kept_selectors_reject_duplicates_overflow_and_inferences() {
    use super::super::wire::{call, decode_arguments};
    let ds = ds(":A a owl:Class .");
    assert!(decode_arguments("keptClassification", br#"{"limit":1,"limit":2}"#).is_err());
    assert!(
        decode_arguments(
            "classify",
            br#"{"projection":"frame","nodes":false,"nodes":true}"#
        )
        .is_err()
    );
    for a in [
        json!({"inferences":false}),
        json!({"part":"other"}),
        json!({"resultId":"not-id"}),
        json!({"limit":0}),
        json!({"offset":"18446744073709551616"}),
        json!({"threads":1}),
        json!({"target":"urn:write"}),
    ] {
        assert!(
            matches!(
                call(&ds, "keptClassification", &a, &opts()),
                Err(Error::Invalid(_))
            ),
            "{a}"
        );
    }
    empty_slot(&ds);
    assert!(ds.state().ontology_cache.lock().is_empty());
}
