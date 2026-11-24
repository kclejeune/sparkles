//! Citation document identity under C17 §5.5 and C18 §7.6.
use super::*;

const PREFIXES: &str = r#"@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix spk: <urn:x-sparkles:> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
"#;

fn fixture(body: &str) -> McpServer {
    let server = server_with(&[], McpConfig::default());
    let ds = server.state.attach("mem", DbType::Mem, None).unwrap();
    load_trig(&ds, &format!("{PREFIXES}{body}"));
    server
}

fn read(server: &McpServer, principal: Principal, extra: Value) -> Value {
    let mut call = Call {
        arrived: Instant::now(),
        cancel: Arc::new(AtomicBool::new(false)),
        request_id: "citation-source".into(),
        principal,
        headers: None,
        held: None,
        grant: None,

        preparation: None,
    };
    let mut args = json!({"dataset":"mem", "seeds":["urn:cite:entity"], "hops":0, "format":"json"});
    args.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    match server
        .run_now("recall", args.as_object().unwrap().clone(), &mut call)
        .unwrap()
    {
        Outcome::Text(text) => serde_json::from_str(&text).unwrap(),
        Outcome::Structured(_) => panic!("recall JSON is a text block"),
    }
}

fn citation<'a>(result: &'a Value, reifier: &str) -> &'a Value {
    result["citations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["reifier"] == format!("<{reifier}>"))
        .unwrap_or_else(|| panic!("missing reifier {reifier}: {result}"))
}

#[test]
fn recall_citation_document_source_survives_candidate_order() {
    for (n, candidates) in [
        "<urn:a:rendition#char=0,22>, <urn:z:document>, <urn:0:prior>",
        "<urn:z:document>, <urn:0:prior>, <urn:a:rendition#char=0,22>",
        "<urn:0:prior>, <urn:a:rendition#char=0,22>, <urn:z:document>",
    ]
    .iter()
    .enumerate()
    {
        let server = fixture(&format!(
            r#"
<urn:cite:graph> {{
  <urn:cite:entity> <urn:cite:predicate> "Ana moved to Platform." .
  <urn:cite:reifier> rdf:reifies <<( <urn:cite:entity> <urn:cite:predicate> "Ana moved to Platform." )>> ;
    prov:wasDerivedFrom {candidates} ; prov:generatedAtTime "2026-10-01T10:00:00Z"^^xsd:dateTime ;
    prov:wasGeneratedBy <urn:cite:activity> ; spk:confidence 0.9 ; spk:quote "Ana moved to Platform." .
  <urn:cite:activity> prov:wasAssociatedWith <urn:cite:principal> .
  <urn:z:document> spk:rendition <urn:a:rendition> .
  <urn:a:rendition> a spk:TextRendition ; spk:length 22 ; prov:wasDerivedFrom <urn:z:document> .
  <urn:0:prior> rdf:reifies <<( <urn:prior:entity> <urn:cite:predicate> "old" )>> .
}}
"#
        ));
        let result = read(&server, Principal::local(), json!({}));
        let c = citation(&result, "urn:cite:reifier");
        assert_eq!(
            c["source"], "<urn:z:document>",
            "R4_DOCUMENT_SOURCE_IDENTITY candidate order {n}: {result}"
        );
        assert_eq!(c["graph"], "<urn:cite:graph>");
        assert_eq!(c["quote"], "\"Ana moved to Platform.\"");
        assert_eq!(c["at"], "2026-10-01T10:00:00Z");
        assert_eq!(c["by"], "<urn:cite:principal>");
        assert_eq!(c["confidence"], "0.9");
    }
}

#[test]
fn recall_citation_source_keeps_opaque_legacy_identity() {
    for source in ["urn:legacy:document", "urn:legacy:document#char=1,3"] {
        let server = fixture(&format!(
            r#"
<urn:cite:graph> {{
  <urn:cite:entity> <urn:cite:predicate> "legacy" .
  <urn:cite:reifier> rdf:reifies <<( <urn:cite:entity> <urn:cite:predicate> "legacy" )>> ;
    prov:wasDerivedFrom <{source}> .
}}
"#
        ));
        let result = read(&server, Principal::local(), json!({}));
        assert_eq!(
            citation(&result, "urn:cite:reifier")["source"],
            format!("<{source}>")
        );
    }
}

