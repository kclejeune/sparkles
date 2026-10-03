//! Per-class property profiles ([`profiles`]).

use super::profile::{ClassProfile, PropertyProfile};
use super::*;
use crate::io::{RdfFormat, Source};
use crate::sparql::QueryOptions;
use crate::store::{Store, StoreOptions};

const DATA: &str = r#"@prefix ex: <http://ex.org/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:a a ex:Person ; ex:name "A" ; ex:age 30 ; ex:knows ex:b .
ex:b a ex:Person, ex:Employee ; ex:name "B", "Bee"@en ; ex:worksFor ex:org .
ex:org a ex:Org ; ex:name "O" .
ex:c ex:name "untyped" ; ex:knows ex:a .
ex:g { ex:a ex:age 31 . ex:a ex:knows _:x . _:x a ex:Person }
"#;

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    s
}

fn ex(l: &str) -> String {
    format!("http://ex.org/{l}")
}

fn class<'a>(p: &'a ClassProfiles, c: &str) -> &'a ClassProfile {
    p.classes
        .iter()
        .find(|x| x.class == ex(c))
        .unwrap_or_else(|| panic!("no profile of {c}"))
}

fn prop<'a>(c: &'a ClassProfile, p: &str) -> &'a PropertyProfile {
    c.properties
        .iter()
        .find(|x| x.predicate == ex(p))
        .unwrap_or_else(|| panic!("no {p} in {}", c.class))
}

fn run(s: &Store, graph: GraphSelection, classes: &[&str]) -> ClassProfiles {
    profiles(
        &s.snapshot(),
        &ProfileOptions {
            schema: SchemaOptions {
                graph,
                ..Default::default()
            },
            classes: classes.iter().map(|c| ex(c)).collect(),
        },
    )
    .unwrap()
}

#[test]
fn predicates_of_the_instances_of_each_class() {
    let s = store();
    let p = run(&s, GraphSelection::Default, &[]);
    assert_eq!(
        p.classes
            .iter()
            .map(|c| c.class.as_str())
            .collect::<Vec<_>>(),
        [ex("Employee"), ex("Org"), ex("Person")]
    );
    let person = class(&p, "Person");
    assert_eq!(person.instances, 2);
    // most used first, then by IRI
    assert_eq!(
        person
            .properties
            .iter()
            .map(|x| x.predicate.as_str())
            .collect::<Vec<_>>(),
        [ex("name"), ex("age"), ex("knows"), ex("worksFor")]
    );
    let name = prop(person, "name");
    assert_eq!(
        (
            name.instances,
            name.triples,
            name.min_per_instance,
            name.max_per_instance
        ),
        (2, 3, 1, 2)
    );
    let lits: Vec<(&str, u64)> = name
        .objects
        .literals
        .iter()
        .map(|d| (d.datatype.as_str(), d.triples))
        .collect();
    assert_eq!(
        lits,
        [
            ("http://www.w3.org/1999/02/22-rdf-syntax-ns#langString", 1),
            ("http://www.w3.org/2001/XMLSchema#string", 2)
        ]
    );
    let age = prop(person, "age");
    assert_eq!(
        age.objects.literals[0].datatype,
        format!("{}integer", XSD_NS)
    );
    let knows = prop(person, "knows");
    assert_eq!((knows.objects.iri, knows.object_classes.len()), (1, 2));
    assert_eq!(
        knows
            .object_classes
            .iter()
            .map(|c| (c.class.as_str(), c.triples))
            .collect::<Vec<_>>(),
        [(ex("Employee").as_str(), 1), (ex("Person").as_str(), 1)]
    );
    assert_eq!(prop(person, "worksFor").object_classes[0].class, ex("Org"));
    // ex:c knows ex:a and ex:a knows ex:b: two arcs into two people
    let inc = &person.incoming;
    assert_eq!(inc.len(), 1);
    assert_eq!(
        (inc[0].predicate.as_str(), inc[0].triples, inc[0].instances),
        (ex("knows").as_str(), 2, 2)
    );
    let org = class(&p, "Org");
    assert_eq!(org.incoming[0].predicate, ex("worksFor"));
    // the instances agree with the report's
    let report = discover(&s.snapshot(), &SchemaOptions::default()).unwrap();
    for c in &p.classes {
        let r = report.classes.iter().find(|x| x.iri == c.class).unwrap();
        assert_eq!(r.observed.instances, c.instances);
    }
}

#[test]
fn selection_requested_classes_and_changes() {
    let s = store();
    let p = run(&s, GraphSelection::Union, &["Person", "Missing"]);
    assert_eq!(p.classes.len(), 2);
    assert_eq!(class(&p, "Missing").instances, 0);
    let person = class(&p, "Person");
    // the blank node typed in ex:g is a person too
    assert_eq!(person.instances, 3);
    let age = prop(person, "age");
    assert_eq!((age.triples, age.max_per_instance), (2, 2));
    assert_eq!(prop(person, "knows").objects.blank, 1);
    let g = GraphSelection::parse("http://ex.org/g").unwrap();
    let p = run(&s, g, &[]);
    assert_eq!(p.classes.len(), 1);
    assert_eq!(class(&p, "Person").properties.len(), 0);
    // delta terms and deletions
    crate::sparql::update::update(
        &s,
        "PREFIX ex: <http://ex.org/>
         DELETE DATA { ex:a ex:age 30 } ;
         INSERT DATA { ex:org ex:name \"Org\"@en ; ex:founded \"1999-01-01\"^^<http://www.w3.org/2001/XMLSchema#date> }",
        &QueryOptions::default(),
    )
    .unwrap();
    let p = run(&s, GraphSelection::Default, &[]);
    assert!(
        class(&p, "Person")
            .properties
            .iter()
            .all(|x| x.predicate != ex("age"))
    );
    let org = class(&p, "Org");
    assert_eq!(prop(org, "name").triples, 2);
    assert_eq!(prop(org, "founded").objects.literals[0].triples, 1);
}

#[test]
fn profile_budgets() {
    let s = store();
    let opts = ProfileOptions {
        schema: SchemaOptions {
            max_entries: 2,
            ..Default::default()
        },
        classes: Vec::new(),
    };
    assert!(matches!(
        profiles(&s.snapshot(), &opts),
        Err(SchemaError::TooManyEntries { .. })
    ));
    let opts = ProfileOptions {
        schema: SchemaOptions {
            deadline: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
            ..Default::default()
        },
        classes: Vec::new(),
    };
    assert!(matches!(
        profiles(&s.snapshot(), &opts),
        Err(SchemaError::Timeout { .. })
    ));
}

const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema#";
