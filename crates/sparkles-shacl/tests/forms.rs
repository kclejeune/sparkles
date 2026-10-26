//! The fields of editing forms (`sparkles_shacl::form`, spec U02): the non-validating
//! characteristics the parser keeps, the shapes that apply to a node, and the fields of
//! their property shapes merged by path.

use oxrdf::{NamedNode, Term};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_shacl::form::{FieldGroup, FieldPath, form_layout, form_layout_with, pick_lang};
use sparkles_shacl::{Shapes, ValidateOptions, shapes_for_node};

const SHAPES: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ex: <http://ex.org/> .
@prefix dash: <http://datashapes.org/dash#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

ex:Identity a sh:PropertyGroup ; rdfs:label "Identity"@en, "Identité"@fr ; sh:order 1 .

ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:name "Name"@en, "Nom"@fr ; sh:minCount 1 ;
                sh:datatype xsd:string ; sh:order 1 ; sh:group ex:Identity ;
                sh:description "The full name" ] ;
  sh:property [ sh:path ex:email ; sh:maxCount 1 ; sh:pattern "^[^@]+@[^@]+$" ; sh:order 2 ] ;
  sh:property [ sh:path ex:worksFor ; sh:class ex:Org ; sh:minCount 1 ] ;
  sh:property [ sh:path ex:role ; sh:in ( "Engineer" "Manager" "Other" ) ] ;
  sh:property [ sh:path [ sh:inversePath ex:member ] ; sh:class ex:Team ] ;
  sh:property [ sh:path ( ex:worksFor ex:name ) ; sh:maxCount 1 ] ;
  sh:property [ sh:path ex:bio ; sh:datatype xsd:string ; dash:singleLine false ;
                dash:editor dash:TextAreaEditor ] ;
  sh:property [ sh:path ex:id ; dash:readOnly true ; sh:defaultValue "x" ] ;
  sh:node ex:Aged .

ex:Aged a sh:NodeShape ; sh:name "Age facts" ;
  sh:property [ sh:path ex:age ; sh:datatype xsd:integer ; sh:minInclusive 0 ; sh:maxInclusive 150 ] .

ex:EmployeeShape a sh:NodeShape ; sh:targetClass ex:Employee ;
  sh:property [ sh:path ex:name ; sh:minCount 2 ; sh:maxLength 40 ] ;
  sh:property [ sh:path ex:age ; sh:minInclusive 18 ; sh:maxInclusive 200 ] .

ex:Off a sh:NodeShape ; sh:targetClass ex:Person ; sh:deactivated true ;
  sh:property [ sh:path ex:zzz ; sh:minCount 1 ] .

ex:OrgShape a sh:NodeShape ; sh:targetClass ex:Org ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ] .
"#;

const DATA: &str = r#"
@prefix ex: <http://ex.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:Employee rdfs:subClassOf ex:Person .
ex:alice a ex:Employee ; ex:name "Alice" .
ex:bob a ex:Person .
ex:acme a ex:Org .
"#;

fn ex(s: &str) -> NamedNode {
    NamedNode::new(format!("http://ex.org/{s}")).unwrap()
}

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn shapes() -> Shapes {
    Shapes::parse(SHAPES, RdfFormat::Turtle, None).unwrap()
}

#[test]
fn controlled_layout_keeps_merged_fields_and_uses_one_shared_allowance() {
    let shapes = shapes();
    let selected =
        ["PersonShape", "EmployeeShape"].map(|name| shapes.shape_id(&ex(name).into()).unwrap());
    let ordinary = form_layout(&shapes, &selected);
    let mut spent = 0;
    let mut check = || {
        spent += 1;
        anyhow::ensure!(spent <= 10_000, "layout allowance exhausted");
        Ok(())
    };
    let controlled = form_layout_with(&shapes, &selected, &mut check).unwrap();
    assert_eq!(format!("{controlled:?}"), format!("{ordinary:?}"));
    let first = spent;
    let mut check = || {
        spent += 1;
        anyhow::ensure!(spent < first * 2, "layout allowance exhausted");
        Ok(())
    };
    let failure = form_layout_with(&shapes, &selected, &mut check).unwrap_err();
    assert_eq!(failure.to_string(), "layout allowance exhausted");
}