const MULTI: &str = r#"
<urn:cite:graph> {
  <urn:cite:entity> <urn:cite:predicate> "multiple sources" .
  <urn:cite:reifier> rdf:reifies <<( <urn:cite:entity> <urn:cite:predicate> "multiple sources" )>> ;
    prov:wasDerivedFrom <urn:cite:rendition#char=0,10>, <urn:0:explicit> .
  <urn:cite:rendition> a spk:TextRendition ; spk:length 10 ;
    prov:wasDerivedFrom <urn:source:z>, <urn:source:b> .
}
<urn:cite:other-metadata> {
  <urn:cite:rendition> a spk:TextRendition ; spk:length 10 ; prov:wasDerivedFrom <urn:source:a> .
}
"#;

#[test]
fn recall_citation_source_uses_stable_visible_representative() {
    let server = fixture(MULTI);
    let result = read(&server, Principal::local(), json!({}));
    assert_eq!(
        citation(&result, "urn:cite:reifier")["source"],
        "<urn:source:b>",
        "R4_STABLE_REPRESENTATIVE {result}"
    );
    let restricted = Principal::local().hiding_graphs(&["urn:cite:other-metadata".into()]);
    let result = read(&server, restricted, json!({}));
    assert_eq!(
        citation(&result, "urn:cite:reifier")["source"],
        "<urn:source:b>"
    );
    assert!(!result.to_string().contains("urn:source:a"));
}

#[test]
fn recall_citation_source_metadata_stays_in_actual_view() {
    let server = fixture(
        r#"
<urn:cite:graph> {
  <urn:cite:entity> <urn:cite:predicate> "visible" .
  <urn:cite:reifier> rdf:reifies <<( <urn:cite:entity> <urn:cite:predicate> "visible" )>> ;
    prov:wasDerivedFrom <urn:cite:rendition#char=0,10> .
  <urn:cite:rendition> a spk:TextRendition ; spk:length 10 .
}
<urn:cite:hidden> { <urn:cite:rendition> prov:wasDerivedFrom <urn:secret:document> . }
"#,
    );
    let full = read(&server, Principal::local(), json!({}));
    assert_eq!(
        citation(&full, "urn:cite:reifier")["source"],
        "<urn:secret:document>",
        "R4_VISIBLE_METADATA_SOURCE {full}"
    );
    let restricted = read(
        &server,
        Principal::local().hiding_graphs(&["urn:cite:hidden".into()]),
        json!({}),
    );
    assert!(
        citation(&restricted, "urn:cite:reifier")
            .get("source")
            .is_none(),
        "{restricted}"
    );
    assert!(!restricted.to_string().contains("urn:secret:document"));
}

#[test]
fn recall_citation_source_metadata_uses_historical_and_branch_readers() {
    let server = fixture(
        r#"
<urn:cite:graph> {
  <urn:cite:entity> <urn:cite:predicate> "snapshot" .
  <urn:cite:reifier> rdf:reifies <<( <urn:cite:entity> <urn:cite:predicate> "snapshot" )>> ;
    prov:wasDerivedFrom <urn:cite:rendition#char=0,10> .
  <urn:cite:rendition> a spk:TextRendition ; spk:length 10 ; prov:wasDerivedFrom <urn:source:old> .
}
"#,
    );
    let ds = server.state.get("mem").unwrap();
    ds.store
        .create_snapshot_opts(
            "before",
            &sparkles::history::At::Commit(1),
            &Default::default(),
        )
        .unwrap();
    ds.store
        .create_branch("citation-branch", &Default::default())
        .unwrap();
    let replace = |store, source| {
        sparkles::sparql::update::update(store, &format!(
            "DELETE WHERE {{ GRAPH <urn:cite:graph> {{ <urn:cite:rendition> <http://www.w3.org/ns/prov#wasDerivedFrom> ?old }} }} ; INSERT DATA {{ GRAPH <urn:cite:graph> {{ <urn:cite:rendition> <http://www.w3.org/ns/prov#wasDerivedFrom> <{source}> }} }}"
        ), &Default::default()).unwrap();
    };
    replace(&ds.store, "urn:source:current");
    let branch = server.state.branch_dataset(&ds, "citation-branch").unwrap();
    replace(&branch.store, "urn:source:branch");
    for (extra, source) in [
        (json!({}), "urn:source:current"),
        (json!({"at":1}), "urn:source:old"),
        (json!({"branch":"citation-branch"}), "urn:source:branch"),
    ] {
        let result = read(&server, Principal::local(), extra);
        assert_eq!(
            citation(&result, "urn:cite:reifier")["source"],
            format!("<{source}>"),
            "R4_SNAPSHOT_SOURCE {result}"
        );
    }
}

