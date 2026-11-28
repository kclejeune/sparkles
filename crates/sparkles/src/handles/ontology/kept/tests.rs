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
    ds.state()
        .classify_cache
        .lock()
        .slot(false)
        .publication
        .clone()
}
fn result_for(ds: &Dataset, el_only: bool) -> Option<Arc<KeptClassification>> {
    ds.state()
        .classify_cache
        .lock()
        .slot(el_only)
        .publication
        .clone()
}
fn empty_slot(ds: &Dataset) {
    let cache = ds.state().classify_cache.lock();
    for el_only in [false, true] {
        let slot = cache.slot(el_only);
        assert!(slot.entry.is_none());
        assert!(slot.publication.is_none());
        assert!(slot.active.is_none());
    }
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
            h.with_kept(&o, &req(), false, &opts(), "dlQuery", |e| {
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
    h.with_kept(&o, &req(), false, &opts(), "classify", |_| {
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
    assert!(
        ds.state()
            .classify_cache
            .lock()
            .slot(false)
            .active
            .is_none()
    );
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
            h.with_kept(&o, &req(), false, &opts(), "dlQuery", |e| {
                e.run_over(&o, vec![], |s, axioms, changes| {
                    let par = super::super::classify::admit(1, &opts())?;
                    super::super::classify::saturate(s, axioms, changes, par, false, &opts())?;
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
    h.with_kept(&o, &req(), false, &opts(), "dlQuery", |e| {
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
        assert_eq!(e.retained_bytes, CACHE_MAX_BYTES);
        assert!(retain_dl(e, CACHE_MAX_BYTES - tail + 1));
        assert!(e.result.is_none());
        assert_eq!(
            e.retained_bytes,
            dl_retained_bytes(e, CACHE_MAX_BYTES - tail + 1)
        );
        assert!(e.retained_bytes < CACHE_MAX_BYTES);
        Ok(((), true))
    })
    .unwrap();
    assert!(result(&ds).is_none());
    assert!(ds.state().classify_cache.lock().slot(false).entry.is_some());
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
    assert_eq!(classified.classification.complete_for_taken, !partial);
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

/// A run whose only gap is a triple that maps to no axiom, here the domain of a property
/// that is not declared, names the count in its outcome, since no `excluded` entry
/// names such a triple.
#[test]
fn the_outcome_counts_the_triples_that_map_to_no_axiom() {
    let ds = ds(":A a owl:Class . :B a owl:Class . :B rdfs:subClassOf :A . :p rdfs:domain :A .");
    let frame = super::super::wire::call(
        &ds,
        "classify",
        &json!({"projection":"frame","about":"http://ex.org/B","limit":10}),
        &opts(),
    )
    .unwrap();
    let outcome = &frame["frame"]["outcome"];
    assert_eq!(outcome["complete"], false, "{outcome}");
    assert_eq!(outcome["excludedCount"], 0, "{outcome}");
    assert!(outcome["unmappedCount"].as_u64().unwrap() > 0, "{outcome}");
}

#[test]
fn bounded_exclusions_have_typed_gaps_and_preserve_legacy_shape() {
    let ds = ds(
        ":A a owl:Class . :Opaque a rdfs:Datatype . :p a owl:DatatypeProperty ; rdfs:range :Opaque .",
    );
    let frame = super::super::wire::call(
        &ds,
        "classify",
        &json!({"projection":"frame","about":"http://ex.org/A","limit":1}),
        &opts(),
    )
    .unwrap();
    assert!(frame["frame"]["outcome"]["excludedCount"].as_u64().unwrap() > 0);
    assert_eq!(
        frame["frame"]["excluded"]["items"][0]["gap"]["kind"],
        "partiallyUsed"
    );
    assert!(frame["frame"]["excluded"]["items"][0]["gap"]["reason"].is_string());
    let legacy = super::super::wire::call(
        &ds,
        "classify",
        &json!({"about":"http://ex.org/A","limit":1}),
        &opts(),
    )
    .unwrap();
    assert!(legacy["excluded"][0]["partial"].is_string());
    assert!(legacy["excluded"][0].get("gap").is_none());
    assert_eq!(
        legacy["classificationSource"],
        frame["classificationSource"]
    );
}

#[test]
fn unrelated_legacy_raw_null_reads_keep_empty_argument_compatibility() {
    let ds = ds(":A a owl:Class .");
    // Compare equivalent cached reads, rather than the first call's cache-hit bit.
    ds.ontology().open(&req(), &opts()).unwrap();
    for operation in ["summary", "entities"] {
        let arguments = super::super::wire::decode_arguments(operation, b"null").unwrap();
        let actual = super::super::wire::call(&ds, operation, &arguments, &opts()).unwrap();
        let empty = super::super::wire::call(&ds, operation, &json!({}), &opts()).unwrap();
        assert_eq!(actual, empty);
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
            h.with_kept(&o, &req(), false, &options, "classify", |_| Ok(((), true)))
        });
        let waiting = rx.recv_timeout(Duration::from_secs(5));
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let outcome = worker.join().unwrap();
        assert!(waiting.is_ok());
        assert!(matches!(outcome, Err(Error::Cancelled)));
    });
    drop(held);
    assert!(Arc::ptr_eq(&result(&ds).unwrap(), &p));
    assert!(
        ds.state()
            .classify_cache
            .lock()
            .slot(false)
            .active
            .is_none()
    );
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
        assert_eq!(!terminal.classification.complete_for_taken, partial);
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
    // the count of unmapped triples is part of the outcome, so two answers that differ
    // only in it differ
    let mut unmapped = timing.clone();
    unmapped.stats.unmapped = 1;
    let one = content_fingerprint(&unmapped, &a.excluded, &opts()).unwrap();
    unmapped.stats.unmapped = 2;
    assert_ne!(
        content_fingerprint(&unmapped, &a.excluded, &opts()).unwrap(),
        one
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
    for (assertions, skip_coverage) in [(true, false), (false, true)] {
        let c = ds
            .ontology()
            .classify(
                &req(),
                &ClassifyRequest {
                    assertions,
                    skip_coverage,
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
        assert_eq!(frame["frame"]["result"]["elOnly"], false);
        assert_eq!(frame["frame"]["retention"]["kept"], false);
        assert!(frame["frame"]["retention"]["resultId"].is_null());
        assert_eq!(
            read(&ds, &req(), &KeptQuery::default()).status,
            Status::Unavailable
        );
    }
    // EL-only runs are kept under their own el_only slot and remain invisible to default reads.
    let el = ds
        .ontology()
        .classify(
            &req(),
            &ClassifyRequest {
                el_only: true,
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
        &el,
        &req(),
        &super::super::wire::renderer(&ds),
        &q,
    )
    .unwrap();
    assert_eq!(frame["frame"]["result"]["assertions"], false);
    assert_eq!(frame["frame"]["result"]["elOnly"], true);
    assert_eq!(frame["frame"]["retention"]["kept"], true);
    assert_eq!(
        frame["frame"]["retention"]["resultId"],
        json!(el.publication.id)
    );
    assert_eq!(
        read(&ds, &req(), &KeptQuery::default()).status,
        Status::Unavailable
    );
    let kept_el = read(
        &ds,
        &req(),
        &KeptQuery {
            el_only: true,
            ..Default::default()
        },
    );
    assert_eq!(kept_el.status, Status::Available);
    assert!(kept_el.result.unwrap().el_only);
}

#[test]
fn projection_binding_does_not_upgrade_unprepared_write_guards() {
    use crate::access::{Decider, GuardDecision};
    struct WriteGuard;
    impl GuardDecision for WriteGuard {
        fn exempt(&self, _snap: &Arc<Snapshot>) -> Vec<String> {
            vec!["http://ex.org/open".into(), "http://ex.org/output".into()]
        }
    }
    let ds = ds("GRAPH :open { :A a owl:Class . }");
    let guard = Arc::new(WriteGuard);
    let options = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::All,
            Graphs::All.guarded_by("http://ex.org/*", Decider(guard.clone())),
        ))),
        ..opts()
    };
    let request = OntologyRequest {
        graphs: vec![sparkles_reasoner::inputs::GraphRef::Named(
            "http://ex.org/open".into(),
        )],
        ..req()
    };
    let head = ds.snapshot().commit;
    let outcome = ds.ontology().classify(
        &request,
        &ClassifyRequest {
            target: Some(sparkles_reasoner::inputs::GraphRef::Named(
                "http://ex.org/output".into(),
            )),
            ..Default::default()
        },
        &options,
    );
    assert!(matches!(outcome, Err(Error::NotPermitted(_))));
    assert_eq!(ds.snapshot().commit, head);
}

#[test]
fn isolated_names_never_redecide_graph_guards_after_triple_masking() {
    use crate::access::{Decider, GuardDecision};
    use crate::index::Perm;
    struct Policy {
        decisions: std::sync::Mutex<Vec<bool>>,
    }
    impl GuardDecision for Policy {
        fn exempt(&self, snap: &Arc<Snapshot>) -> Vec<String> {
            let policy_present = snap
                .lookup_iri("http://ex.org/policy")
                .is_some_and(|p| snap.count(Perm::Pos, &[p.0]).unwrap() > 0);
            self.decisions.lock().unwrap().push(policy_present);
            let mut allowed = vec!["http://ex.org/open".into()];
            if !policy_present {
                allowed.push("http://ex.org/other".into());
            }
            allowed
        }
    }
    let ds = ds("GRAPH :open { :o a owl:Ontology ; owl:imports :other .
        :o :policy true . :A a owl:Class ; owl:equivalentClass :E .
        :E a owl:Class ; rdfs:label \"Visible\"@en . }
        GRAPH :other { :other a owl:Ontology . :B a owl:Class .
        :A rdfs:subClassOf :B . :E rdfs:label \"Hidden\"@en . }");
    let policy = Arc::new(Policy {
        decisions: std::sync::Mutex::new(Vec::new()),
    });
    let mut options = crate::handles::ontology::tests::hiding("http://ex.org/policy");
    let access = Arc::make_mut(options.graphs.as_mut().unwrap());
    access.read = Graphs::All.guarded_by("http://ex.org/*", Decider(policy.clone()));
    access.write = Graphs::none();
    let request = OntologyRequest {
        graphs: vec![sparkles_reasoner::inputs::GraphRef::Named(
            "http://ex.org/open".into(),
        )],
        ..req()
    };
    let shared = ds
        .ontology()
        .classify(&request, &ClassifyRequest::default(), &opts())
        .unwrap()
        .publication;
    assert_eq!(
        ds.ontology()
            .kept_classification(&request, &KeptQuery::default(), &options)
            .unwrap()
            .reason,
        Some("unsupportedScope")
    );
    let mut c = ds
        .ontology()
        .classify(&request, &ClassifyRequest::default(), &options)
        .unwrap();
    assert_eq!(c.opened.graphs.len(), 1);
    let frame = super::super::wire::classify_body(
        "fixture",
        &c,
        &request,
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
    assert_eq!(*policy.decisions.lock().unwrap(), vec![true]);
    assert!(!frame.to_string().contains("Hidden"));
    assert_eq!(frame["frame"]["retention"]["kept"], false);
    // A deterministic terminal partial object exercises the same post-stop result
    // options; the unchanged deadline cases separately execute real stopped runs.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut stopped_options = c.read_options.clone();
    stopped_options.cancel = Some(stop.clone());
    c.read_options = super::super::classify::result_options(&stopped_options, false);
    let mut partial = (*c.classification).clone();
    partial.complete = false;
    partial.complete_for_taken = false;
    partial.consistency = sparkles_owl::classify::Consistency::Unknown;
    c.classification = Arc::new(partial);
    c.publication = KeptClassification::capture(
        &ds.ontology(),
        &request,
        &ClassifyRequest {
            partial: true,
            ..Default::default()
        },
        &c.opened,
        c.classification.clone(),
        c.excluded.clone(),
        &c.read_options,
    )
    .unwrap();
    let partial_frame = super::super::wire::classify_body(
        "fixture",
        &c,
        &request,
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
    assert_eq!(partial_frame["frame"]["outcome"]["partial"], true);
    assert_eq!(
        partial_frame["frame"]["equivalents"]["items"][0]["label"],
        "Visible"
    );
    assert!(!partial_frame.to_string().contains("Hidden"));
    assert_eq!(*policy.decisions.lock().unwrap(), vec![true]);
    assert!(stop.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(result(&ds).unwrap().id, shared.id);
    assert_eq!(
        ds.ontology()
            .kept_classification(&request, &KeptQuery::default(), &options)
            .unwrap()
            .reason,
        Some("unsupportedScope")
    );
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

/// A kept classification whose modules' pass the deadline stopped answers with every
/// class incomplete but publishes nothing, and keeps its saturation, so the next request
/// takes the pass again without saturating from the start.
#[test]
fn a_stopped_coverage_pass_keeps_the_saturation_and_publishes_nothing() {
    let ds = ds(":A a owl:Class ; rdfs:subClassOf :B . :B a owl:Class .");
    super::super::classify::FORCE_COVERAGE_STOP.with(|f| f.set(true));
    // the hook sets this cancellation, which the stopped pass acknowledged, so the
    // metadata and the answer finish without it
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopping = QueryOptions {
        cancel: Some(stop.clone()),
        ..opts()
    };
    let first = ds
        .ontology()
        .classify(&req(), &ClassifyRequest::default(), &stopping)
        .unwrap();
    assert!(stop.load(std::sync::atomic::Ordering::Relaxed));
    assert!(first.classification.stats.coverage_stopped);
    assert!(!first.classification.complete && first.classification.complete_for_taken);
    assert!(!first.retained && !first.reused);
    let second = ds
        .ontology()
        .classify(&req(), &ClassifyRequest::default(), &opts())
        .unwrap();
    assert!(!second.reused, "a stopped pass is never published");
    assert!(
        second.classification.stats.incremental,
        "the saturation was kept"
    );
    assert!(second.classification.complete && second.retained);
    let third = ds
        .ontology()
        .classify(&req(), &ClassifyRequest::default(), &opts())
        .unwrap();
    assert!(third.reused);
}

#[test]
fn kept_entity_status_reports_per_class_incomplete_on_retained_finished_result() {
    // An unread assertion leaves the terminology-only run finished (`partial: false`)
    // and retained (`complete_for_taken: true`), while marking classes incomplete.
    let dataset = ds(":A a owl:Class ; rdfs:subClassOf :B . :B a owl:Class . :a a :A .");
    let c = dataset
        .ontology()
        .classify(&req(), &ClassifyRequest::default(), &opts())
        .unwrap();
    assert!(c.retained);
    assert!(c.classification.complete_for_taken);
    assert!(!c.complete());

    let read_a = read(
        &dataset,
        &req(),
        &KeptQuery {
            about: Some("http://ex.org/A".into()),
            ..Default::default()
        },
    );
    assert_eq!(read_a.status, Status::Available);
    let outcome = read_a.outcome.as_ref().unwrap();
    assert!(!outcome.partial);
    assert!(!outcome.complete);
    assert_eq!(outcome.excluded_count, 1);
    let entity_a = read_a.entity.as_ref().unwrap();
    assert!(entity_a.present);
    assert_eq!(entity_a.incomplete, Some(true));

    let read_missing = read(
        &dataset,
        &req(),
        &KeptQuery {
            about: Some("http://ex.org/Missing".into()),
            ..Default::default()
        },
    );
    let entity_missing = read_missing.entity.as_ref().unwrap();
    assert!(!entity_missing.present);
    assert_eq!(entity_missing.incomplete, None);

    // A complete classification retains `incomplete: Some(false)` for present classes.
    let clean = ds(":A a owl:Class ; rdfs:subClassOf :B . :B a owl:Class .");
    let done = clean
        .ontology()
        .classify(&req(), &ClassifyRequest::default(), &opts())
        .unwrap();
    assert!(done.retained && done.complete());
    let read_clean = read(
        &clean,
        &req(),
        &KeptQuery {
            about: Some("http://ex.org/A".into()),
            ..Default::default()
        },
    );
    let entity_clean = read_clean.entity.as_ref().unwrap();
    assert!(entity_clean.present);
    assert_eq!(entity_clean.incomplete, Some(false));
}

#[test]
fn el_only_and_full_classifications_are_keyed_separately_and_never_clobber() {
    // Horn-not-EL ontology: A ⊑ ∃r.B, A ⊑ ∀r.C, BC ≡ B ⊓ C, ∃r.BC ⊑ D.
    // Full classification derives A ⊑ D (Horn tier, complete); EL-only (ELK) leaves out
    // ObjectAllValuesFrom and does not derive A ⊑ D.
    let ds = ds("
        :A a owl:Class . :B a owl:Class . :C a owl:Class . :D a owl:Class . :BC a owl:Class .
        :r a owl:ObjectProperty .
        :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; owl:someValuesFrom :B ] ,
                           [ a owl:Restriction ; owl:onProperty :r ; owl:allValuesFrom :C ] .
        :BC owl:equivalentClass [ a owl:Class ; owl:intersectionOf ( :B :C ) ] .
        [ a owl:Restriction ; owl:onProperty :r ; owl:someValuesFrom :BC ] rdfs:subClassOf :D .
    ");
    let a = sparkles_owl::Class::new("http://ex.org/A");
    let d = sparkles_owl::Class::new("http://ex.org/D");

    let full_req = ClassifyRequest::default();
    let el_req = ClassifyRequest {
        el_only: true,
        ..Default::default()
    };

    let full1 = ds.ontology().classify(&req(), &full_req, &opts()).unwrap();
    assert!(!full1.reused);
    assert!(full1.retained);
    assert!(full1.complete());
    assert!(!full1.publication.source.el_only);
    assert!(full1.classification.taxonomy.is_subclass(&a, &d));

    // EL-only classification must not reuse or clobber the full classification.
    let el1 = ds.ontology().classify(&req(), &el_req, &opts()).unwrap();
    assert!(!el1.reused);
    assert!(el1.retained);
    assert!(!el1.complete());
    assert!(el1.classification.complete_for_taken);
    assert!(el1.publication.source.el_only);
    assert_eq!(el1.excluded.len(), 1);
    assert!(!el1.classification.taxonomy.is_subclass(&a, &d));
    assert_ne!(full1.publication.id, el1.publication.id);

    // Both slots stay live and reuse their own exact publication.
    let full2 = ds.ontology().classify(&req(), &full_req, &opts()).unwrap();
    assert!(full2.reused);
    assert_eq!(full2.publication.id, full1.publication.id);
    assert!(full2.classification.taxonomy.is_subclass(&a, &d));

    let el2 = ds.ontology().classify(&req(), &el_req, &opts()).unwrap();
    assert!(el2.reused);
    assert_eq!(el2.publication.id, el1.publication.id);
    assert!(!el2.classification.taxonomy.is_subclass(&a, &d));

    // Kept reads distinguish el_only=false vs el_only=true and refuse cross-slot resultId pins.
    let kept_full = read(
        &ds,
        &req(),
        &KeptQuery {
            part: Part::Parents,
            about: Some("http://ex.org/A".into()),
            el_only: false,
            ..Default::default()
        },
    );
    assert_eq!(kept_full.status, Status::Available);
    assert_eq!(
        kept_full.result.as_ref().unwrap().result_id,
        full1.publication.id
    );
    assert!(!kept_full.result.as_ref().unwrap().el_only);
    assert!(
        kept_full
            .page
            .unwrap()
            .items
            .iter()
            .any(|row| row["iri"] == "http://ex.org/D")
    );

    let kept_el = read(
        &ds,
        &req(),
        &KeptQuery {
            part: Part::Parents,
            about: Some("http://ex.org/A".into()),
            el_only: true,
            ..Default::default()
        },
    );
    assert_eq!(kept_el.status, Status::Available);
    assert_eq!(
        kept_el.result.as_ref().unwrap().result_id,
        el1.publication.id
    );
    assert!(kept_el.result.as_ref().unwrap().el_only);
    assert!(
        !kept_el
            .page
            .unwrap()
            .items
            .iter()
            .any(|row| row["iri"] == "http://ex.org/D")
    );

    // Cross-slot pin is refused as Mismatch (resultChanged).
    let wrong_pin = read(
        &ds,
        &req(),
        &KeptQuery {
            result_id: Some(full1.publication.id),
            el_only: true,
            ..Default::default()
        },
    );
    assert_eq!(wrong_pin.status, Status::Mismatch);
    assert_eq!(wrong_pin.reason, Some("resultChanged"));

    // An incremental edit updates each slot independently without cross-contamination.
    ds.update(&format!(
        "{BASE} INSERT DATA {{ :E a owl:Class . :F a owl:Class . :E rdfs:subClassOf :F . }}"
    ))
    .unwrap();
    let e = sparkles_owl::Class::new("http://ex.org/E");
    let f = sparkles_owl::Class::new("http://ex.org/F");

    let el3 = ds.ontology().classify(&req(), &el_req, &opts()).unwrap();
    assert!(!el3.reused);
    assert!(el3.classification.stats.incremental);
    assert!(el3.classification.taxonomy.is_subclass(&e, &f));
    assert!(!el3.classification.taxonomy.is_subclass(&a, &d));

    let full3 = ds.ontology().classify(&req(), &full_req, &opts()).unwrap();
    assert!(!full3.reused);
    assert!(full3.classification.taxonomy.is_subclass(&e, &f));
    assert!(full3.classification.taxonomy.is_subclass(&a, &d));

    // Strict mode succeeds on full (complete) and refuses EL-only (excluded ObjectAllValuesFrom).
    assert!(
        ds.ontology()
            .classify(
                &req(),
                &ClassifyRequest {
                    strict: true,
                    ..full_req.clone()
                },
                &opts()
            )
            .is_ok()
    );
    assert!(
        ds.ontology()
            .classify(
                &req(),
                &ClassifyRequest {
                    strict: true,
                    ..el_req.clone()
                },
                &opts()
            )
            .is_err()
    );

    // Combined retention cap: when both slots together exceed the cap, storing one evicts the
    // other slot's saturation owner and publication while keeping the surviving slot within cap.
    struct CapRestore(usize);
    impl Drop for CapRestore {
        fn drop(&mut self) {
            TEST_RETENTION_CAP.with(|c| c.set(self.0));
        }
    }
    let full_bytes = ds
        .state()
        .classify_cache
        .lock()
        .slot(false)
        .entry
        .as_ref()
        .unwrap()
        .retained_bytes;
    let el_bytes = ds
        .state()
        .classify_cache
        .lock()
        .slot(true)
        .entry
        .as_ref()
        .unwrap()
        .retained_bytes;
    let cap = full_bytes.max(el_bytes) + full_bytes.min(el_bytes) / 2;
    {
        let _restore = CapRestore(TEST_RETENTION_CAP.with(|c| c.replace(cap)));
        ds.update(&format!("{BASE} INSERT DATA {{ :G a owl:Class . }}"))
            .unwrap();
        let el4 = ds.ontology().classify(&req(), &el_req, &opts()).unwrap();
        assert!(el4.retained);
        assert!(result_for(&ds, true).is_some());
        assert!(
            result_for(&ds, false).is_none(),
            "combined cap must evict the other slot's publication"
        );
        let cache = ds.state().classify_cache.lock();
        assert!(
            cache.slot(false).entry.is_none(),
            "combined cap must release the other slot's saturation owner"
        );
        assert!(cache.slot(false).publication.is_none());
        let surviving = cache
            .slot(true)
            .entry
            .as_ref()
            .expect("surviving slot must retain its entry");
        assert!(
            surviving.retained_bytes <= cap,
            "surviving slot must stay within cap: {} <= {cap}",
            surviving.retained_bytes
        );
    }

    // Active flight isolation: an in-flight EL-only operation must not mark the default slot running.
    let _full_warm = ds.ontology().classify(&req(), &full_req, &opts()).unwrap();
    ds.update(&format!("{BASE} INSERT DATA {{ :H a owl:Class . }}"))
        .unwrap();
    let h = ds.ontology();
    let opened = h.open(&req(), &opts()).unwrap();
    h.with_kept(&opened, &req(), true, &opts(), "classify", |_e| {
        let el_status = read(
            &ds,
            &req(),
            &KeptQuery {
                el_only: true,
                ..Default::default()
            },
        );
        assert_eq!(el_status.status, Status::Running);
        let full_status = read(&ds, &req(), &KeptQuery::default());
        assert_eq!(full_status.status, Status::Mismatch);
        assert!(full_status.activity.is_none());
        Ok(((), true))
    })
    .unwrap();

    // DL refill under combined cap: start from an empty EL-only slot (retained_bytes == 0) while
    // the default slot is warm, then run an EL-only DL query under a cap that only overflows once
    // retain_dl updates the EL-only slot's retained_bytes.
    let _full_dl = ds.ontology().classify(&req(), &full_req, &opts()).unwrap();
    let full_bytes2 = ds
        .state()
        .classify_cache
        .lock()
        .slot(false)
        .entry
        .as_ref()
        .unwrap()
        .retained_bytes;
    let mut dl_q = super::super::dl_query::DlQueryRequest::new(
        sparkles_owl::ClassExpression::Class(sparkles_owl::Class::new("http://ex.org/A")),
    );
    dl_q.el_only = true;
    dl_q.answers.instances = false;
    {
        let mut cache = ds.state().classify_cache.lock();
        let el_slot = cache.slot_mut(true);
        el_slot.entry = None;
        el_slot.publication = None;
    }
    let _probe = ds.ontology().dl_query(&req(), &dl_q, &opts()).unwrap();
    let el_dl_bytes = ds
        .state()
        .classify_cache
        .lock()
        .slot(true)
        .entry
        .as_ref()
        .expect("EL-only DL query must retain its slot")
        .retained_bytes;
    assert!(
        el_dl_bytes > 0,
        "retain_dl must record a nonzero byte charge"
    );
    {
        let mut cache = ds.state().classify_cache.lock();
        let el_slot = cache.slot_mut(true);
        el_slot.entry = None;
        el_slot.publication = None;
    }
    let dl_cap = full_bytes2.max(el_dl_bytes) + full_bytes2.min(el_dl_bytes) / 2;
    assert!(
        full_bytes2 <= dl_cap && full_bytes2 + el_dl_bytes > dl_cap,
        "cap must admit the warm default slot alone ({full_bytes2} <= {dl_cap}) and overflow once retain_dl charges {el_dl_bytes}"
    );
    {
        let _restore = CapRestore(TEST_RETENTION_CAP.with(|c| c.replace(dl_cap)));
        let answer = ds.ontology().dl_query(&req(), &dl_q, &opts()).unwrap();
        assert!(answer.incremental);
        assert!(answer.reasoning.el_only);
        let cache = ds.state().classify_cache.lock();
        assert!(
            cache.slot(false).entry.is_none(),
            "DL refill under combined cap must release the other slot's saturation owner"
        );
        assert!(
            cache.slot(false).publication.is_none(),
            "DL refill under combined cap must evict the other slot's publication"
        );
        let surviving = cache
            .slot(true)
            .entry
            .as_ref()
            .expect("EL-only slot must stay retained after DL refill");
        assert_eq!(
            surviving.retained_bytes, el_dl_bytes,
            "retain_dl must record the exact updated byte charge"
        );
        assert!(
            surviving.retained_bytes <= dl_cap,
            "surviving EL-only slot must stay within cap after retain_dl: {} <= {dl_cap}",
            surviving.retained_bytes
        );
    }
}

// ---------------------------------------------------- item 588: kept reads ------

const TAXONOMY: &str = ":A a owl:Class . :B a owl:Class . :C a owl:Class . :D a owl:Class . \
     :E a owl:Class . :B rdfs:subClassOf :A . :C rdfs:subClassOf :A . :D rdfs:subClassOf :B . \
     :E owl:equivalentClass :D . :N a owl:Class ; rdfs:subClassOf :A , \
     [ a owl:Class ; owl:complementOf :A ] .";
const EX: &str = "http://ex.org/";
fn iri(local: &str) -> String {
    format!("{EX}{local}")
}
fn iris(page: &Option<Page>) -> Vec<String> {
    page.as_ref()
        .expect("page")
        .items
        .iter()
        .map(|i| i["iri"].as_str().unwrap().to_string())
        .collect()
}
fn kept_axioms_all(ds: &Dataset, q: &KeptAxiomQuery) -> Vec<sparkles_owl::Axiom> {
    ds.ontology()
        .kept_axioms(&req(), q, &opts())
        .unwrap()
        .axioms
        .expect("available")
}

#[test]
fn kept_entity_and_axiom_misses_do_not_map_initialize_or_admit() {
    let ds = ds(TAXONOMY);
    REFUSE_ADMISSION.with(|f| f.set(true));
    let e = ds
        .ontology()
        .kept_entity(&req(), &KeptEntityQuery::new(iri("B")), &opts())
        .unwrap();
    assert_eq!(e.read.status, Status::Unavailable);
    assert_eq!(e.read.reason, Some("notKept"));
    assert!(e.read.entity.is_none() && e.parents.is_none() && e.children.is_none());
    let a = ds
        .ontology()
        .kept_axioms(&req(), &KeptAxiomQuery::default(), &opts())
        .unwrap();
    assert_eq!(a.read.status, Status::Unavailable);
    assert!(a.axioms.is_none() && a.total.is_none());
    let w = super::super::wire::call(&ds, "axioms", &json!({"classification":"kept"}), &opts())
        .unwrap();
    assert!(w["axioms"].is_null() && w["total"].is_null(), "{w}");
    assert_eq!(w["source"], "keptClassification");
    assert!(REFUSE_ADMISSION.with(|f| f.replace(false)));
    empty_slot(&ds);
    assert!(ds.state().ontology_cache.lock().is_empty());
}

#[test]
fn kept_entity_reads_direct_relations_equivalents_and_unsatisfiability() {
    let ds = ds(TAXONOMY);
    let p = classify(&ds);
    let e = ds
        .ontology()
        .kept_entity(&req(), &KeptEntityQuery::new(iri("B")), &opts())
        .unwrap();
    assert_eq!(e.read.status, Status::Available);
    assert_eq!(e.read.result.as_ref().unwrap().result_id, p.id);
    assert!(e.read.outcome.is_some());
    assert_eq!(iris(&e.parents), [iri("A")]);
    // D and E are one group, which D represents
    assert_eq!(iris(&e.children), [iri("D")]);
    assert_eq!(e.children.as_ref().unwrap().items[0]["equivalenceCount"], 2);
    assert!(iris(&e.equivalents).is_empty());
    let d = ds
        .ontology()
        .kept_entity(&req(), &KeptEntityQuery::new(iri("E")), &opts())
        .unwrap();
    assert_eq!(iris(&d.equivalents), [iri("D")]);
    assert_eq!(iris(&d.parents), [iri("B")]);
    let entity = d.read.entity.unwrap();
    assert_eq!(entity.representative.as_deref(), Some(iri("D").as_str()));
    assert_eq!(entity.unsatisfiable, Some(false));
    let n = ds
        .ontology()
        .kept_entity(&req(), &KeptEntityQuery::new(iri("N")), &opts())
        .unwrap();
    assert_eq!(n.read.entity.as_ref().unwrap().unsatisfiable, Some(true));
    assert!(iris(&n.equivalents).contains(&sparkles_owl::vocab::OWL_NOTHING.to_string()));
    let missing = ds
        .ontology()
        .kept_entity(&req(), &KeptEntityQuery::new(iri("Missing")), &opts())
        .unwrap();
    assert_eq!(missing.read.status, Status::Available);
    assert!(!missing.read.entity.as_ref().unwrap().present);
    assert!(
        missing.parents.is_none(),
        "an absent class is not an empty inference"
    );
    // the page bound caps each relation and reports the continuation
    let mut q = KeptEntityQuery::new(iri("A"));
    q.limit = 1;
    let a = ds.ontology().kept_entity(&req(), &q, &opts()).unwrap();
    let children = a.children.unwrap();
    // B and C, since the unsatisfiable N sits in the bottom group
    assert_eq!((children.items.len(), children.total), (1, 2));
    assert_eq!(children.next_offset, Some(1));
    q.limit = 1001;
    assert!(ds.ontology().kept_entity(&req(), &q, &opts()).is_err());
}

#[test]
fn kept_axioms_page_the_taxonomy_axioms_in_order_with_exact_filters() {
    let ds = ds(TAXONOMY);
    let p = classify(&ds);
    let expected = p.classification.taxonomy.axioms();
    let all = kept_axioms_all(
        &ds,
        &KeptAxiomQuery {
            limit: 1000,
            ..Default::default()
        },
    );
    assert_eq!(all, expected);
    // pages, continued with the pin, concatenate to the whole listing
    let commit = p.source.commit;
    let pin = OntologyRequest {
        at: Some(At::Commit(commit)),
        ..req()
    };
    let mut paged = Vec::new();
    let mut offset = 0;
    loop {
        let page = ds.ontology().kept_axioms(
            &pin,
            &KeptAxiomQuery {
                offset,
                limit: 2,
                result_id: (offset != 0).then_some(p.id),
                ..Default::default()
            },
            &opts(),
        );
        let page = match page {
            Ok(page) => page,
            // a memory dataset keeps only its head, which is the classified commit here
            Err(e) => panic!("{e}"),
        };
        assert_eq!(page.total, Some(expected.len()));
        paged.extend(page.axioms.unwrap());
        match page.next_offset {
            Some(n) => offset = n,
            None => break,
        }
    }
    assert_eq!(paged, expected);
    // `about` keeps exactly the axioms that mention it, in the same order
    for local in ["A", "B", "D", "E", "N", "Missing"] {
        let about = iri(local);
        let got = kept_axioms_all(
            &ds,
            &KeptAxiomQuery {
                about: Some(about.clone()),
                limit: 1000,
                ..Default::default()
            },
        );
        let want: Vec<_> = expected
            .iter()
            .filter(|a| a.mentions(&about))
            .cloned()
            .collect();
        assert_eq!(got, want, "{local}");
    }
    // an absent class is reported as absent, not as an empty listing
    let missing = ds
        .ontology()
        .kept_axioms(
            &req(),
            &KeptAxiomQuery {
                about: Some(iri("Missing")),
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!(missing.total, Some(0));
    assert!(!missing.read.entity.as_ref().unwrap().present);
    let w = super::super::wire::call(
        &ds,
        "axioms",
        &json!({"classification": "kept", "about": iri("B")}),
        &opts(),
    )
    .unwrap();
    assert_eq!(w["entity"]["present"], true, "{w}");
    // E is not its group's representative, so only the group's equivalence names it
    let e = kept_axioms_all(
        &ds,
        &KeptAxiomQuery {
            about: Some(iri("E")),
            ..Default::default()
        },
    );
    assert_eq!(e.len(), 1);
    assert_eq!(
        e[0].axiom_type(),
        sparkles_owl::AxiomType::EquivalentClasses
    );
    let types = Some([sparkles_owl::AxiomType::SubClassOf].into_iter().collect());
    let sub = kept_axioms_all(
        &ds,
        &KeptAxiomQuery {
            types,
            limit: 1000,
            ..Default::default()
        },
    );
    assert!(!sub.is_empty());
    assert!(
        sub.iter()
            .all(|a| a.axiom_type() == sparkles_owl::AxiomType::SubClassOf)
    );
    // a continuation needs the exact pin, and inferences are refused
    for (r, q) in [
        (
            req(),
            KeptAxiomQuery {
                offset: 1,
                result_id: Some(p.id),
                ..Default::default()
            },
        ),
        (
            pin.clone(),
            KeptAxiomQuery {
                offset: 1,
                ..Default::default()
            },
        ),
        (
            OntologyRequest {
                inferences: true,
                ..req()
            },
            KeptAxiomQuery::default(),
        ),
        (
            req(),
            KeptAxiomQuery {
                limit: 1001,
                ..Default::default()
            },
        ),
    ] {
        assert!(ds.ontology().kept_axioms(&r, &q, &opts()).is_err());
    }
}

#[test]
fn kept_axioms_charge_the_memory_budget_before_building_rows() {
    let ds = ds(TAXONOMY);
    classify(&ds);
    let tight = QueryOptions {
        max_memory_bytes: Some(64),
        ..opts()
    };
    match ds
        .ontology()
        .kept_axioms(&req(), &KeptAxiomQuery::default(), &tight)
    {
        Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, crate::BudgetKind::Memory),
        other => panic!("expected a memory refusal, got {other:?}"),
    }
    // a refused page leaves the kept result as it was
    assert!(result(&ds).is_some());
    let roomy = QueryOptions {
        max_memory_bytes: Some(1 << 20),
        ..opts()
    };
    assert!(
        ds.ontology()
            .kept_axioms(&req(), &KeptAxiomQuery::default(), &roomy)
            .unwrap()
            .axioms
            .is_some()
    );
}

#[test]
fn kept_reads_keep_their_scope_and_report_mismatch_without_switching() {
    let ds = ds(TAXONOMY);
    let p = classify(&ds);
    // a restricted view never observes the shared slot
    let restricted = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::Only(GraphRule::new(["default"], &[])),
            Graphs::All,
        ))),
        ..opts()
    };
    let a = ds
        .ontology()
        .kept_axioms(&req(), &KeptAxiomQuery::default(), &restricted)
        .unwrap();
    assert_eq!(a.read.reason, Some("unsupportedScope"));
    assert!(a.axioms.is_none());
    // a later commit reports the stale result and never reads it as current
    ds.update(&format!("{BASE} INSERT DATA {{ :F a owl:Class }}"))
        .unwrap();
    let e = ds
        .ontology()
        .kept_entity(&req(), &KeptEntityQuery::new(iri("B")), &opts())
        .unwrap();
    assert_eq!(e.read.status, Status::Mismatch);
    assert_eq!(e.read.stale_commit, Some(p.source.commit));
    assert!(e.parents.is_none());
    let mut q = KeptEntityQuery::new(iri("B"));
    q.result_id = Some(Uuid::new_v4());
    classify(&ds);
    let e = ds.ontology().kept_entity(&req(), &q, &opts()).unwrap();
    assert_eq!(e.read.reason, Some("resultChanged"));
}

#[test]
fn a_pinned_snapshot_reads_its_own_commit_after_memory_history_is_gone() {
    let ds = ds(TAXONOMY);
    let p = classify(&ds);
    let snap = ds.store().snapshot();
    assert_eq!(snap.commit, p.source.commit);
    ds.update(&format!(
        "{BASE} INSERT DATA {{ :F a owl:Class ; rdfs:subClassOf :B }}"
    ))
    .unwrap();
    // the live handle cannot name the pruned commit, and its head is newer
    let old = OntologyRequest {
        at: Some(At::Commit(p.source.commit)),
        ..req()
    };
    assert!(matches!(
        ds.ontology()
            .kept_classification(&old, &KeptQuery::default(), &opts()),
        Err(Error::HistoryGone(_))
    ));
    let pinned = ds.ontology().at_snapshot(snap.clone());
    let r = pinned
        .kept_classification(&req(), &KeptQuery::default(), &opts())
        .unwrap();
    assert_eq!(r.status, Status::Available);
    assert_eq!(r.commit, p.source.commit);
    // the pin's own commit is accepted, and a continuation needs only the result
    let q = KeptQuery {
        part: Part::Children,
        about: Some(iri("A")),
        offset: 1,
        limit: 1,
        result_id: Some(p.id),
        ..Default::default()
    };
    let page = pinned.kept_classification(&old, &q, &opts()).unwrap();
    assert_eq!(page.page.unwrap().total, 2);
    assert!(pinned.kept_classification(&req(), &q, &opts()).is_ok());
    let e = pinned
        .kept_entity(&req(), &KeptEntityQuery::new(iri("B")), &opts())
        .unwrap();
    assert_eq!(
        iris(&e.children),
        [iri("D")],
        "F is a child only at the head"
    );
    let a = pinned
        .kept_axioms(
            &req(),
            &KeptAxiomQuery {
                limit: 1000,
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!(a.axioms.unwrap(), p.classification.taxonomy.axioms());
    // another state, an edit and a writing classification are refused
    let other = OntologyRequest {
        at: Some(At::Commit(p.source.commit + 1)),
        ..req()
    };
    assert!(
        pinned
            .kept_classification(&other, &KeptQuery::default(), &opts())
            .is_err()
    );
    assert!(
        pinned
            .edit(&req(), &super::super::EditRequest::default(), &opts())
            .is_err()
    );
    assert!(pinned.remove_entity(&req(), &iri("A"), &opts()).is_err());
    let write = ClassifyRequest {
        target: Some(sparkles_reasoner::inputs::GraphRef::Named(
            "http://ex.org/out".into(),
        )),
        ..Default::default()
    };
    assert!(pinned.classify(&req(), &write, &opts()).is_err());
    let head = ds.store().snapshot().commit;
    assert_ne!(head, snap.commit);
    // the pinned wire answers its reads and refuses the others
    let w = super::super::wire::call_at(
        &ds,
        snap.clone(),
        "entity",
        &json!({"iri": iri("B"), "classification": "kept"}),
        &opts(),
    )
    .unwrap();
    assert_eq!(w["classification"]["status"], "available", "{w}");
    assert_eq!(w["classification"]["commit"], snap.commit);
    assert!(super::super::wire::call_at(&ds, snap, "summary", &json!({}), &opts()).is_err());
}

#[test]
fn wire_kept_selectors_refuse_ambiguous_combinations() {
    let ds = ds(TAXONOMY);
    for (op, args) in [
        ("axioms", json!({"elOnly": true})),
        ("axioms", json!({"resultId": Uuid::new_v4().to_string()})),
        ("axioms", json!({"classification": "materialized"})),
        (
            "axioms",
            json!({"classification": "kept", "inferences": true}),
        ),
        (
            "axioms",
            json!({"classification": "kept", "assertions": true}),
        ),
        ("axioms", json!({"classification": "kept", "limit": 1001})),
        ("axioms", json!({"classification": "kept", "offset": 1})),
        ("entity", json!({"iri": iri("A"), "elOnly": true})),
        (
            "entity",
            json!({"iri": iri("A"), "classification": "kept", "inferences": true}),
        ),
        (
            "entity",
            json!({"iri": iri("A"), "classification": "kept", "limit": 1001}),
        ),
    ] {
        assert!(
            super::super::wire::call(&ds, op, &args, &opts()).is_err(),
            "{op} {args}"
        );
    }
    classify(&ds);
    let e = super::super::wire::call(
        &ds,
        "entity",
        &json!({"iri": iri("B"), "classification": "kept", "limit": 1}),
        &opts(),
    )
    .unwrap();
    let c = &e["classification"];
    assert_eq!(c["status"], "available");
    assert_eq!(c["source"], "keptClassification");
    assert_eq!(c["covers"], json!(["namedClass"]));
    assert_eq!(c["parents"]["items"][0]["iri"], iri("A"));
    assert!(c.get("page").is_none());
    // the asserted accessors are unchanged beside it
    assert!(e["class"]["superClasses"].is_array());
}