#[test]
fn controlled_layout_stops_before_copying_one_large_label_or_path() {
    for predicate in [false, true] {
        let large = "x".repeat(1024 * 1024);
        let property = if predicate {
            format!("sh:path <http://ex.org/{large}>")
        } else {
            format!("sh:path ex:p ; sh:name \"{large}\"")
        };
        let text = format!(
            "@prefix sh: <http://www.w3.org/ns/shacl#> .\n\
             @prefix ex: <http://ex.org/> .\n\
             ex:S a sh:NodeShape ; sh:property [ {property} ] ."
        );
        let shapes = Shapes::parse(&text, RdfFormat::Turtle, None).unwrap();
        let root = shapes.shape_id(&ex("S").into()).unwrap();
        let mut spent = 0;
        let failure = form_layout_with(&shapes, &[root], &mut || {
            spent += 1;
            anyhow::ensure!(spent <= 20, "layout allowance exhausted");
            Ok(())
        })
        .unwrap_err();
        assert_eq!(failure.to_string(), "layout allowance exhausted");
        assert_eq!(spent, 21);
    }
}

#[test]
fn controlled_layout_preserves_cancellation_and_rejects_deep_expansion() {
    let shapes = shapes();
    let root = shapes.shape_id(&ex("PersonShape").into()).unwrap();
    let failure =
        form_layout_with(&shapes, &[root], &mut || anyhow::bail!("cancelled")).unwrap_err();
    assert_eq!(failure.to_string(), "cancelled");

    let mut text = String::from(
        "@prefix sh: <http://www.w3.org/ns/shacl#> .\n\
         @prefix ex: <http://ex.org/> .\n",
    );
    for n in 0..80 {
        text.push_str(&format!("ex:S{n} sh:node ex:S{} .\n", n + 1));
    }
    text.push_str("ex:S80 sh:property [ sh:path ex:p ] .");
    let shapes = Shapes::parse(&text, RdfFormat::Turtle, None).unwrap();
    let root = shapes.shape_id(&ex("S0").into()).unwrap();
    let failure = form_layout_with(&shapes, &[root], &mut || Ok(())).unwrap_err();
    assert!(failure.to_string().contains("nesting exceeds"));
}

#[test]
fn controlled_layout_handles_cycles_and_repeated_allowed_value_intersections() {
    let text = r#"
        @prefix sh: <http://www.w3.org/ns/shacl#> .
        @prefix ex: <http://ex.org/> .
        ex:A sh:node ex:B ; sh:property [ sh:path ex:p ; sh:in ("a" "b" "c") ] .
        ex:B sh:node ex:A ; sh:property [ sh:path ex:p ; sh:in ("b" "c") ; sh:in ("c" "d") ] .
    "#;
    let shapes = Shapes::parse(text, RdfFormat::Turtle, None).unwrap();
    let root = shapes.shape_id(&ex("A").into()).unwrap();
    let ordinary = form_layout(&shapes, &[root]);
    let controlled = form_layout_with(&shapes, &[root], &mut || Ok(())).unwrap();
    assert_eq!(format!("{controlled:?}"), format!("{ordinary:?}"));
    assert_eq!(controlled.fields.len(), 1);
    assert_eq!(
        controlled.fields[0].constraints.allowed.as_ref().unwrap(),
        &vec![Term::Literal(oxrdf::Literal::new_simple_literal("c"))]
    );
}

#[test]
fn the_parser_keeps_names_orders_groups_and_dash_hints() {
    let shapes = shapes();
    let name = shapes
        .shapes()
        .iter()
        .find(|s| s.info().description_is("The full name"))
        .expect("the name shape");
    let info = name.info();
    assert_eq!(info.order, Some(1.0));
    assert_eq!(info.names.len(), 2);
    let g = info.group.clone().unwrap();
    assert_eq!(g, Term::from(ex("Identity")));
    let group = shapes.group(&g).unwrap();
    assert_eq!(group.order, Some(1.0));
    assert_eq!(
        pick_lang(&group.labels, Some("fr")).unwrap().value(),
        "Identité"
    );
    assert_eq!(
        pick_lang(&group.labels, Some("de")).unwrap().value(),
        "Identity"
    );
    assert_eq!(pick_lang(&info.names, None).unwrap().value(), "Name");
    let bio = shapes
        .shapes()
        .iter()
        .find(|s| s.info().single_line == Some(false))
        .unwrap();
    assert_eq!(
        bio.info().editor.as_ref().unwrap().as_str(),
        "http://datashapes.org/dash#TextAreaEditor"
    );
    let id = shapes.shapes().iter().find(|s| s.info().read_only).unwrap();
    assert_eq!(
        id.info().default_value,
        Some(Term::from(oxrdf::Literal::new_simple_literal("x")))
    );
    // shapes without any keep no box
    let org = shapes.shape(&ex("OrgShape").into()).unwrap();
    assert!(org.info.is_none());
}