#[test]
fn recall_citation_source_rejects_reifiers_without_parsing_unregistered_fragments() {
    for (candidate, metadata, expected) in [
        (
            "urn:prior:reifier",
            "<urn:prior:reifier> rdf:reifies <<( <urn:other> <urn:cite:predicate> 1 )>> .",
            None,
        ),
        (
            "urn:cite:rendition#char=10,20",
            "<urn:cite:rendition> a spk:TextRendition ; spk:length 10 ; prov:wasDerivedFrom <urn:wrong:document> .",
            Some("<urn:cite:rendition#char=10,20>"),
        ),
        (
            "urn:cite:rendition#char=0,10",
            "<urn:cite:rendition> spk:length 10 ; prov:wasDerivedFrom <urn:wrong:document> .",
            Some("<urn:cite:rendition#char=0,10>"),
        ),
    ] {
        let server = fixture(&format!(
            r#"
<urn:cite:graph> {{
  <urn:cite:entity> <urn:cite:predicate> "opaque" .
  <urn:cite:reifier> rdf:reifies <<( <urn:cite:entity> <urn:cite:predicate> "opaque" )>> ; prov:wasDerivedFrom <{candidate}> .
  {metadata}
}}
"#
        ));
        let result = read(&server, Principal::local(), json!({}));
        let source = citation(&result, "urn:cite:reifier")
            .get("source")
            .and_then(Value::as_str);
        assert_eq!(
            source, expected,
            "R4_DERIVATION_REIFIER_IS_NOT_SOURCE {result}"
        );
    }
}

