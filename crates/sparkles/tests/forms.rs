//! Editing forms generated from SHACL shapes (spec U02): the form model, checks and saves
//! with conflicts, over the shapes of a write guard and of shapes graphs.
#![cfg(feature = "shacl")]

use oxrdf::{Literal, NamedNode, Term};
use serde_json::{Value, json};
use sparkles::Dataset;
use sparkles::forms::{
    FieldChange, FormChange, FormRequest, FormSaved, FormSettings, ShapesSource,
};
use std::collections::BTreeMap;

const SHAPES: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ex: <http://example.org/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:Work a sh:PropertyGroup ; rdfs:label "Work" ; sh:order 2 .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:name "Name" ; sh:minCount 1 ; sh:datatype xsd:string ; sh:order 1 ] ;
  sh:property [ sh:path ex:email ; sh:name "Email" ; sh:maxCount 1 ;
                sh:pattern "^[^@]+@[^@]+\\.[a-z]+$" ; sh:order 2 ] ;
  sh:property [ sh:path ex:worksFor ; sh:name "Works for" ; sh:class ex:Org ; sh:minCount 1 ;
                sh:group ex:Work ] ;
  sh:property [ sh:path ex:role ; sh:in ( "Engineer" "Manager" "Other" ) ; sh:group ex:Work ] ;
  sh:property [ sh:path [ sh:inversePath ex:member ] ; sh:name "Teams" ] ;
  sh:property [ sh:path ( ex:worksFor ex:name ) ; sh:name "Employer name" ] .
"#;

const DATA: &str = r#"
PREFIX ex: <http://example.org/>
INSERT DATA {
  ex:alice a ex:Person ; ex:name "Alice Lima" ; ex:email "alice@example.org" ;
    ex:worksFor ex:acme ; ex:role "Engineer" ; ex:nickname "Al" .
  ex:acme a ex:Org ; ex:name "Acme Corp" .
  ex:bob a ex:Person ; ex:name "Bob" ; ex:worksFor ex:acme .
  ex:core ex:member ex:alice .
}"#;

const EX: &str = "http://example.org/";

fn ex(s: &str) -> String {
    format!("{EX}{s}")
}

fn lit(s: &str) -> Term {
    Literal::new_simple_literal(s).into()
}

fn iri(s: &str) -> Term {
    NamedNode::new(ex(s)).unwrap().into()
}

/// A dataset whose shapes sit in a shapes graph.
fn dataset() -> (Dataset, FormSettings) {
    let ds = Dataset::memory();
    ds.update(DATA).unwrap();
    ds.load_str_into(SHAPES, sparkles::io::RdfFormat::Turtle, "urn:shapes")
        .unwrap();
    let settings = FormSettings {
        shapes: ShapesSource::Graphs,
        graphs: vec!["urn:shapes".into()],
        ..Default::default()
    };
    (ds, settings)
}

fn model(ds: &Dataset, settings: &FormSettings, focus: &str) -> Value {
    let req = FormRequest::new(ex(focus), settings.clone());
    serde_json::to_value(ds.forms().model(&req, &Default::default()).unwrap()).unwrap()
}

fn field<'a>(m: &'a Value, label: &str) -> &'a Value {
    m["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["label"] == label)
        .unwrap_or_else(|| panic!("no field {label}: {m}"))
}

