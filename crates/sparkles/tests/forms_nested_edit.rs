//! Atomic nested and list changes through forms (spec U02 §2.3.1, Phase 3): new children,
//! edits of pooled children, removal with their quads, `rdf:List` values, conflicts and
//! round trips that leave unrelated quads alone.
#![cfg(feature = "shacl")]

use oxrdf::{BlankNode, Literal, NamedNode, Term};
use sparkles::Dataset;
use sparkles::access::{GraphAccess, GraphRule, Graphs};
use sparkles::forms::{
    FieldChange, FormChange, FormRequest, FormSaved, FormSettings, NodeChange, ShapesSource,
};
use sparkles::sparql::QueryOptions;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const EX: &str = "http://ex/";
const PREFIX: &str = "@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex/> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> . ";
const SHAPES: &str = "ex:P sh:targetClass ex:Person ;
      sh:property [ sh:path ex:name ; sh:maxCount 1 ] ;
      sh:property [ sh:path ex:address ; sh:node ex:Address ; sh:nodeKind sh:BlankNode ] ;
      sh:property [ sh:path ex:tags ; sh:memberShape [ sh:nodeKind sh:IRI ] ; sh:maxListLength 3 ] .
    ex:Address sh:property [ sh:path ex:street ; sh:datatype xsd:string ; sh:minCount 1 ] ;
      sh:property [ sh:path ex:geo ; sh:node ex:Geo ] .
    ex:Geo sh:property [ sh:path ex:lat ] .";

fn ex(local: &str) -> String {
    format!("{EX}{local}")
}
fn iri(local: &str) -> Term {
    NamedNode::new_unchecked(ex(local)).into()
}
fn lit(s: &str) -> Term {
    Literal::new_simple_literal(s).into()
}
fn id(local: &str) -> String {
    format!("<{}>", ex(local))
}
fn bn(label: &str) -> BlankNode {
    BlankNode::new_unchecked(label)
}

fn settings() -> FormSettings {
    FormSettings {
        shapes: ShapesSource::Graphs,
        graphs: vec!["urn:shapes".into()],
        ..Default::default()
    }
}

fn dataset(data: &str) -> Dataset {
    let ds = Dataset::memory();
    ds.load_str_into(
        &format!("{PREFIX}{SHAPES}"),
        sparkles::io::RdfFormat::Turtle,
        "urn:shapes",
    )
    .unwrap();
    ds.update(&format!(
        "PREFIX ex: <{EX}> INSERT DATA {{ ex:alice a ex:Person ; ex:name \"Alice\" . ex:bob ex:name \"Bob\" . {data} }}"
    ))
    .unwrap();
    ds
}

