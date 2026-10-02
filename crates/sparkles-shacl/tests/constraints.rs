//! The constraints layer of the schema report (`sparkles_shacl::constraints`): the
//! property shapes that apply to the instances of each target class.

use sparkles::guard::{GuardMode, Severity};
use sparkles::io::{RdfFormat, Source};
use sparkles::schema::constraints::{ClassConstraints, Enforcement, SourceKind};
use sparkles::store::{Store, StoreOptions};
use sparkles_shacl::Shapes;
use sparkles_shacl::constraints::{Checked, class_constraints, graphs_source};

const EX: &str = "http://ex.org/";
const SH: &str = "http://www.w3.org/ns/shacl#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

const SHAPES: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ex: <http://ex.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

ex:PersonShape a sh:NodeShape ;
    sh:targetClass ex:Person ;
    sh:closed true ;
    sh:ignoredProperties ( rdf:type ) ;
    sh:property ex:nameShape ,
        [ sh:path ex:knows ; sh:class ex:Person ; sh:nodeKind sh:IRI ] ,
        [ sh:path [ sh:inversePath ex:knows ] ; sh:maxCount 10 ] ;
    sh:node ex:Aged .

ex:nameShape sh:path ex:name ;
    sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ;
    sh:pattern "^A" ; sh:severity sh:Warning .

ex:Aged a sh:NodeShape ;
    sh:property [ sh:path ex:age ; sh:maxCount 1 ] .

ex:Org a rdfs:Class, sh:NodeShape ;
    sh:property [ sh:path ex:name ; sh:minCount 1 ] .

ex:NodeTarget a sh:NodeShape ;
    sh:targetNode ex:x ;
    sh:property [ sh:path ex:name ; sh:minCount 1 ] .

ex:Off a sh:NodeShape ;
    sh:targetClass ex:Person ;
    sh:deactivated true ;
    sh:property [ sh:path ex:zzz ; sh:minCount 1 ] .

ex:Either a sh:NodeShape ;
    sh:targetClass ex:Person ;
    sh:or ( [ sh:path ex:email ; sh:minCount 1 ] [ sh:path ex:phone ; sh:minCount 1 ] ) .
"#;

fn ex(s: &str) -> String {
    format!("{EX}{s}")
}

fn class<'a>(classes: &'a [ClassConstraints], c: &str) -> &'a ClassConstraints {
    classes
        .iter()
        .find(|x| x.class == ex(c))
        .unwrap_or_else(|| panic!("no class {c}"))
}

#[test]
fn class_constraints_follow_targets_node_and_property_shapes() {
    let shapes = Shapes::parse(SHAPES, RdfFormat::Turtle, None).unwrap();
    let (classes, other_targets) = class_constraints(&shapes, Checked::OnRequest);
    assert_eq!(other_targets, 1, "ex:NodeTarget");
    let names: Vec<&str> = classes.iter().map(|c| c.class.as_str()).collect();
    assert_eq!(names, [ex("Org"), ex("Person")]);

    let person = class(&classes, "Person");
    assert_eq!(person.shapes, [ex("Either"), ex("PersonShape")]);
    assert!(person.closed);
    // the inverse path is counted, not listed; sh:or and deactivated shapes add nothing
    assert_eq!(person.other_paths, 1);
    let paths: Vec<&str> = person.properties.iter().map(|p| p.path.as_str()).collect();
    assert_eq!(paths, [ex("age"), ex("knows"), ex("name")]);

    let age = &person.properties[0];
    assert_eq!((age.min_count, age.max_count), (None, Some(1)));
    let knows = &person.properties[1];
    assert_eq!(knows.class, [ex("Person")]);
    assert_eq!(knows.node_kind.as_deref(), Some(&*format!("{SH}IRI")));
    let name = &person.properties[2];
    assert_eq!(name.shape.as_deref(), Some(&*ex("nameShape")));
    assert_eq!((name.min_count, name.max_count), (Some(1), Some(1)));
    assert_eq!(name.datatype.as_deref(), Some(&*format!("{XSD}string")));
    assert_eq!(name.other, [format!("{SH}PatternConstraintComponent")]);
    assert_eq!(name.severity, format!("{SH}Warning"));
    assert!(
        person
            .properties
            .iter()
            .all(|p| p.enforcement == Enforcement::ValidatedOnRequest)
    );
    assert_eq!(
        name.summary(|i| i.replace(XSD, "xsd:")),
        "min 1 · max 1 · datatype xsd:string · +Pattern"
    );

    // an implicit class target
    let org = class(&classes, "Org");
    assert_eq!(org.shapes, [ex("Org")]);
    assert_eq!(org.properties.len(), 1);
    assert_eq!(org.properties[0].min_count, Some(1));
}

#[test]
fn enforcement_follows_the_guard_mode_and_threshold() {
    let shapes = Shapes::parse(SHAPES, RdfFormat::Turtle, None).unwrap();
    let of = |checked| {
        let (classes, _) = class_constraints(&shapes, checked);
        let person = class(&classes, "Person").clone();
        person
            .properties
            .iter()
            .map(|p| (p.path.trim_start_matches(EX).to_string(), p.enforcement))
            .collect::<Vec<_>>()
    };
    use Enforcement::*;
    let reject = of(Checked::OnWrite {
        mode: GuardMode::Reject,
        threshold: Severity::Violation,
    });
    assert_eq!(
        reject,
        [
            ("age".into(), RejectOnWrite),
            ("knows".into(), RejectOnWrite),
            ("name".into(), WarnOnWrite),
        ]
    );
    let strict = of(Checked::OnWrite {
        mode: GuardMode::Reject,
        threshold: Severity::Warning,
    });
    assert!(strict.iter().all(|(_, e)| *e == RejectOnWrite));
    let warn = of(Checked::OnWrite {
        mode: GuardMode::Warn,
        threshold: Severity::Violation,
    });
    assert!(warn.iter().all(|(_, e)| *e == WarnOnWrite));
}

#[test]
fn graphs_source_reads_shapes_graphs_of_the_store() {
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            SHAPES.as_bytes().to_vec(),
            RdfFormat::Turtle,
            Some(oxrdf::NamedNode::new(ex("shapes")).unwrap()),
        )])
        .unwrap();
    let snap = store.snapshot();
    let src = graphs_source(&snap, &[ex("shapes")]).unwrap();
    assert_eq!(src.kind, SourceKind::Graphs);
    assert_eq!(src.graphs, [ex("shapes")]);
    assert_eq!(src.classes.len(), 2);
    assert!(src.shapes > 5);
    // the default graph holds no shapes
    let empty = graphs_source(&snap, &["default".to_string()]).unwrap();
    assert!(empty.classes.is_empty());
    assert_eq!(empty.shapes, 0);
}
