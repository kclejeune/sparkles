//! Schema diffs ([`compare`]).

use super::compare::FieldChange;
use super::*;
use crate::sparql::QueryOptions;
use crate::store::{Store, StoreOptions};

fn ex(l: &str) -> String {
    format!("http://ex.org/{l}")
}

fn write(s: &Store, u: &str) {
    let text = format!(
        "PREFIX ex: <http://ex.org/>
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         {u}"
    );
    crate::sparql::update::update(s, &text, &QueryOptions::default()).unwrap();
}

fn paths(c: &[FieldChange]) -> Vec<&str> {
    c.iter().map(|x| x.path()).collect()
}

#[test]
fn diff_of_two_states() {
    let s = Store::in_memory(StoreOptions::default());
    write(
        &s,
        "INSERT DATA { ex:a a ex:P ; ex:p 1 ; rdfs:label \"a\" . ex:P rdfs:subClassOf ex:Q ; rdfs:label \"P\"@en }",
    );
    let before = discover(&s.snapshot(), &SchemaOptions::default()).unwrap();
    write(
        &s,
        "INSERT DATA { ex:b a ex:P, ex:R ; ex:p \"x\"@en ; ex:q ex:a . ex:P rdfs:label \"Pe\"@de }
         ; DELETE DATA { ex:P rdfs:subClassOf ex:Q }",
    );
    let after = discover(&s.snapshot(), &SchemaOptions::default()).unwrap();
    let d = compare(&before, &after);
    assert_eq!(d.from.commit + 1, d.to.commit);
    assert_eq!(
        d.classes
            .added
            .iter()
            .map(|c| c.iri.as_str())
            .collect::<Vec<_>>(),
        [ex("R")]
    );
    assert_eq!(
        d.classes
            .removed
            .iter()
            .map(|c| c.iri.as_str())
            .collect::<Vec<_>>(),
        [ex("Q")]
    );
    let p = &d.classes.changed[0];
    assert_eq!(p.iri, ex("P"));
    assert_eq!(
        paths(&p.changes),
        [
            "declared.labels",
            "declared.superClasses",
            "observed.instances"
        ]
    );
    assert_eq!(
        p.changes[1],
        FieldChange::Members {
            path: "declared.superClasses".into(),
            added: vec![],
            removed: vec![serde_json::json!(ex("Q"))],
        }
    );
    assert_eq!(
        p.changes[2],
        FieldChange::Value {
            path: "observed.instances".into(),
            from: 1.into(),
            to: 2.into(),
        }
    );
    assert_eq!(
        d.predicates
            .added
            .iter()
            .map(|c| c.iri.as_str())
            .collect::<Vec<_>>(),
        [ex("q")]
    );
    let pp = d
        .predicates
        .changed
        .iter()
        .find(|c| c.iri == ex("p"))
        .unwrap();
    let lang =
        "observed.objects.literals[datatype=http://www.w3.org/1999/02/22-rdf-syntax-ns#langString]";
    assert!(paths(&pp.changes).contains(&lang), "{:?}", pp.changes);
    assert!(paths(&pp.changes).contains(&"observed.triples"));
    assert!(
        paths(&d.report).contains(&"totals.triples"),
        "{:?}",
        d.report
    );
    assert!(paths(&d.report).contains(&"hierarchy.roots"));
    let text = diff_text(&d);
    assert!(
        text.contains("+ class http://ex.org/R (instances 1)"),
        "{text}"
    );
    assert!(
        text.contains("- class http://ex.org/Q (instances 0)"),
        "{text}"
    );
    assert!(
        text.contains("~ class http://ex.org/P: declared.labels +"),
        "{text}"
    );
    // a report against itself
    let same = compare(&after, &after);
    assert!(same.is_empty());
    assert!(diff_text(&same).ends_with("no changes\n"));
}

#[test]
fn languages_and_groups_by_key() {
    let s = Store::in_memory(StoreOptions::default());
    write(&s, "INSERT DATA { ex:a ex:p \"x\"@en, \"y\"@en, 1 }");
    let before = discover(&s.snapshot(), &SchemaOptions::default()).unwrap();
    write(
        &s,
        "INSERT DATA { ex:a ex:p \"z\"@fr } ; DELETE DATA { ex:a ex:p \"y\"@en }",
    );
    let after = discover(&s.snapshot(), &SchemaOptions::default()).unwrap();
    let d = compare(&before, &after);
    let p = &d.predicates.changed[0];
    let base =
        "observed.objects.literals[datatype=http://www.w3.org/1999/02/22-rdf-syntax-ns#langString]";
    assert_eq!(
        paths(&p.changes),
        [
            format!("{base}.languages[lang=en].triples").as_str(),
            format!("{base}.languages[lang=fr]").as_str(),
        ]
    );
}
