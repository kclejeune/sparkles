//! The constraints layer of a schema report from the schema of a ShEx guard.

use sparkles_core::schema::constraints::{Enforcement, SourceKind};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_shex::NoImports;
use sparkles_shex::constraints::guard_source;
use sparkles_shex::guard::{SetOutcome, ShexValidationConfig, set_config};

const SCHEMA: &str = "PREFIX ex: <http://ex.org/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
ex:PersonShape CLOSED {
  ex:name xsd:string + ;
  ex:age xsd:integer MININCLUSIVE 0 ? ;
  ex:status [\"active\" \"inactive\"] {1} ;
  ex:employer @ex:OrgShape ? ;
  ex:knows @ex:PersonShape * ;
  ^ex:manages @ex:PersonShape ? ;
  ex:note LITERAL /^[a-z]+$/ ?
}
ex:OrgShape { ex:title . ; ( ex:email IRI | ex:phone LITERAL ) }
ex:EmployerShape { ex:title . }";

fn config(mode: &str) -> ShexValidationConfig {
    let c: ShexValidationConfig = serde_json::from_value(serde_json::json!({
        "language": "shex",
        "mode": mode,
        "baseline": "grandfather",
        "schema": {"inline": SCHEMA, "format": "shexc"},
        "shapeMap": "{FOCUS a ex:Person}@ex:PersonShape, {FOCUS a ex:Org}@ex:OrgShape, {FOCUS a ex:Employer}@ex:EmployerShape, {FOCUS a ex:Employer}@ex:OrgShape, ex:one@ex:OrgShape",
    }))
    .unwrap();
    c.check().unwrap();
    c
}

fn source(mode: &str) -> sparkles_core::schema::constraints::ConstraintSource {
    let store = Store::in_memory(StoreOptions::default());
    let SetOutcome::Installed(g, _) = set_config(&store, Some(config(mode)), &NoImports).unwrap()
    else {
        panic!("not installed")
    };
    guard_source(&g)
}

#[test]
fn a_shex_guard_is_summarized_by_class() {
    let s = source("reject");
    assert_eq!(s.kind, SourceKind::Guard);
    assert_eq!(s.mode.as_deref(), Some("reject"));
    assert_eq!(s.shapes, 3);
    // the fixed node `ex:one` is not selected by a class
    assert_eq!(s.other_targets, 1);
    let classes: Vec<&str> = s.classes.iter().map(|c| c.class.as_str()).collect();
    assert_eq!(
        classes,
        [
            "http://ex.org/Employer",
            "http://ex.org/Org",
            "http://ex.org/Person"
        ]
    );
    let person = s
        .classes
        .iter()
        .find(|c| c.class == "http://ex.org/Person")
        .unwrap();
    assert!(person.closed);
    assert_eq!(person.shapes, ["http://ex.org/PersonShape"]);
    let by = |path: &str, inverse: bool| {
        person
            .properties
            .iter()
            .find(|p| p.path == format!("http://ex.org/{path}") && p.inverse == inverse)
            .unwrap_or_else(|| panic!("no {path}: {:#?}", person.properties))
    };
    let name = by("name", false);
    assert_eq!((name.min_count, name.max_count), (Some(1), None));
    assert_eq!(
        name.datatype.as_deref(),
        Some("http://www.w3.org/2001/XMLSchema#string")
    );
    assert_eq!(name.enforcement, Enforcement::RejectOnWrite);
    let age = by("age", false);
    assert_eq!((age.min_count, age.max_count), (None, Some(1)));
    assert!(age.other[0].ends_with("MinInclusiveConstraintComponent"));
    let status = by("status", false);
    assert_eq!(status.values, ["\"active\"", "\"inactive\""]);
    assert_eq!((status.min_count, status.max_count), (Some(1), Some(1)));
    // a reference to a shape that one class selects is that class; two shapes select Org
    // (and Employer), so a reference to OrgShape is only named
    let employer = by("employer", false);
    assert_eq!(employer.class, Vec::<String>::new());
    assert!(employer.other.iter().any(|o| o.ends_with("ShapeReference")));
    let knows = by("knows", false);
    assert_eq!(knows.class, ["http://ex.org/Person"]);
    assert_eq!((knows.min_count, knows.max_count), (None, None));
    let manages = by("manages", true);
    assert_eq!(manages.class, ["http://ex.org/Person"]);
    assert_eq!(manages.max_count, Some(1));
    let note = by("note", false);
    assert_eq!(
        note.node_kind.as_deref(),
        Some("http://www.w3.org/ns/shacl#Literal")
    );
    assert!(
        note.other
            .iter()
            .any(|o| o.ends_with("PatternConstraintComponent"))
    );
    // a choice is not a cardinality of the shape
    let org = s
        .classes
        .iter()
        .find(|c| c.class == "http://ex.org/Org")
        .unwrap();
    let email = org
        .properties
        .iter()
        .find(|p| p.path == "http://ex.org/email")
        .unwrap();
    assert_eq!((email.min_count, email.max_count), (None, None));
    assert!(email.other.iter().any(|o| o.ends_with("Group")));
    let title = org
        .properties
        .iter()
        .find(|p| p.path == "http://ex.org/title")
        .unwrap();
    assert_eq!(title.min_count, Some(1));
}

#[test]
fn warn_mode_only_reports() {
    let s = source("warn");
    assert!(
        s.classes
            .iter()
            .flat_map(|c| &c.properties)
            .all(|p| p.enforcement == Enforcement::WarnOnWrite)
    );
}
