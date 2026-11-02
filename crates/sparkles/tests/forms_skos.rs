//! SKOS-shaped forms: the fixture a thesaurus editor builds on. A concept with preferred
//! labels in two languages, broader and narrower links, a SKOS-XL label as a nested
//! blank node, a collection whose members are concepts or collections (`sh:or`) and an
//! ordered collection whose `skos:memberList` is an `rdf:List` with a member shape.
#![cfg(feature = "shacl")]

use oxrdf::{BlankNode, Literal, NamedNode, Term};
use sparkles::Dataset;
use sparkles::forms::{
    FieldChange, FormChange, FormRequest, FormSaved, FormSettings, NodeChange, ShapesSource,
};
use std::collections::{BTreeMap, BTreeSet};

const SKOS: &str = "http://www.w3.org/2004/02/skos/core#";
const XL: &str = "http://www.w3.org/2008/05/skos-xl#";
const EX: &str = "http://ex/";

const SHAPES: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> . @prefix skosxl: <http://www.w3.org/2008/05/skos-xl#> .
ex:ConceptShape a sh:NodeShape ; sh:targetClass skos:Concept ;
  sh:property [ sh:path skos:prefLabel ; sh:name "Preferred label" ; sh:datatype rdf:langString ;
                sh:uniqueLang true ; sh:minCount 1 ; sh:order 1 ] ;
  sh:property [ sh:path skos:broader ; sh:name "Broader" ; sh:class skos:Concept ; sh:order 2 ] ;
  sh:property [ sh:path [ sh:inversePath skos:broader ] ; sh:name "Narrower" ; sh:order 3 ] ;
  sh:property [ sh:path skos:inScheme ; sh:class skos:ConceptScheme ; sh:minCount 1 ] ;
  sh:property [ sh:path skosxl:prefLabel ; sh:name "XL label" ; sh:node ex:XLabel ;
                sh:nodeKind sh:BlankNode ] .
ex:XLabel sh:property [ sh:path skosxl:literalForm ; sh:datatype rdf:langString ;
                        sh:minCount 1 ; sh:maxCount 1 ] .
ex:CollectionShape a sh:NodeShape ; sh:targetClass skos:Collection ;
  sh:property [ sh:path skos:member ; sh:name "Members" ;
                sh:or ( [ sh:class skos:Concept ; sh:name "Concept" ]
                        [ sh:class skos:Collection ; sh:name "Collection" ] ) ] .
ex:OrderedShape a sh:NodeShape ; sh:targetClass skos:OrderedCollection ;
  sh:property [ sh:path skos:memberList ; sh:name "Ordered members" ; sh:minCount 1 ; sh:maxCount 1 ;
                sh:memberShape [ sh:class skos:Concept ] ] .
"#;

const DATA: &str = r#"
PREFIX ex: <http://ex/> PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
INSERT DATA {
  ex:scheme a skos:ConceptScheme .
  ex:animals a skos:Concept ; skos:prefLabel "Animals"@en , "Animaux"@fr ; skos:inScheme ex:scheme .
  ex:mammals a skos:Concept ; skos:prefLabel "Mammals"@en ; skos:broader ex:animals ; skos:inScheme ex:scheme .
  ex:birds a skos:Concept ; skos:prefLabel "Birds"@en ; skos:inScheme ex:scheme .
  ex:pets a skos:Collection ; skos:member ex:mammals .
  ex:ranked a skos:OrderedCollection ; skos:memberList ( ex:mammals ex:birds ) .
  ex:animals skos:note "kept" .
}"#;

fn skos(l: &str) -> String {
    format!("{SKOS}{l}")
}
fn ex(l: &str) -> String {
    format!("{EX}{l}")
}
fn iri(s: &str) -> Term {
    NamedNode::new_unchecked(s).into()
}
fn lang(v: &str, l: &str) -> Term {
    Literal::new_language_tagged_literal_unchecked(v, l).into()
}
fn id(s: &str) -> String {
    format!("<{s}>")
}

fn settings() -> FormSettings {
    FormSettings {
        shapes: ShapesSource::Graphs,
        graphs: vec!["urn:shapes".into()],
        ..Default::default()
    }
}

