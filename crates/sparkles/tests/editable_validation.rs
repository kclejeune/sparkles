//! The public guard configuration must be reusable on memory and persistent stores.
#![cfg(all(feature = "shacl", feature = "shex"))]
use serde_json::{Value, json};
use sparkles::Dataset;

#[test]
fn memory_shacl_configuration_round_trips_with_inline_source() {
    let ds = Dataset::memory();
    let cfg = json!({"mode":"warn", "shapes":{"inline":"@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:S> a sh:NodeShape ."}});
    ds.validation()
        .guard()
        .set_shacl(serde_json::from_value(cfg).unwrap())
        .unwrap();
    let mut editable = ds.validation().guard().get().unwrap().json()["config"].clone();
    assert!(editable["shapes"]["inline"].is_string());
    editable["reportLimit"] = json!(12);
    ds.validation()
        .guard()
        .set_shacl(serde_json::from_value(editable).unwrap())
        .unwrap();
    assert_eq!(
        ds.validation().guard().get().unwrap().json()["config"]["reportLimit"],
        12
    );
}

#[test]
fn memory_shex_configuration_round_trips_without_a_copied_file() {
    let ds = Dataset::memory();
    let cfg = json!({"language":"shex", "mode":"warn", "schema":{"inline":"<urn:S> {}", "format":"shexc"}, "shapeMap":"<urn:n>@<urn:S>"});
    ds.validation()
        .guard()
        .set_shex(
            serde_json::from_value(cfg).unwrap(),
            &sparkles_shex::NoImports,
        )
        .unwrap();
    let mut editable = ds.validation().guard().get().unwrap().json()["config"].clone();
    assert!(editable["schema"]["inline"].is_string());
    assert_eq!(editable["schema"]["file"], Value::Null);
    editable["reportLimit"] = json!(12);
    ds.validation()
        .guard()
        .set_shex(
            serde_json::from_value(editable).unwrap(),
            &sparkles_shex::NoImports,
        )
        .unwrap();
    assert_eq!(
        ds.validation().guard().get().unwrap().json()["config"]["reportLimit"],
        12
    );
}

#[test]
fn memory_shex_json_source_retains_shape_map_prefixes() {
    let ds = Dataset::memory();
    let schema = json!({ "type":"Schema", "shapes":[{ "type":"ShapeDecl", "id":"urn:ex:S", "shapeExpr": { "type":"Shape" } }] }).to_string();
    let cfg = json!({"language":"shex", "mode":"warn", "schema":{"inline":schema, "format":"shexj", "prefixes":{"ex":"urn:ex:"}}, "shapeMap":"<urn:n>@ex:S"});
    ds.validation()
        .guard()
        .set_shex(
            serde_json::from_value(cfg).unwrap(),
            &sparkles_shex::NoImports,
        )
        .unwrap();
    let editable = ds.validation().guard().get().unwrap().json()["config"].clone();
    assert_eq!(editable["schema"]["prefixes"]["ex"], "urn:ex:");
    ds.validation()
        .guard()
        .set_shex(
            serde_json::from_value(editable).unwrap(),
            &sparkles_shex::NoImports,
        )
        .unwrap();
}

#[test]
fn memory_shex_backup_copy_keeps_its_digest_after_conversion() {
    let ds = Dataset::memory();
    let turtle = sparkles_shex::Schema::parse_shexc("<urn:S> {}", None)
        .unwrap()
        .to_shexr_turtle();
    let config = json!({"language":"shex", "mode":"reject", "schema":{"inline":turtle, "format":"shexr"}, "shapeMap":"<urn:n>@<urn:S>"});
    ds.validation()
        .guard()
        .set_shex(
            serde_json::from_value(config).unwrap(),
            &sparkles_shex::NoImports,
        )
        .unwrap();
    let files = ds.validation().guard().get().unwrap().memory_files();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("restored");
    std::fs::create_dir(&root).unwrap();
    for (file, bytes) in files {
        std::fs::write(root.join(file), bytes).unwrap();
    }
    let config: Value =
        serde_json::from_slice(&std::fs::read(root.join("validation.json")).unwrap()).unwrap();
    assert_eq!(config["format"], 3);
    assert_ne!(config["schema"]["sha256"], config["schema"]["copySha256"]);
    let restored = Dataset::open(&root).unwrap();
    assert!(restored.validation().guard().get().is_some());
    assert_eq!(
        restored.validation().guard().get().unwrap().json()["config"]["schema"]["copySha256"],
        config["schema"]["copySha256"]
    );
}