/// The dataset's quads as N-Quads lines.
fn quads(ds: &Dataset) -> BTreeSet<String> {
    let mut out = Vec::new();
    ds.dump(&mut out, sparkles::io::RdfFormat::NQuads).unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn change(
    changes: Vec<FieldChange>,
    expect: Vec<(String, Vec<Term>)>,
    nested: Vec<NodeChange>,
) -> FormChange {
    FormChange {
        focus: ex("alice"),
        changes,
        expect: expect.into_iter().collect(),
        message: Some("nested".into()),
        settings: settings(),
        nested,
        ..Default::default()
    }
}

fn field(field: String, remove: Vec<Term>, add: Vec<Term>) -> FieldChange {
    FieldChange {
        field,
        remove,
        add,
        list: None,
    }
}

fn saved(r: FormSaved) -> sparkles::commit::Receipt {
    match r {
        FormSaved::Saved(r) => *r,
        FormSaved::Conflict(c) => panic!("unexpected conflict {c:?}"),
    }
}

/// The address child of alice in the nested model, by its stored term.
fn address(ds: &Dataset) -> Term {
    let req = FormRequest::new(ex("alice"), settings());
    let m = ds.forms().nested_model(&req, &Default::default()).unwrap();
    m.nested_nodes
        .iter()
        .find(|n| {
            n.anchors
                .iter()
                .any(|a| a.parent == "root" && a.field == id("address"))
        })
        .expect("an address child")
        .term
        .clone()
}

fn blank(t: &Term) -> BlankNode {
    match t {
        Term::BlankNode(b) => b.clone(),
        t => panic!("not a blank node: {t}"),
    }
}

#[test]
fn a_new_child_and_its_fields_commit_in_one_save() {
    let ds = dataset("");
    let before = quads(&ds);
    let c = change(
        vec![field(
            id("address"),
            vec![],
            vec![Term::BlankNode(bn("a1"))],
        )],
        vec![(id("address"), vec![])],
        vec![NodeChange {
            node: bn("a1"),
            parent: None,
            field: id("address"),
            changes: vec![field(id("street"), vec![], vec![lit("Main")])],
            expect: BTreeMap::new(),
        }],
    );
    let check = ds.forms().check(&c, &Default::default()).unwrap();
    assert!(check.conflicts.is_empty());
    assert_eq!(check.conforms, Some(true), "{:?}", check.results);
    assert_eq!(check.preview.commit.as_ref().unwrap().inserted, 2);
    // the check is a dry run
    assert_eq!(quads(&ds), before);
    let receipt = saved(ds.forms().save(&c, &Default::default()).unwrap());
    assert_eq!(receipt.commit.inserted, 2);
    let after = quads(&ds);
    assert!(before.is_subset(&after), "unrelated quads stay");
    let added: Vec<_> = after.difference(&before).collect();
    assert_eq!(added.len(), 2, "{added:?}");
    let child = address(&ds);
    assert!(
        added
            .iter()
            .any(|l| l.starts_with(&format!("{child} <{EX}street> \"Main\"")))
    );
    // the label the client chose names nothing stored
    assert!(!after.iter().any(|l| l.contains("_:a1")));
}

#[test]
fn a_new_child_label_that_spells_a_stored_node_is_still_a_new_node() {
    let ds = dataset("ex:bob ex:address _:other . _:other ex:street \"Elsewhere\" .");
    // the stored label of bob's child
    let other = {
        let req = FormRequest::new(ex("bob"), settings());
        let _ = req;
        let mut out = Vec::new();
        ds.dump(&mut out, sparkles::io::RdfFormat::NQuads).unwrap();
        let text = String::from_utf8(out).unwrap();
        let line = text
            .lines()
            .find(|l| l.contains("Elsewhere"))
            .unwrap()
            .to_string();
        line.split(' ')
            .next()
            .unwrap()
            .trim_start_matches("_:")
            .to_string()
    };
    let before = quads(&ds);
    let c = change(
        vec![field(
            id("address"),
            vec![],
            vec![Term::BlankNode(bn(&other))],
        )],
        vec![(id("address"), vec![])],
        vec![NodeChange {
            node: bn(&other),
            parent: None,
            field: id("address"),
            changes: vec![field(id("street"), vec![], vec![lit("Main")])],
            expect: BTreeMap::new(),
        }],
    );
    saved(ds.forms().save(&c, &Default::default()).unwrap());
    let after = quads(&ds);
    assert!(before.is_subset(&after));
    // alice's new child is another node than bob's
    assert!(!after.contains(&format!("<{EX}alice> <{EX}address> _:{other} .")));
    assert!(!after.contains(&format!("_:{other} <{EX}street> \"Main\" .")));
}

#[test]
fn a_pooled_child_field_is_edited_with_its_expectation() {
    let ds = dataset("ex:alice ex:address [ ex:street \"Old\" ] .");
    let child = address(&ds);
    let before = quads(&ds);
    let c = change(
        vec![],
        vec![],
        vec![NodeChange {
            node: blank(&child),
            parent: None,
            field: id("address"),
            changes: vec![field(id("street"), vec![lit("Old")], vec![lit("New")])],
            expect: BTreeMap::from([(id("street"), vec![lit("Old")])]),
        }],
    );
    saved(ds.forms().save(&c, &Default::default()).unwrap());
    let after = quads(&ds);
    let removed: Vec<_> = before.difference(&after).collect();
    let added: Vec<_> = after.difference(&before).collect();
    assert_eq!(removed, vec![&format!("{child} <{EX}street> \"Old\" .")]);
    assert_eq!(added, vec![&format!("{child} <{EX}street> \"New\" .")]);

    // the same change again: the stored value is no longer the one the form loaded
    match ds.forms().save(&c, &Default::default()).unwrap() {
        FormSaved::Conflict(c) => {
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].id, id("street"));
            assert_eq!(c[0].node.as_ref(), Some(&child));
            assert_eq!(c[0].now, vec![lit("New")]);
        }
        FormSaved::Saved(_) => panic!("a stale expectation must conflict"),
    }
    // a changed child field needs its expectation
    let mut missing = c.clone();
    missing.nested[0].expect.clear();
    let e = ds.forms().save(&missing, &Default::default()).unwrap_err();
    assert!(e.to_string().contains("expect"), "{e}");
}