fn dataset() -> Dataset {
    let ds = Dataset::memory();
    ds.load_str_into(SHAPES, sparkles::io::RdfFormat::Turtle, "urn:shapes")
        .unwrap();
    ds.update(DATA).unwrap();
    ds
}

fn quads(ds: &Dataset) -> BTreeSet<String> {
    let mut out = Vec::new();
    ds.dump(&mut out, sparkles::io::RdfFormat::NQuads).unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn save(ds: &Dataset, c: &FormChange) {
    match ds.forms().save(c, &Default::default()).unwrap() {
        FormSaved::Saved(_) => {}
        FormSaved::Conflict(c) => panic!("conflict {c:?}"),
    }
}

#[test]
fn a_concept_form_lists_skos_fields_and_saves_them_atomically() {
    let ds = dataset();
    let req = FormRequest::new(ex("animals"), settings());
    let m = ds.forms().nested_model(&req, &Default::default()).unwrap();
    let root = &m.root;
    let field = |path: &str| root.fields.iter().find(|f| f.id == path).unwrap();
    let pref = field(&id(&skos("prefLabel")));
    assert_eq!(pref.widget, "text-lang");
    assert_eq!(pref.values.len(), 2);
    assert_eq!(field(&id(&skos("broader"))).widget, "entity");
    let narrower = field(&format!("^<{}>", skos("broader")));
    assert_eq!(narrower.values[0].term, iri(&ex("mammals")));
    assert_eq!(field(&id(&format!("{XL}prefLabel"))).widget, "nested");
    assert_eq!(root.fields[0].label, "Preferred label");

    let before = quads(&ds);
    // one save: a German label, a new SKOS-XL label child and a narrower concept
    let c = FormChange {
        focus: ex("animals"),
        changes: vec![
            FieldChange {
                field: id(&skos("prefLabel")),
                add: vec![lang("Tiere", "de")],
                ..Default::default()
            },
            FieldChange {
                field: id(&format!("{XL}prefLabel")),
                add: vec![Term::BlankNode(BlankNode::new_unchecked("xl"))],
                ..Default::default()
            },
            FieldChange {
                field: format!("^<{}>", skos("broader")),
                add: vec![iri(&ex("birds"))],
                ..Default::default()
            },
        ],
        expect: BTreeMap::from([
            (
                id(&skos("prefLabel")),
                vec![lang("Animals", "en"), lang("Animaux", "fr")],
            ),
            (id(&format!("{XL}prefLabel")), vec![]),
            (format!("^<{}>", skos("broader")), vec![iri(&ex("mammals"))]),
        ]),
        nested: vec![NodeChange {
            node: BlankNode::new_unchecked("xl"),
            parent: None,
            field: id(&format!("{XL}prefLabel")),
            changes: vec![FieldChange {
                field: id(&format!("{XL}literalForm")),
                add: vec![lang("Animals", "en")],
                ..Default::default()
            }],
            expect: BTreeMap::new(),
        }],
        settings: settings(),
        message: Some("labels and narrower".into()),
        ..Default::default()
    };
    let check = ds.forms().check(&c, &Default::default()).unwrap();
    assert_eq!(check.conforms, Some(true), "{:?}", check.results);
    save(&ds, &c);
    let after = quads(&ds);
    assert!(before.is_subset(&after), "unrelated quads stay");
    let added: Vec<_> = after.difference(&before).collect();
    assert_eq!(added.len(), 4, "{added:?}");
    assert!(after.contains(&format!("<{EX}birds> <{SKOS}broader> <{EX}animals> .")));

    // a second label in English breaks sh:uniqueLang, which the check reports on the field
    let c = FormChange {
        focus: ex("animals"),
        changes: vec![FieldChange {
            field: id(&skos("prefLabel")),
            add: vec![lang("Beasts", "en")],
            ..Default::default()
        }],
        expect: BTreeMap::from([(
            id(&skos("prefLabel")),
            vec![
                lang("Animals", "en"),
                lang("Animaux", "fr"),
                lang("Tiere", "de"),
            ],
        )]),
        settings: settings(),
        ..Default::default()
    };
    let check = ds.forms().check(&c, &Default::default()).unwrap();
    assert_eq!(check.conforms, Some(false));
    assert!(
        check
            .results
            .unwrap()
            .iter()
            .any(|r| r["field"] == id(&skos("prefLabel")))
    );
}

#[test]
fn collections_offer_member_alternatives_and_ordered_member_lists() {
    let ds = dataset();
    let m = ds
        .forms()
        .model(
            &FormRequest::new(ex("pets"), settings()),
            &Default::default(),
        )
        .unwrap();
    let members = serde_json::to_value(&m.fields[0]).unwrap();
    assert_eq!(members["constraints"]["or"]["kind"], "or");
    let options = members["constraints"]["or"]["options"].as_array().unwrap();
    assert_eq!(options.len(), 2);
    assert_eq!(options[0]["label"], "Concept");
    assert_eq!(options[1]["widget"], "entity");
    assert_eq!(
        options[1]["constraints"]["class"][0],
        format!("{SKOS}Collection")
    );

    // a member that is neither a concept nor a collection fails the check
    let bad = FormChange {
        focus: ex("pets"),
        changes: vec![FieldChange {
            field: id(&skos("member")),
            add: vec![iri(&ex("scheme"))],
            ..Default::default()
        }],
        expect: BTreeMap::from([(id(&skos("member")), vec![iri(&ex("mammals"))])]),
        settings: settings(),
        ..Default::default()
    };
    let check = ds.forms().check(&bad, &Default::default()).unwrap();
    assert_eq!(check.conforms, Some(false));
    let mut good = bad.clone();
    good.changes[0].add = vec![iri(&ex("ranked"))];
    ds.update(&format!(
        "PREFIX skos: <{SKOS}> INSERT DATA {{ <{EX}ranked> a skos:Collection }}"
    ))
    .unwrap();
    let check = ds.forms().check(&good, &Default::default()).unwrap();
    assert_eq!(check.conforms, Some(true), "{:?}", check.results);

    let ranked = ds
        .forms()
        .model(
            &FormRequest::new(ex("ranked"), settings()),
            &Default::default(),
        )
        .unwrap();
    let list = ranked
        .fields
        .iter()
        .find(|f| f.id == id(&skos("memberList")))
        .unwrap();
    assert_eq!(list.widget, "list");
    let items: Vec<Term> = list.values[0]
        .items
        .as_ref()
        .unwrap()
        .iter()
        .map(|v| v.term.clone())
        .collect();
    assert_eq!(items, vec![iri(&ex("mammals")), iri(&ex("birds"))]);
    let before = quads(&ds);
    let c = FormChange {
        focus: ex("ranked"),
        changes: vec![FieldChange {
            field: id(&skos("memberList")),
            list: Some(vec![
                iri(&ex("birds")),
                iri(&ex("animals")),
                iri(&ex("mammals")),
            ]),
            ..Default::default()
        }],
        expect: BTreeMap::from([(id(&skos("memberList")), items)]),
        settings: settings(),
        ..Default::default()
    };
    let check = ds.forms().check(&c, &Default::default()).unwrap();
    assert_eq!(check.conforms, Some(true), "{:?}", check.results);
    save(&ds, &c);
    let after = quads(&ds);
    // the edge and two cells go, the edge and three cells come
    assert_eq!(before.difference(&after).count(), 5);
    assert_eq!(after.difference(&before).count(), 7);
    let unrelated = |s: &BTreeSet<String>| {
        s.iter()
            .filter(|l| !l.contains("rdf-syntax-ns#first") && !l.contains("rdf-syntax-ns#rest"))
            .filter(|l| !l.contains("memberList"))
            .cloned()
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(unrelated(&before), unrelated(&after));
    // a list member outside the member shape fails the check
    let mut c = c;
    c.expect.insert(
        id(&skos("memberList")),
        vec![iri(&ex("birds")), iri(&ex("animals")), iri(&ex("mammals"))],
    );
    c.changes[0].list = Some(vec![iri(&ex("scheme"))]);
    let check = ds.forms().check(&c, &Default::default()).unwrap();
    assert_eq!(check.conforms, Some(false));
}