#[test]
fn memory_compact_shacl_backup_source_is_reusable_turtle() {
    let ds = Dataset::memory();
    let cfg = json!({"mode":"reject", "shapes":{"inline":"PREFIX ex: <urn:ex:> shape ex:S -> ex:Person { ex:name [1..1] . }", "format":"text/shaclc"}});
    ds.validation()
        .guard()
        .set_shacl(serde_json::from_value(cfg).unwrap())
        .unwrap();
    let files = ds.validation().guard().get().unwrap().memory_files();
    let mut cfg: Value = serde_json::from_slice(
        &files
            .iter()
            .find(|(path, _)| path == "validation.json")
            .unwrap()
            .1,
    )
    .unwrap();
    let text = String::from_utf8(
        files
            .iter()
            .find(|(path, _)| path == "validation-shapes.ttl")
            .unwrap()
            .1
            .clone(),
    )
    .unwrap();
    cfg["shapes"].as_object_mut().unwrap().remove("file");
    cfg["shapes"]["inline"] = json!(text);
    let restored = Dataset::memory();
    restored
        .validation()
        .guard()
        .set_shacl(serde_json::from_value(cfg).unwrap())
        .unwrap();
    assert!(
        restored
            .update("INSERT DATA { <urn:alice> a <urn:ex:Person> }")
            .is_err()
    );
}

fn combined(shacl_mode: &str, shex_mode: &str) -> Vec<sparkles::write_guard::GuardConfig> {
    sparkles::write_guard::parse_configs(json!({
        "format": 4,
        "guards": [
            {"language": "shex", "mode": shex_mode, "schema": {"inline": "<urn:Shex> { <urn:email> . }"}, "shapeMap": "{FOCUS a <urn:C>}@<urn:Shex>"},
            {"language": "shacl", "mode": shacl_mode, "shapes": {"inline":
                "@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:S> a sh:NodeShape ; sh:targetClass <urn:C> ; sh:property [ sh:path <urn:name> ; sh:minCount 1 ] ."}}
        ]
    })).unwrap()
}

const BOTH_FAIL: &str = "INSERT DATA { <urn:n> a <urn:C> }";
const ONLY_SHACL_FAILS: &str = "INSERT DATA { <urn:n> a <urn:C> ; <urn:email> 'n@example' }";
const ONLY_SHEX_FAILS: &str = "INSERT DATA { <urn:n> a <urn:C> ; <urn:name> 'N' }";

#[test]
fn combined_validation_reports_both_guards_and_installs_atomically() {
    use sparkles::guard::{GuardLanguage, GuardStatus};
    use sparkles::handles::GuardOutcome;
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    let settings = ds.validation().guard();
    ds.update(BOTH_FAIL).unwrap();
    let result = settings
        .set_guards(combined("reject", "reject"), &sparkles_shex::NoImports)
        .unwrap();
    let GuardOutcome::NotConforming(s) = result else {
        panic!("{result:?}")
    };
    assert_eq!(
        s.guards.iter().map(|g| g.language).collect::<Vec<_>>(),
        [GuardLanguage::Shex, GuardLanguage::Shacl]
    );
    assert!(
        s.guards
            .iter()
            .all(|g| g.status == GuardStatus::Rejected && g.blocking == 1)
    );
    assert!(settings.get().is_none());
    assert!(!dir.path().join("validation.json").exists());
    ds.update("DELETE DATA { <urn:n> a <urn:C> }").unwrap();
    settings
        .set_guards(combined("reject", "warn"), &sparkles_shex::NoImports)
        .unwrap();

    let head = ds.snapshot().commit;
    let sparkles::Error::Rejected(e) = ds.update(BOTH_FAIL).unwrap_err() else {
        panic!()
    };
    assert_eq!(e.summary.guards[0].status, GuardStatus::Warned);
    assert_eq!(e.summary.guards[1].status, GuardStatus::Rejected);
    assert_eq!(e.summary.guards[0].results.len(), 1);
    assert_eq!(e.summary.guards[1].results.len(), 1);
    assert_eq!(ds.snapshot().commit, head);
    assert!(ds.update(ONLY_SHEX_FAILS).is_ok());
    let file = dir.path().join("validation.json");
    let bytes = std::fs::read(&file).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["format"],
        4
    );
    drop(settings);
    drop(ds);

    let ds = Dataset::open(dir.path()).unwrap();
    let settings = ds.validation().guard();
    assert_eq!(settings.get().unwrap().language(), GuardLanguage::Combined);
    assert!(matches!(
        ds.update("DELETE DATA { <urn:n> <urn:name> 'N' }"),
        Err(sparkles::Error::Rejected(_))
    ));
    assert_eq!(std::fs::read(&file).unwrap(), bytes);

    // An exact retained section keeps its existing state and uses the legacy flat
    // configuration/report shape when it becomes the only guard.
    let v = settings.get().unwrap();
    ds.update("INSERT DATA { <urn:n> <urn:email> 'n@example' }")
        .unwrap();
    let before = v.shex().unwrap().status().baseline.unwrap();
    let config = v.json()["config"]["guards"][0].clone();
    let configs =
        sparkles::write_guard::parse_configs(json!({"format": 4, "guards": [config]})).unwrap();
    assert!(matches!(
        settings
            .set_guards(configs, &sparkles_shex::NoImports)
            .unwrap(),
        GuardOutcome::Retained
    ));
    let retained = settings.get().unwrap();
    assert_eq!(
        retained.shex().unwrap().status().baseline.unwrap().commit,
        before.commit
    );
    assert_eq!(retained.language(), GuardLanguage::Shex);
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&file).unwrap()).unwrap()["format"],
        3
    );
    drop(retained);
    drop(v);
    drop(settings);
    drop(ds);
    assert_eq!(
        Dataset::open(dir.path())
            .unwrap()
            .write_guard()
            .unwrap()
            .language(),
        GuardLanguage::Shex
    );
}