#[test]
fn removing_a_child_deletes_its_quads_lists_and_grandchildren_only() {
    let ds = dataset(
        "ex:alice ex:address _:a . _:a ex:street \"Main\" ; ex:geo _:g ; ex:tags (ex:x ex:y) . _:g ex:lat 1 .
         ex:bob ex:address _:b . _:b ex:street \"Kept\" .",
    );
    let child = address(&ds);
    let before = quads(&ds);
    let c = change(
        vec![field(id("address"), vec![child.clone()], vec![])],
        vec![(id("address"), vec![child.clone()])],
        vec![],
    );
    let receipt = saved(ds.forms().save(&c, &Default::default()).unwrap());
    let after = quads(&ds);
    assert!(after.is_subset(&before));
    let removed: BTreeSet<_> = before.difference(&after).cloned().collect();
    // the edge, street, geo, the grandchild's lat, tags and the two list cells' four quads
    assert_eq!(removed.len(), 1 + 3 + 1 + 4, "{removed:?}");
    assert_eq!(receipt.commit.deleted as usize, removed.len());
    assert!(after.iter().any(|l| l.contains("\"Kept\"")));
    assert!(after.iter().any(|l| l.contains("\"Alice\"")));
}

#[test]
fn removing_a_shared_child_keeps_it_for_its_other_referrer() {
    let ds = dataset("ex:alice ex:address _:a . _:a ex:street \"Main\" . ex:bob ex:address _:a .");
    let child = address(&ds);
    let before = quads(&ds);
    let c = change(
        vec![field(id("address"), vec![child.clone()], vec![])],
        vec![(id("address"), vec![child.clone()])],
        vec![],
    );
    saved(ds.forms().save(&c, &Default::default()).unwrap());
    let removed: Vec<_> = before.difference(&quads(&ds)).cloned().collect();
    assert_eq!(
        removed,
        vec![format!("<{EX}alice> <{EX}address> {child} .")]
    );
}

#[test]
fn a_child_removed_meanwhile_answers_a_conflict_on_its_parent_field() {
    let ds = dataset("ex:alice ex:address _:a . _:a ex:street \"Old\" .");
    let child = address(&ds);
    let c = change(
        vec![],
        vec![],
        vec![NodeChange {
            node: blank(&child),
            parent: None,
            field: id("address"),
            changes: vec![field(id("street"), vec![lit("Old")], vec![lit("New")])],
            expect: BTreeMap::from([(id("street"), vec![lit("Old")])]),
        }],
    );
    ds.update(&format!(
        "PREFIX ex: <{EX}> DELETE WHERE {{ ex:alice ex:address ?a . ?a ?p ?o }}"
    ))
    .unwrap();
    let before = quads(&ds);
    match ds.forms().save(&c, &Default::default()).unwrap() {
        FormSaved::Conflict(c) => {
            assert_eq!(c[0].id, id("address"));
            assert!(c[0].node.is_none());
            assert!(c[0].now.is_empty());
        }
        FormSaved::Saved(_) => panic!("the child is gone"),
    }
    assert_eq!(quads(&ds), before);
    // a check reports the same conflict
    let check = ds.forms().check(&c, &Default::default()).unwrap();
    assert_eq!(check.conflicts[0].id, id("address"));
    // a label that never hung from the field grants nothing either
    let mut made_up = c.clone();
    made_up.nested[0].node = bn("b7777");
    assert!(matches!(
        ds.forms().save(&made_up, &Default::default()).unwrap(),
        FormSaved::Conflict(_)
    ));
}