trait Described {
    fn description_is(&self, s: &str) -> bool;
}

impl Described for sparkles_shacl::ShapeInfo {
    fn description_is(&self, s: &str) -> bool {
        self.descriptions.iter().any(|d| d.value() == s)
    }
}

#[test]
fn shapes_apply_by_class_with_subclasses_and_skip_deactivated_ones() {
    let store = store();
    let snap = store.snapshot();
    let shapes = shapes();
    let names = |node: &str| -> Vec<String> {
        let mut v: Vec<String> = shapes_for_node(
            &snap,
            &shapes,
            &ex(node).into(),
            &ValidateOptions::default(),
        )
        .unwrap()
        .into_iter()
        .map(|si| shapes.shapes()[si].node.to_string())
        .collect();
        v.sort();
        v
    };
    assert_eq!(
        names("alice"),
        vec![
            "<http://ex.org/EmployeeShape>",
            "<http://ex.org/PersonShape>"
        ]
    );
    assert_eq!(names("bob"), vec!["<http://ex.org/PersonShape>"]);
    assert_eq!(names("acme"), vec!["<http://ex.org/OrgShape>"]);
    assert!(names("nobody").is_empty());
}

#[test]
fn fields_merge_by_path_and_keep_the_stricter_constraints() {
    let store = store();
    let snap = store.snapshot();
    let shapes = shapes();
    let applying = shapes_for_node(
        &snap,
        &shapes,
        &ex("alice").into(),
        &ValidateOptions::default(),
    )
    .unwrap();
    let layout = form_layout(&shapes, &applying);
    let mut ids: Vec<String> = layout.fields.iter().map(|f| f.id()).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "<http://ex.org/age>",
            "<http://ex.org/bio>",
            "<http://ex.org/email>",
            "<http://ex.org/id>",
            "<http://ex.org/name>",
            "<http://ex.org/role>",
            "<http://ex.org/worksFor>",
            "<http://ex.org/worksFor>/<http://ex.org/name>",
            "^<http://ex.org/member>",
        ]
    );
    let field = |id: &str| layout.fields.iter().find(|f| f.id() == id).unwrap();
    let name = field("<http://ex.org/name>");
    assert_eq!(name.sources.len(), 2);
    assert_eq!(name.constraints.min_count, Some(2));
    assert_eq!(name.constraints.max_length, Some(40));
    assert_eq!(name.widget(), "text");
    assert_eq!(name.group, Some(FieldGroup::Group(ex("Identity").into())));
    let age = field("<http://ex.org/age>");
    // reached through sh:node: grouped under the node shape
    assert_eq!(age.group, Some(FieldGroup::Shape(ex("Aged").into())));
    assert_eq!(
        age.constraints
            .min_inclusive
            .as_ref()
            .unwrap()
            .term
            .to_string(),
        "\"18\"^^<http://www.w3.org/2001/XMLSchema#integer>"
    );
    assert_eq!(
        age.constraints
            .max_inclusive
            .as_ref()
            .unwrap()
            .term
            .to_string(),
        "\"150\"^^<http://www.w3.org/2001/XMLSchema#integer>"
    );
    assert_eq!(age.widget(), "number");
    assert_eq!(field("<http://ex.org/role>").widget(), "radio");
    assert_eq!(field("<http://ex.org/worksFor>").widget(), "entity");
    assert_eq!(field("<http://ex.org/bio>").widget(), "textarea");
    assert_eq!(field("<http://ex.org/email>").widget(), "text");
    assert!(field("<http://ex.org/id>").read_only);
    assert!(matches!(
        field("^<http://ex.org/member>").path,
        FieldPath::Inverse(_)
    ));
    assert!(matches!(
        field("<http://ex.org/worksFor>/<http://ex.org/name>").path,
        FieldPath::Complex(_)
    ));
    assert!(!layout.closed);
}