#[test]
fn retained_persistent_guard_keeps_config_and_validation_state() {
    use sparkles::guard::config::{CONFIG_FILE, STATUS_FILE, StatusFile, TYPING_FILE, sha256_hex};
    use sparkles::handles::GuardOutcome;

    for (kept, source) in [(0, "schema"), (1, "shapes")] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let ds = Dataset::open(root).unwrap();
        ds.update("INSERT DATA { <urn:n> a <urn:C> ; <urn:name> 'N' ; <urn:email> 'old' }")
            .unwrap();
        let mut configs = combined("reject", "reject");
        let sparkles::write_guard::GuardConfig::Shex(cfg) = &mut configs[0] else {
            unreachable!()
        };
        cfg.schema.inline = Some("<urn:Shex> { <urn:email> @<urn:E> } <urn:E> LITERAL".into());
        let settings = ds.validation().guard();
        assert!(matches!(
            settings
                .set_guards(configs, &sparkles_shex::NoImports)
                .unwrap(),
            GuardOutcome::Installed(_)
        ));
        let combined = settings.get().unwrap();
        let entry = combined.json()["config"]["guards"][kept].clone();
        let configs =
            sparkles::write_guard::parse_configs(json!({"format": 4, "guards": [entry]})).unwrap();
        assert!(matches!(
            settings
                .set_guards(configs, &sparkles_shex::NoImports)
                .unwrap(),
            GuardOutcome::Retained
        ));
        let bytes = std::fs::read(root.join(CONFIG_FILE)).unwrap();
        let config: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(config[source]["file"].is_string());
        assert!(config[source].get("inline").is_none());
        assert_eq!(settings.get().unwrap().json()["config"], config);
        assert_eq!(combined.json()["config"]["guards"][kept], entry);

        let hash = sha256_hex(&bytes);
        let initial = ds.snapshot().commit;
        assert!(StatusFile::read(root, initial, &hash).is_some());
        ds.update("INSERT DATA { <urn:m> a <urn:C> ; <urn:name> 'M' ; <urn:email> 'new' }")
            .unwrap();
        let head = ds.snapshot().commit;
        assert!(head > initial);
        assert!(StatusFile::read(root, head, &hash).is_some());
        drop(settings);
        drop(ds);
        assert!(root.join(STATUS_FILE).is_file());
        assert_eq!(root.join(TYPING_FILE).is_file(), source == "schema");
        let reopened = Dataset::open(root).unwrap();
        let state = reopened.validation().guard().get().unwrap().json();
        assert_eq!(state["config"], config);
        assert_eq!(state["status"]["baseline"]["commit"], head);
    }
}

#[test]
fn combined_guard_protects_merges_and_new_memory_branches() {
    let ds = Dataset::memory();
    ds.create_branch("source", &Default::default()).unwrap();
    let source = ds.branch("source").unwrap();
    source.update(ONLY_SHACL_FAILS).unwrap();
    ds.validation()
        .guard()
        .set_guards(combined("reject", "reject"), &sparkles_shex::NoImports)
        .unwrap();
    let head = ds.snapshot().commit;
    let Err(sparkles::Error::Rejected(e)) = ds.merge("source", "main", &Default::default()) else {
        panic!("merge bypassed the guard")
    };
    assert_eq!(e.summary.guards.len(), 2);
    assert_eq!(ds.snapshot().commit, head);
    ds.create_branch("guarded", &Default::default()).unwrap();
    let branch = ds.branch("guarded").unwrap();
    let Err(sparkles::Error::Rejected(e)) = branch.update(ONLY_SHEX_FAILS) else {
        panic!("branch bypassed the guard")
    };
    assert_eq!(e.summary.guards.len(), 2);
}

#[test]
fn unknown_combined_format_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    ds.validation()
        .guard()
        .set_guards(combined("warn", "warn"), &sparkles_shex::NoImports)
        .unwrap();
    drop(ds);
    let path = dir.path().join("validation.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["format"] = json!(5);
    std::fs::write(&path, config.to_string()).unwrap();
    let Err(sparkles::Error::Unsupported(message)) = Dataset::open(dir.path()) else {
        panic!("the dataset opened with an unknown validation format");
    };
    assert!(
        message.contains("validation.json has format 5"),
        "{message}"
    );
}