#[test]
fn a1_the_form_lists_the_fields_in_order_with_groups_and_values() {
    let (ds, settings) = dataset();
    let m = model(&ds, &settings, "alice");
    assert_eq!(m["formFormat"], 1);
    assert_eq!(m["types"], json!([ex("Person")]));
    assert_eq!(m["shapes"]["nodeShapes"], json!([ex("PersonShape")]));
    let labels: Vec<&str> = m["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["label"].as_str().unwrap())
        .collect();
    // ungrouped first by sh:order then label, then the Work group
    assert_eq!(
        labels,
        [
            "Name",
            "Email",
            "Employer name",
            "Teams",
            "Works for",
            "ex:role"
        ]
    );
    assert_eq!(
        m["groups"],
        json!([{ "iri": ex("Work"), "label": "Work", "order": 2.0 }])
    );
    let name = field(&m, "Name");
    assert_eq!(name["id"], format!("<{}>", ex("name")));
    assert_eq!(name["constraints"]["minCount"], 1);
    assert_eq!(name["widget"], "text");
    assert_eq!(
        name["values"][0]["term"],
        json!({ "type": "literal", "value": "Alice Lima" })
    );
    assert_eq!(name["values"][0]["graph"], Value::Null);
    assert_eq!(field(&m, "ex:role")["widget"], "radio");
    let works = field(&m, "Works for");
    assert_eq!(works["widget"], "entity");
    assert_eq!(works["values"][0]["term"]["value"], ex("acme"));
    let teams = field(&m, "Teams");
    assert_eq!(teams["inverse"], true);
    assert_eq!(teams["path"], format!("^<{}>", ex("member")));
    assert_eq!(teams["values"][0]["term"]["value"], ex("core"));
    let complex = field(&m, "Employer name");
    assert_eq!(complex["editable"], false);
    assert_eq!(complex["reason"], "complex-path");
    // A7: other properties
    assert_eq!(
        m["other"],
        json!([{ "predicate": ex("nickname"), "values": [
            { "term": { "type": "literal", "value": "Al" }, "graph": null, "readOnly": true }
        ] }])
    );
    assert_eq!(m["editable"], true);
}

#[test]
fn a_focus_no_shape_targets_has_no_fields() {
    let (ds, settings) = dataset();
    let m = model(&ds, &settings, "acme");
    assert_eq!(m["fields"], json!([]));
    assert_eq!(m["shapes"]["nodeShapes"], json!([]));
    assert_eq!(m["editable"], false);
}

fn change(field: &str, remove: Vec<Term>, add: Vec<Term>, expect: Vec<Term>) -> FormChange {
    let id = format!("<{}>", ex(field));
    FormChange {
        focus: ex("alice"),
        changes: vec![FieldChange {
            field: id.clone(),
            remove,
            add,
        }],
        expect: BTreeMap::from([(id, expect)]),
        message: Some("fix email".into()),
        ..Default::default()
    }
}

#[test]
fn a3_the_server_check_maps_results_to_fields_and_previews_the_commit() {
    let (ds, settings) = dataset();
    let mut c = change(
        "worksFor",
        vec![iri("acme")],
        vec![iri("bob")],
        vec![iri("acme")],
    );
    c.settings = settings;
    let head = ds.snapshot().commit;
    let check = ds.forms().check(&c, &Default::default()).unwrap();
    assert!(!check.withheld);
    assert_eq!(check.conforms, Some(false));
    let results = check.results.unwrap();
    let class = results
        .iter()
        .find(|r| {
            r["sourceConstraintComponent"]["value"]
                == "http://www.w3.org/ns/shacl#ClassConstraintComponent"
        })
        .expect("an sh:class result");
    assert_eq!(class["field"], format!("<{}>", ex("worksFor")));
    assert!(check.preview.commit.is_some());
    assert_eq!(check.preview.graphs[0].inserted, 1);
    assert_eq!(check.preview.graphs[0].deleted, 1);
    assert!(check.update.contains("DELETE DATA"), "{}", check.update);
    // nothing was written
    assert_eq!(ds.snapshot().commit, head);
}

#[test]
fn a5_a_save_commits_once_and_a_stale_form_gets_the_conflict() {
    let (ds, settings) = dataset();
    let old = lit("alice@example.org");
    let mut first = change(
        "email",
        vec![old.clone()],
        vec![lit("al@example.org")],
        vec![old.clone()],
    );
    first.settings = settings.clone();
    let FormSaved::Saved(receipt) = ds.forms().save(&first, &Default::default()).unwrap() else {
        panic!("the first save conflicts");
    };
    assert!(receipt.committed);
    assert_eq!(receipt.annotation.message.as_deref(), Some("fix email"));
    let m = model(&ds, &settings, "alice");
    assert_eq!(
        field(&m, "Email")["values"][0]["term"]["value"],
        "al@example.org"
    );
    // the second page loaded the old email
    let mut second = change(
        "email",
        vec![old.clone()],
        vec![lit("x@example.org")],
        vec![old],
    );
    second.settings = settings.clone();
    match ds.forms().save(&second, &Default::default()).unwrap() {
        FormSaved::Conflict(c) => {
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].id, format!("<{}>", ex("email")));
            assert_eq!(c[0].now, vec![lit("al@example.org")]);
        }
        FormSaved::Saved(_) => panic!("a stale save went through"),
    }
    // a change to another field from the same stale page saves
    let mut role = change(
        "role",
        vec![lit("Engineer")],
        vec![lit("Manager")],
        vec![lit("Engineer")],
    );
    role.settings = settings;
    assert!(matches!(
        ds.forms().save(&role, &Default::default()).unwrap(),
        FormSaved::Saved(_)
    ));
}