#[test]
fn a_child_with_a_quad_the_caller_cannot_write_is_not_removed() {
    let ds = dataset(
        "GRAPH <urn:people> { ex:alice ex:address _:a . _:a ex:street \"Main\" } GRAPH <urn:locked> { _:a ex:geo ex:somewhere }",
    );
    let opts = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::All,
            Graphs::Only(GraphRule::new(["urn:people"], &[])),
        ))),
        ..Default::default()
    };
    let child = address(&ds);
    let before = quads(&ds);
    let c = change(
        vec![field(id("address"), vec![child.clone()], vec![])],
        vec![(id("address"), vec![child.clone()])],
        vec![],
    );
    let e = ds.forms().save(&c, &opts).unwrap_err();
    assert!(matches!(e, sparkles::Error::NotPermitted(_)), "{e}");
    assert_eq!(quads(&ds), before);
}

#[test]
fn a_list_field_is_read_and_replaced_as_a_whole() {
    let ds = dataset("ex:alice ex:tags (ex:x ex:y) .");
    let req = FormRequest::new(ex("alice"), settings());
    let m = ds.forms().model(&req, &Default::default()).unwrap();
    let tags = m.fields.iter().find(|f| f.id == id("tags")).unwrap();
    assert_eq!(tags.widget, "list");
    assert!(tags.editable, "{:?}", tags.reason);
    let items: Vec<Term> = tags.values[0]
        .items
        .as_ref()
        .unwrap()
        .iter()
        .map(|v| v.term.clone())
        .collect();
    assert_eq!(items, vec![iri("x"), iri("y")]);
    let j = serde_json::to_value(tags).unwrap();
    assert_eq!(j["constraints"]["list"]["maxLength"], 3);
    assert_eq!(j["constraints"]["list"]["memberWidget"], "entity");
    assert_eq!(
        j["constraints"]["list"]["member"]["nodeKind"],
        "http://www.w3.org/ns/shacl#IRI"
    );

    let before = quads(&ds);
    let mut c = change(
        vec![FieldChange {
            field: id("tags"),
            list: Some(vec![iri("y"), iri("z"), iri("x")]),
            ..Default::default()
        }],
        vec![(id("tags"), items.clone())],
        vec![],
    );
    let check = ds.forms().check(&c, &Default::default()).unwrap();
    assert_eq!(check.conforms, Some(true), "{:?}", check.results);
    saved(ds.forms().save(&c, &Default::default()).unwrap());
    let m = ds.forms().model(&req, &Default::default()).unwrap();
    let tags = m.fields.iter().find(|f| f.id == id("tags")).unwrap();
    let now: Vec<Term> = tags.values[0]
        .items
        .as_ref()
        .unwrap()
        .iter()
        .map(|v| v.term.clone())
        .collect();
    assert_eq!(now, vec![iri("y"), iri("z"), iri("x")]);
    // the old cells are gone: one edge and three cells
    let after = quads(&ds);
    assert_eq!(after.len(), before.len() - 5 + 7);
    assert!(after.iter().any(|l| l.contains("\"Alice\"")));

    // an edit of one cell in place conflicts with a form that loaded the old members
    ds.update(&format!(
        "PREFIX ex: <{EX}> PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
         DELETE {{ ?c rdf:first ex:z }} INSERT {{ ?c rdf:first ex:w }} WHERE {{ ?c rdf:first ex:z }}"
    ))
    .unwrap();
    c.expect.insert(id("tags"), now);
    c.changes[0].list = Some(vec![]);
    match ds.forms().save(&c, &Default::default()).unwrap() {
        FormSaved::Conflict(c) => {
            assert_eq!(c[0].id, id("tags"));
            assert_eq!(c[0].now, vec![iri("y"), iri("w"), iri("x")]);
        }
        FormSaved::Saved(_) => panic!("a changed member must conflict"),
    }
    // an empty list is rdf:nil
    c.expect
        .insert(id("tags"), vec![iri("y"), iri("w"), iri("x")]);
    saved(ds.forms().save(&c, &Default::default()).unwrap());
    assert!(quads(&ds).contains(&format!(
        "<{EX}alice> <{EX}tags> <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> ."
    )));
    // add and remove do not change a list field
    let wrong = change(
        vec![field(id("tags"), vec![], vec![iri("q")])],
        vec![(id("tags"), vec![])],
        vec![],
    );
    let e = ds.forms().save(&wrong, &Default::default()).unwrap_err();
    assert!(e.to_string().contains("is a list"), "{e}");
}