#[test]
fn pooled_server_grantless_recall_with_review_policy_succeeds() {
    use sparkles::evidence::{ClaimKind, ConflictMode, Policy, Settings};
    let dir = tempfile::tempdir().unwrap();
    let cfg = McpConfig::default();
    let mut st = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        cfg.max_timeout,
        Some(64 << 20),
    )
    .unwrap();
    st.read_only = false;
    st.limits.query_memory_bytes = cfg.query_memory_bytes;
    let st = Arc::new(st);
    let ds = st.attach("mem", DbType::Mem, None).unwrap();
    let prep =
        sparkles::access::Preparation::new(sparkles::task::Control::none(), 64 << 20).unwrap();
    ds.dataset
        .store()
        .activate_memory_configuration_history(&prep)
        .unwrap();
    drop(prep);
    let settings = Settings {
        enabled: true,
        source_records: vec!["urn:ev:sources".into()],
        proposals: vec!["urn:ev:claims".into()],
        decisions: vec!["urn:ev:decisions".into()],
        supports: vec!["urn:ev:supports".into()],
        projections: vec!["urn:ev:accepted".into()],
        policies: [(
            "main".into(),
            Policy {
                version: 1,
                admitted_kinds: vec![ClaimKind::Reported],
                required_scope: vec![],
                conflicts: ConflictMode::AdmitLabelled,
                boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
                decisions: "urn:ev:decisions".into(),
                supports: "urn:ev:supports".into(),
                output: "urn:ev:accepted".into(),
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    sparkles::evidence::configure(&ds.dataset, &settings, &sparkles::task::Control::none())
        .unwrap();
    load_trig(
        &ds,
        &format!(
            "{PREFIXES}\n<urn:g:facts> {{ <urn:cite:entity> <urn:cite:predicate> \"hello\" . }}\n"
        ),
    );
    let server = McpServer::new(st.clone(), cfg);
    let result = read(&server, Principal::local(), json!({"reviewPolicy": "main"}));
    assert!(
        result.get("entities").is_some(),
        "expected recall result JSON, got: {result}"
    );
    assert_eq!(st.catalog.pool().unwrap().granted(), 0);
}

#[cfg(feature = "shacl")]
#[test]
fn recall_conflicts_use_historical_reader_state_and_evidence_view() {
    use sparkles::evidence::{ClaimKind, ConflictMode, Policy, Settings};
    let server = server_with(&[], McpConfig::default());
    let ds = server.state.attach("mem", DbType::Mem, None).unwrap();
    let prep =
        sparkles::access::Preparation::new(sparkles::task::Control::none(), 64 << 20).unwrap();
    ds.dataset
        .store()
        .activate_memory_configuration_history(&prep)
        .unwrap();
    drop(prep);
    let settings = Settings {
        enabled: true,
        source_records: vec!["urn:ev:sources".into()],
        proposals: vec!["urn:ev:claims".into()],
        decisions: vec!["urn:ev:decisions".into()],
        supports: vec!["urn:ev:supports".into()],
        projections: vec!["urn:ev:accepted".into()],
        policies: [(
            "main".into(),
            Policy {
                version: 1,
                admitted_kinds: vec![ClaimKind::Reported],
                required_scope: vec![],
                conflicts: ConflictMode::AdmitLabelled,
                boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
                decisions: "urn:ev:decisions".into(),
                supports: "urn:ev:supports".into(),
                output: "urn:ev:accepted".into(),
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    sparkles::evidence::configure(&ds.dataset, &settings, &sparkles::task::Control::none())
        .unwrap();
    load_trig(
        &ds,
        &format!(
            "{PREFIXES}\n<urn:g:a> {{ <urn:cite:entity> a <urn:Person> ; <urn:email> \"a@example.org\" . }}\n<urn:g:b> {{ <urn:cite:entity> <urn:email> \"b@example.org\" . }}\n"
        ),
    );
    let commit_with_conflict = ds.store.head_commit().seq;
    ds.store
        .create_snapshot_opts(
            "before-conflict-fix",
            &sparkles::history::At::Commit(commit_with_conflict),
            &Default::default(),
        )
        .unwrap();
    // Install a SHACL write guard limiting <urn:email> to maxCount 1 for <urn:Person>.
    let cfg: sparkles_shacl::guard::ValidationConfig = serde_json::from_value(json!({
        "format": 2,
        "language": "shacl",
        "mode": "warn",
        "dataGraph": "union",
        "shapes": {
            "inline": "@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:shape:person> a sh:NodeShape ; sh:targetClass <urn:Person> ; sh:property [ sh:path <urn:email> ; sh:maxCount 1 ] ."
        }
    }))
    .unwrap();
    ds.dataset.validation().guard().set_shacl(cfg).unwrap();
    // Resolve the conflict at HEAD by deleting the second graph's email.
    ds.dataset
        .update(
            "DELETE DATA { GRAPH <urn:g:b> { <urn:cite:entity> <urn:email> \"b@example.org\" } }",
        )
        .unwrap();

    // At HEAD, there is no conflict; at the historical commit, both snapshot recall
    // and evidence-backed recall report the conflict cleanly.
    let head = read(&server, Principal::local(), json!({}));
    assert!(
        head.get("conflicts")
            .is_none_or(|c| c.as_array().unwrap().is_empty()),
        "expected no conflict at HEAD: {head}"
    );
    let hist_snap = read(
        &server,
        Principal::local(),
        json!({"atCommit": commit_with_conflict}),
    );
    assert_eq!(
        hist_snap["conflicts"].as_array().unwrap().len(),
        1,
        "expected historical snapshot conflict: {hist_snap}"
    );
    let hist_ev = read(
        &server,
        Principal::local(),
        json!({"atCommit": commit_with_conflict, "reviewPolicy": "main"}),
    );
    assert_eq!(
        hist_ev["conflicts"].as_array().unwrap().len(),
        1,
        "expected historical evidence-backed conflict: {hist_ev}"
    );
}