#[test]
fn a_save_needs_the_loaded_values_and_refuses_read_only_fields() {
    let (ds, settings) = dataset();
    let mut c = change("email", vec![], vec![lit("y@example.org")], vec![]);
    c.expect.clear();
    c.settings = settings.clone();
    let e = ds.forms().save(&c, &Default::default()).unwrap_err();
    assert!(e.to_string().contains("expect"), "{e}");
    let id = format!("<{}>/<{}>", ex("worksFor"), ex("name"));
    let c = FormChange {
        focus: ex("alice"),
        changes: vec![FieldChange {
            field: id.clone(),
            add: vec![lit("x")],
            ..Default::default()
        }],
        expect: BTreeMap::from([(id, vec![])]),
        settings,
        ..Default::default()
    };
    let e = ds.forms().save(&c, &Default::default()).unwrap_err();
    assert!(e.to_string().contains("complex path"), "{e}");
}

#[test]
fn inverse_fields_write_the_focus_as_object() {
    let (ds, settings) = dataset();
    let id = format!("^<{}>", ex("member"));
    let c = FormChange {
        focus: ex("alice"),
        changes: vec![FieldChange {
            field: id.clone(),
            add: vec![iri("platform")],
            ..Default::default()
        }],
        expect: BTreeMap::from([(id, vec![iri("core")])]),
        settings,
        ..Default::default()
    };
    assert!(matches!(
        ds.forms().save(&c, &Default::default()).unwrap(),
        FormSaved::Saved(_)
    ));
    let q = format!(
        "ASK {{ <{}> <{}> <{}> }}",
        ex("platform"),
        ex("member"),
        ex("alice")
    );
    assert!(ds.ask(&q).unwrap());
}

#[test]
fn guard_shapes_and_their_rejections() {
    let ds = Dataset::memory();
    ds.update(DATA).unwrap();
    let cfg = json!({ "mode": "reject", "shapes": { "inline": SHAPES } });
    ds.validation()
        .guard()
        .set_shacl(serde_json::from_value(cfg).unwrap())
        .unwrap();
    let settings = FormSettings::default();
    let m = model(&ds, &settings, "alice");
    assert_eq!(m["shapes"]["source"], "guard");
    assert_eq!(field(&m, "Name")["constraints"]["minCount"], 1);
    // removing the only name: the check and the guard both refuse
    let mut c = change(
        "name",
        vec![lit("Alice Lima")],
        vec![],
        vec![lit("Alice Lima")],
    );
    c.settings = settings.clone();
    let check = ds.forms().check(&c, &Default::default()).unwrap();
    assert!(check.preview.rejected());
    assert_eq!(check.conforms, Some(false));
    let e = ds.forms().save(&c, &Default::default()).unwrap_err();
    assert!(matches!(e, sparkles::Error::Rejected(_)), "{e}");
    // without a guard, the guard source says so
    let plain = Dataset::memory();
    plain.update(DATA).unwrap();
    let m = model(&plain, &settings, "alice");
    assert_eq!(m["fields"], json!([]));
    assert!(
        m["shapes"]["missing"]
            .as_str()
            .unwrap()
            .contains("no write-time")
    );
}