#[test]
fn a_malformed_list_is_read_only() {
    let ds = dataset(
        "ex:alice ex:tags _:c . _:c <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> ex:x , ex:y ; <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> .",
    );
    let req = FormRequest::new(ex("alice"), settings());
    let m = ds.forms().model(&req, &Default::default()).unwrap();
    let tags = m.fields.iter().find(|f| f.id == id("tags")).unwrap();
    assert!(!tags.editable);
    assert_eq!(tags.reason, Some("list"));
}

#[test]
fn nested_entries_must_reach_the_root_through_declared_fields() {
    let ds = dataset("ex:alice ex:address _:a . _:a ex:street \"Main\" .");
    // a blank node in add without its entry
    let c = change(
        vec![field(id("address"), vec![], vec![Term::BlankNode(bn("x"))])],
        vec![(id("address"), vec![])],
        vec![],
    );
    let e = ds.forms().save(&c, &Default::default()).unwrap_err();
    assert!(e.to_string().contains("new nested node"), "{e}");
    // an entry under a field without sh:node
    let c = change(
        vec![field(id("name"), vec![], vec![Term::BlankNode(bn("x"))])],
        vec![(id("name"), vec![lit("Alice")])],
        vec![NodeChange {
            node: bn("x"),
            parent: None,
            field: id("name"),
            changes: vec![],
            expect: BTreeMap::new(),
        }],
    );
    let e = ds.forms().save(&c, &Default::default()).unwrap_err();
    assert!(e.to_string().contains("sh:node"), "{e}");
    // an entry whose parent is not in the change
    let c = change(
        vec![],
        vec![],
        vec![NodeChange {
            node: bn("x"),
            parent: Some(bn("nowhere")),
            field: id("geo"),
            changes: vec![field(id("lat"), vec![], vec![lit("1")])],
            expect: BTreeMap::new(),
        }],
    );
    let e = ds.forms().save(&c, &Default::default()).unwrap_err();
    assert!(e.to_string().contains("neither the root"), "{e}");
}

#[test]
fn a_grandchild_is_created_under_a_pooled_child() {
    let ds = dataset("ex:alice ex:address _:a . _:a ex:street \"Main\" .");
    let child = address(&ds);
    let before = quads(&ds);
    let c = change(
        vec![],
        vec![],
        vec![
            NodeChange {
                node: blank(&child),
                parent: None,
                field: id("address"),
                changes: vec![field(id("geo"), vec![], vec![Term::BlankNode(bn("g"))])],
                expect: BTreeMap::from([(id("geo"), vec![])]),
            },
            NodeChange {
                node: bn("g"),
                parent: Some(blank(&child)),
                field: id("geo"),
                changes: vec![field(id("lat"), vec![], vec![lit("52")])],
                expect: BTreeMap::new(),
            },
        ],
    );
    let check = ds.forms().check(&c, &Default::default()).unwrap();
    assert_eq!(check.conforms, Some(true), "{:?}", check.results);
    saved(ds.forms().save(&c, &Default::default()).unwrap());
    let added: Vec<_> = quads(&ds).difference(&before).cloned().collect();
    assert_eq!(added.len(), 2, "{added:?}");
    assert!(
        added
            .iter()
            .any(|l| l.starts_with(&format!("{child} <{EX}geo> _:")))
    );
    assert!(
        added
            .iter()
            .any(|l| l.contains(&format!("<{EX}lat> \"52\"")))
    );
}

#[test]
fn a_check_maps_a_child_violation_to_the_child_field() {
    let ds = dataset("");
    // a new address without its required street
    let c = change(
        vec![field(
            id("address"),
            vec![],
            vec![Term::BlankNode(bn("a1"))],
        )],
        vec![(id("address"), vec![])],
        vec![NodeChange {
            node: bn("a1"),
            parent: None,
            field: id("address"),
            changes: vec![field(id("geo"), vec![], vec![iri("somewhere")])],
            expect: BTreeMap::new(),
        }],
    );
    let check = ds.forms().check(&c, &Default::default()).unwrap();
    assert_eq!(check.conforms, Some(false));
    let results = check.results.unwrap();
    assert!(
        results
            .iter()
            .any(|r| r["field"] == id("address") || r["field"] == id("street")),
        "{results:?}"
    );
}
