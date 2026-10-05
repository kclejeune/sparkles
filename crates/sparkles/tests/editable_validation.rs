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