#[test]
fn drafted_shapes_describe_the_types_of_the_data() {
    let (ds, _) = dataset();
    let settings = FormSettings {
        shapes: ShapesSource::Drafted,
        ..Default::default()
    };
    let m = model(&ds, &settings, "alice");
    assert_eq!(m["shapes"]["source"], "drafted");
    assert!(
        !m["shapes"]["nodeShapes"].as_array().unwrap().is_empty(),
        "{m}"
    );
    let paths: Vec<&str> = m["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&ex("name").as_str()), "{paths:?}");
}

#[test]
fn settings_are_checked() {
    let mut s = FormSettings::default();
    assert!(s.validate().is_ok());
    s.shapes = ShapesSource::Graphs;
    assert!(s.validate().unwrap_err().contains("graphs"));
    s.graphs = vec!["urn:shapes".into()];
    assert!(s.validate().is_ok());
    s.branch_template = "edits/{user}".into();
    assert!(s.validate().is_err());
    s.branch_template = "edits.{user}.{date}".into();
    assert_eq!(
        s.session_branch("ana@x.org", "20261012"),
        "edits.ana-x.org.20261012"
    );
    s.target_graph = "nope".into();
    assert!(s.validate().is_err());
    s.target_graph = "urn:g".into();
    s.new_iri = "http://example.org/person/{uuid}".into();
    assert!(s.validate().is_ok());
    let j: FormSettings = serde_json::from_value(json!({})).unwrap();
    assert_eq!(j, FormSettings::default());
    assert!(serde_json::from_value::<FormSettings>(json!({ "x": 1 })).is_err());
}

#[test]
fn a_new_instance_takes_a_minted_iri_its_type_and_its_values() {
    let (ds, settings) = dataset();
    // the form of a new Person: the class's shapes, no values and no focus
    let mut req = FormRequest::new(settings.mint_iri(), settings.clone());
    req.class = Some(ex("Person"));
    let m = serde_json::to_value(ds.forms().model(&req, &Default::default()).unwrap()).unwrap();
    assert_eq!(m["focus"], Value::Null);
    assert_eq!(m["types"], json!([ex("Person")]));
    assert_eq!(field(&m, "Name")["values"], json!([]));
    assert!(m["editable"].as_bool().unwrap());
    // a subclass of Person gets the same shape
    ds.update(&format!(
        "INSERT DATA {{ <{EX}Manager> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{EX}Person> }}"
    ))
    .unwrap();
    req.class = Some(ex("Manager"));
    let m = serde_json::to_value(ds.forms().model(&req, &Default::default()).unwrap()).unwrap();
    assert_eq!(m["shapes"]["nodeShapes"], json!([ex("PersonShape")]));
    // a class no shape targets has no fields
    req.class = Some(ex("Org"));
    let m = serde_json::to_value(ds.forms().model(&req, &Default::default()).unwrap()).unwrap();
    assert_eq!(m["fields"], json!([]));

    let focus = settings.mint_iri();
    assert!(focus.starts_with("urn:uuid:"), "{focus}");
    let mut c = change("name", vec![], vec![lit("Carol")], vec![]);
    c.focus = focus.clone();
    c.class = Some(ex("Person"));
    c.settings = settings.clone();
    c.changes.push(FieldChange {
        field: format!("<{EX}worksFor>"),
        remove: vec![],
        add: vec![iri("acme")],
    });
    c.expect.insert(format!("<{EX}worksFor>"), vec![]);
    let check = ds.forms().check(&c, &Default::default()).unwrap();
    assert_eq!(check.conforms, Some(true), "{:?}", check.results);
    assert!(
        check
            .update
            .contains("<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>")
    );
    let FormSaved::Saved(_) = ds.forms().save(&c, &Default::default()).unwrap() else {
        panic!("the new instance was not saved");
    };
    assert!(
        ds.ask(&format!(
            "ASK {{ <{focus}> a <{EX}Person> ; <{EX}name> \"Carol\" ; <{EX}worksFor> <{EX}acme> }}"
        ))
        .unwrap()
    );
    // the same IRI again: it describes a resource now
    let e = ds.forms().save(&c, &Default::default()).unwrap_err();
    assert!(e.to_string().contains("already describes"), "{e}");
    // a template mints under its namespace
    let t = FormSettings {
        new_iri: format!("{EX}person/{{uuid}}"),
        ..settings
    };
    assert!(t.mint_iri().starts_with(&ex("person/")));
}
