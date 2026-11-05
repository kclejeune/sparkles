#![cfg(feature = "ontology")]
//! An ontology file read without a store answers a classification and a check as the
//! same file loaded into an empty database does (roadmap item 649).

use serde_json::Value as J;
use sparkles::Dataset;
use sparkles::handles::ontology::classify::ClassifyRequest;
use sparkles::handles::ontology::file::OntologyFile;
use sparkles::handles::ontology::wire::{
    ClassificationView, ClassifyQuery, MAX_LIMIT, check_body, classification_body, classify_body,
    reports_body,
};
use sparkles::handles::ontology::{CheckRequest, OntologyRequest};
use sparkles::owl::profiles::Profile;
use sparkles::owl::read::Mode;
use sparkles_reasoner::inputs::ImportMode;
use std::path::Path;

const TURTLE: &str = r#"@prefix : <http://ex.org/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
:o a owl:Ontology ; owl:imports :z, :a ; owl:imports "not an IRI" ; rdfs:label "o" .
:partOf a owl:ObjectProperty , owl:TransitiveProperty .
:hasPart owl:inverseOf :partOf .
:note a owl:AnnotationProperty .
:Nucleus a owl:Class ; :note "kept" ; :other "streamed" ;
  rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :partOf ; owl:someValuesFrom :Cell ] .
:CellPart owl:equivalentClass [ a owl:Restriction ; owl:onProperty :partOf ; owl:someValuesFrom :Cell ] .
:Cell rdfs:subClassOf :Thing .
:Cell rdfs:subClassOf :Thing .
:Rock rdfs:subClassOf :Cell , [ owl:complementOf :Cell ] .
:Nucleus rdfs:subClassOf :Organelle .
[] a owl:Axiom ; owl:annotatedSource :Nucleus ; owl:annotatedProperty rdfs:subClassOf ;
   owl:annotatedTarget :Organelle ; rdfs:comment "annotated" .
:n1 a :Nucleus ; :partOf :c1 ; :size "3"^^xsd:integer .
:n2 a [ a owl:Restriction ; owl:onProperty :partOf ; owl:someValuesFrom :Cell ] .
:n3 a :Rock .
:n1 owl:sameAs :n4 .
[] a owl:Axiom ; owl:annotatedSource :c1 ; owl:annotatedProperty :partOf ;
   owl:annotatedTarget :c2 ; rdfs:comment "an annotated assertion" .
:c1 :partOf :c2 .
_:anon :partOf :c1 .
"#;

const RDFXML: &str = r#"<?xml version="1.0"?>
<rdf:RDF xmlns="http://ex.org/" xml:base="http://ex.org/"
     xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#"
     xmlns:owl="http://www.w3.org/2002/07/owl#">
  <owl:Ontology rdf:about="http://ex.org/o"><owl:imports rdf:resource="http://ex.org/b"/></owl:Ontology>
  <owl:ObjectProperty rdf:about="http://ex.org/partOf"/>
  <owl:Class rdf:about="http://ex.org/Nucleus">
    <rdfs:label xml:lang="en">nucleus</rdfs:label>
    <rdfs:subClassOf>
      <owl:Restriction>
        <owl:onProperty rdf:resource="http://ex.org/partOf"/>
        <owl:someValuesFrom rdf:nodeID="cell"/>
      </owl:Restriction>
    </rdfs:subClassOf>
  </owl:Class>
  <rdf:Description rdf:nodeID="cell">
    <rdf:type rdf:resource="http://www.w3.org/2002/07/owl#Class"/>
    <owl:intersectionOf rdf:parseType="Collection">
      <owl:Class rdf:about="http://ex.org/Cell"/>
      <owl:Class rdf:about="http://ex.org/Living"/>
    </owl:intersectionOf>
  </rdf:Description>
  <owl:Class rdf:about="http://ex.org/CellPart">
    <owl:equivalentClass>
      <owl:Restriction>
        <owl:onProperty rdf:resource="http://ex.org/partOf"/>
        <owl:someValuesFrom rdf:resource="http://ex.org/Cell"/>
      </owl:Restriction>
    </owl:equivalentClass>
  </owl:Class>
  <owl:NamedIndividual rdf:about="http://ex.org/n1"><rdf:type rdf:resource="http://ex.org/Nucleus"/></owl:NamedIndividual>
</rdf:RDF>
"#;

const OFN: &str = "Prefix(:=<http://ex.org/>)
Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)
Ontology(<http://ex.org/el>
  Import(<http://ex.org/z>)
  Import(<http://ex.org/a>)
  Annotation(rdfs:label \"el\")
  Declaration(Class(:Professor))
  Declaration(Class(:Course))
  Declaration(ObjectProperty(:teaches))
  Declaration(AnnotationProperty(:note))
  Declaration(Class(:Teacher))
  Declaration(Class(:Rock))
  Declaration(ObjectProperty(:taughtBy))
  Declaration(DataProperty(:hours))
  Declaration(NamedIndividual(:earth))
  Declaration(NamedIndividual(:ann))
  Declaration(NamedIndividual(:anne))
  Declaration(NamedIndividual(:bob))
  Declaration(NamedIndividual(:robert))
  Declaration(NamedIndividual(:bobby))
  Declaration(NamedIndividual(:logic))
  Declaration(NamedIndividual(:stone))
  Declaration(NamedIndividual(:pebble))
  TransitiveObjectProperty(:partOf)
  SubClassOf(:Professor ObjectSomeValuesFrom(:teaches :Course))
  EquivalentClasses(:Teacher ObjectSomeValuesFrom(:teaches :Course))
  SubClassOf(Annotation(rdfs:comment \"why\") :Rock :Course)
  DisjointClasses(:Rock :Course)
  SubClassOf(:Rock ObjectHasValue(:partOf :earth))
  SubClassOf(:Course DataHasValue(:hours \"3\"^^xsd:integer))
  InverseObjectProperties(:teaches :taughtBy)
  AnnotationAssertion(:note :Professor \"kept\")
  AnnotationAssertion(rdfs:label :Professor \"professor\")
  ClassAssertion(:Professor :ann)
  ClassAssertion(:Professor :ann)
  ClassAssertion(Annotation(rdfs:comment \"annotated\") :Rock :stone)
  ClassAssertion(ObjectSomeValuesFrom(:teaches :Course) :bob)
  ObjectPropertyAssertion(:teaches :ann :logic)
  DataPropertyAssertion(:hours :logic \"4\"^^xsd:integer)
  SameIndividual(:ann :anne)
  DifferentIndividuals(:ann :bob)
  DifferentIndividuals(:ann :bob :logic)
  NegativeObjectPropertyAssertion(:teaches :bob :logic)
  ObjectPropertyAssertion(ObjectInverseOf(:taughtBy) :logic :bob)
  SameIndividual(:bob :robert :bobby)
  SameIndividual(Annotation(rdfs:comment \"three\") :ann :anne :robert)
  EquivalentClasses(:Rock :Teacher :Course)
  ClassAssertion(:Rock :pebble)
)
";

fn store_answers(
    path: &Path,
    req: &OntologyRequest,
    c: &ClassifyRequest,
    check: &CheckRequest,
) -> (J, J) {
    let ds = Dataset::memory();
    let src = sparkles::io::Source::from_path(path, None).unwrap();
    ds.store().load(&[src]).unwrap();
    let name = path.display().to_string();
    let r = sparkles::handles::ontology::wire::renderer(&ds);
    let q = ClassifyQuery {
        about: c.about.clone(),
        nodes: true,
        limit: MAX_LIMIT,
        offset: 0,
        ..Default::default()
    };
    let cl = ds.ontology().classify(req, c, &Default::default()).unwrap();
    let mut a = classify_body(&name, &cl, req, &r, &q).unwrap();
    a["taxonomy"] = taxonomy_doc(&r, &cl.classification.taxonomy);
    let o = ds.ontology().open(req, &Default::default()).unwrap();
    let reports = ds
        .ontology()
        .check(req, check, &Default::default())
        .unwrap();
    let b = check_body(&name, &o, req, &r, &reports, check.assertions);
    (a, b)
}

fn file_answers(
    file: &OntologyFile,
    req: &OntologyRequest,
    c: &ClassifyRequest,
    check: &CheckRequest,
) -> (J, J) {
    let r = file.renderer();
    let q = ClassifyQuery {
        about: c.about.clone(),
        nodes: true,
        limit: MAX_LIMIT,
        offset: 0,
        ..Default::default()
    };
    let out = file.classify(c, &Default::default()).unwrap();
    let view = file.view();
    let mut a = classification_body(
        &file.name,
        &ClassificationView {
            read: &view,
            classification: &out.classification,
            excluded: &out.excluded,
            reused: false,
            written: None,
        },
        req,
        &r,
        &q,
    )
    .unwrap();
    a["taxonomy"] = taxonomy_doc(&r, &out.classification.taxonomy);
    let reports = file.check(check, &Default::default()).unwrap();
    let b = reports_body(
        &file.name,
        &file.view(),
        req,
        &r,
        &reports,
        check.assertions,
    );
    (a, b)
}

fn taxonomy_doc(r: &sparkles::owl::ofn::Renderer, t: &sparkles::owl::classify::Taxonomy) -> J {
    sparkles::owl::ofn::document(r, &sparkles::owl::Header::default(), t.axioms().iter()).into()
}

/// Comparable classification content, excluding timing and store-only publication identity.
fn timeless(mut j: J) -> J {
    // File inputs have no bound store/publication UUID; compare their actual
    // taxonomy, coverage and outcome rather than those deliberately distinct IDs.
    if let Some(object) = j.as_object_mut() {
        object.remove("classificationSource");
        object.remove("resultId");
    }
    for k in ["indexMs", "saturationMs", "taxonomyMs"] {
        if let Some(s) = j["stats"].as_object_mut() {
            s.remove(k);
        }
    }
    j
}

fn compare(name: &str, text: &str) {
    compare_or_refuse(name, text, false)
}

/// Compare the answers of the file and the store, or, with `refused`, check that the
/// file path leaves every read of the file to the store.
fn compare_or_refuse(name: &str, text: &str, refused: bool) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join(name);
    std::fs::write(&path, text).unwrap();
    let checks = [
        CheckRequest {
            profiles: vec![Profile::Dl, Profile::El, Profile::Ql, Profile::Rl],
            ..Default::default()
        },
        CheckRequest {
            profiles: vec![Profile::Dl, Profile::El],
            assertions: false,
            max_violations: 3,
        },
    ];
    let classifies = [
        ClassifyRequest::default(),
        ClassifyRequest {
            assertions: true,
            threads: 2,
            ..Default::default()
        },
        ClassifyRequest {
            about: Some("http://ex.org/Nucleus".into()),
            ..Default::default()
        },
    ];
    for imports in [ImportMode::Dataset, ImportMode::None] {
        for mode in [Mode::Lenient, Mode::Strict] {
            let req = OntologyRequest {
                imports,
                mode,
                ..Default::default()
            };
            let file = OntologyFile::read(&path, &req, 4).unwrap();
            if refused {
                assert!(file.is_none(), "{name} {mode:?} is read");
                continue;
            }
            let Some(file) = file else {
                assert!(
                    name.ends_with(".ofn") && mode == Mode::Strict,
                    "{name} {mode:?}"
                );
                continue;
            };
            for (c, check) in classifies.iter().zip(checks.iter().cycle()) {
                if c.about.is_some() && !name.ends_with(".ttl") {
                    continue;
                }
                let (sa, sb) = store_answers(&path, &req, c, check);
                let (fa, fb) = file_answers(&file, &req, c, check);
                assert_eq!(
                    timeless(fa),
                    timeless(sa),
                    "{name} {mode:?} {imports:?} {c:?}"
                );
                if fb != sb
                    && let Ok(dir) = std::env::var("ONTOLOGY_FILE_DUMP")
                {
                    std::fs::write(
                        format!("{dir}/file.json"),
                        serde_json::to_string_pretty(&fb).unwrap(),
                    )
                    .unwrap();
                    std::fs::write(
                        format!("{dir}/store.json"),
                        serde_json::to_string_pretty(&sb).unwrap(),
                    )
                    .unwrap();
                }
                assert_eq!(fb, sb, "{name} {mode:?} {imports:?} {check:?}");
            }
        }
    }
}

#[test]
fn a_turtle_file_answers_as_a_loaded_one() {
    compare("onto.ttl", TURTLE);
}

#[test]
fn an_rdf_xml_file_answers_as_a_loaded_one() {
    compare("onto.owl", RDFXML);
}

#[test]
fn a_functional_style_file_answers_as_a_loaded_one() {
    compare("onto.ofn", OFN);
}

#[test]
fn a_functional_style_file_reads_as_its_triples_do() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("nary.ofn");
    std::fs::write(
        &path,
        "Prefix(:=<http://ex.org/>)\nOntology(\n Declaration(Class(:A))\n Declaration(Class(:B))\n Declaration(Class(:C))\n Declaration(Class(:D))\n EquivalentClasses(:A :B :C)\n SubClassOf(:C :D)\n)\n",
    )
    .unwrap();
    let file = OntologyFile::read(&path, &OntologyRequest::default(), 2)
        .unwrap()
        .unwrap();
    let mut axioms: Vec<String> = file
        .ontology
        .axiom_set()
        .filter(|a| !matches!(a.kind, sparkles::owl::AxiomKind::Declaration(_)))
        .map(|a| file.renderer().axiom(a))
        .collect();
    axioms.sort();
    assert_eq!(
        axioms,
        [
            "EquivalentClasses(:A :B)",
            "EquivalentClasses(:B :C)",
            "SubClassOf(:C :D)"
        ],
        "{axioms:?}"
    );
    let out = file
        .classify(&ClassifyRequest::default(), &Default::default())
        .unwrap();
    let t = &out.classification.taxonomy;
    let c = |s: &str| sparkles::owl::Class::new(format!("http://ex.org/{s}"));
    assert!(t.is_subclass(&c("A"), &c("D")));
    assert_eq!(t.equivalents(&c("B")).len(), 3);
}

#[test]
fn what_the_store_reads_is_left_to_it() {
    let d = tempfile::tempdir().unwrap();
    let trig = d.path().join("onto.trig");
    std::fs::write(&trig, "<http://ex.org/g> { <http://ex.org/A> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex.org/B> }").unwrap();
    assert!(
        OntologyFile::read(&trig, &OntologyRequest::default(), 2)
            .unwrap()
            .is_none()
    );
    let bad = d.path().join("bad.ttl");
    std::fs::write(&bad, "<http://ex.org/A> <http://ex.org/p> .").unwrap();
    assert!(
        OntologyFile::read(&bad, &OntologyRequest::default(), 2)
            .unwrap()
            .is_none()
    );
    let ofn = d.path().join("onto.ofn");
    std::fs::write(&ofn, OFN).unwrap();
    let strict = OntologyRequest {
        mode: Mode::Strict,
        ..Default::default()
    };
    assert!(OntologyFile::read(&ofn, &strict, 2).unwrap().is_none());
    let named = OntologyRequest {
        graphs: vec![sparkles_reasoner::inputs::GraphRef::Named(
            "http://ex.org/g".into(),
        )],
        ..Default::default()
    };
    assert!(OntologyFile::read(&ofn, &named, 2).unwrap().is_none());
    // the store labels anonymous individuals in its own order
    let anonymous = d.path().join("anonymous.ofn");
    std::fs::write(&anonymous, OFN.replace(":pebble", "_:pebble")).unwrap();
    assert!(
        OntologyFile::read(&anonymous, &OntologyRequest::default(), 2)
            .unwrap()
            .is_none()
    );
}

const OFN_HEAD: &str = "Prefix(:=<http://ex.org/>)\nPrefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\nOntology(<http://ex.org/o>\n";

#[test]
fn an_equivalence_of_named_classes_splits_as_its_triples_do() {
    // the pairs keep the inconsistency of A and B, where a whole axiom with an operand
    // outside OWL 2 EL would be left out
    let text = format!(
        "{OFN_HEAD} Declaration(Class(:A))\n Declaration(Class(:B))\n Declaration(Class(:C))\n Declaration(NamedIndividual(:i))\n EquivalentClasses(:A :B :C)\n SubClassOf(:B owl:Nothing)\n ClassAssertion(:A :i)\n)\n"
    );
    compare("pairs.ofn", &text);
}

#[test]
fn what_the_triples_could_read_otherwise_is_left_to_the_store() {
    let declared = " Declaration(Class(:A))\n Declaration(Class(:B))\n Declaration(Class(:C))\n Declaration(Class(:D))\n Declaration(NamedIndividual(:i))\n Declaration(NamedIndividual(:x))\n";
    let cases = [
        // an equivalence of three with a union, which the pairs of its triples keep in part
        format!(
            "{OFN_HEAD}{declared} EquivalentClasses(:A :B ObjectUnionOf(:C :D))\n SubClassOf(:B owl:Nothing)\n ClassAssertion(:A :i)\n)\n"
        ),
        // an undeclared data property, whose domain triple maps to no axiom
        format!(
            "{OFN_HEAD}{declared} DataPropertyDomain(:p :D)\n DataPropertyAssertion(:p :i \"x\")\n)\n"
        ),
        // an undeclared property of an assertion about a class
        format!("{OFN_HEAD}{declared} SubClassOf(:A :B)\n ObjectPropertyAssertion(:p :A :x)\n)\n"),
        // an undeclared annotation property, and an assertion about the ontology
        format!("{OFN_HEAD}{declared} AnnotationAssertion(:other :A \"streamed\")\n)\n"),
        format!(
            "{OFN_HEAD}{declared} Declaration(AnnotationProperty(:note))\n AnnotationAssertion(:note <http://ex.org/o> \"about\")\n)\n"
        ),
        // a property declared as two kinds
        format!(
            "{OFN_HEAD}{declared} Declaration(ObjectProperty(:p))\n Declaration(DataProperty(:p))\n SubClassOf(:A ObjectSomeValuesFrom(:p :B))\n)\n"
        ),
        // an equivalence that the set of operands makes one class
        format!("{OFN_HEAD}{declared} EquivalentClasses(:A :A)\n)\n"),
        // an annotated assertion of an inverse property, which its triple reads forward
        format!(
            "{OFN_HEAD}{declared} Declaration(ObjectProperty(:p))\n Declaration(NamedIndividual(:j))\n ObjectPropertyDomain(:p :A)\n SubClassOf(:A owl:Nothing)\n ObjectPropertyAssertion(Annotation(rdfs:comment \"inverse\") ObjectInverseOf(:p) :j :i)\n)\n"
        ),
        // an equivalence that an annotated one shares a triple with
        format!(
            "{OFN_HEAD}{declared} EquivalentClasses(:A :B :C)\n EquivalentClasses(Annotation(rdfs:comment \"x\") :A :B)\n)\n"
        ),
        // a header annotation with an undeclared property
        format!(
            "{OFN_HEAD} Annotation(<http://ex.org/note> \"note\")\n{declared} SubClassOf(:A :B)\n)\n"
        ),
        // two annotations that the mapping writes as one triple
        format!(
            "{OFN_HEAD}{declared} SubClassOf(Annotation(rdfs:comment \"x\") Annotation(Annotation(rdfs:label \"nested\") rdfs:comment \"x\") :A ObjectUnionOf(:B :C))\n)\n"
        ),
        // annotations nested deeper than the reader follows
        format!(
            "{OFN_HEAD}{declared} SubClassOf({} :A :B)\n)\n",
            (0..34).fold(String::new(), |inner, i| format!(
                "Annotation({inner} rdfs:comment \"{i}\")"
            ))
        ),
        // a facet that is not an OWL 2 facet
        format!(
            "{OFN_HEAD}{declared} Declaration(DataProperty(:dp))\n DataPropertyRange(:dp DatatypeRestriction(xsd:integer :custom \"1\"^^xsd:integer))\n)\n"
        ),
        // inverse properties with an inverse, whose blank subject the mapping refuses
        format!(
            "{OFN_HEAD}{declared} Declaration(ObjectProperty(:p))\n Declaration(ObjectProperty(:q))\n InverseObjectProperties(ObjectInverseOf(:p) :q)\n)\n"
        ),
    ];
    for (i, c) in cases.iter().enumerate() {
        compare_or_refuse(&format!("case{i}.ofn"), c, true);
    }
    let ttl = [
        // a reified axiom with two sources
        "@prefix : <http://ex.org/> .\n@prefix owl: <http://www.w3.org/2002/07/owl#> .\n@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n:p a owl:ObjectProperty .\n:x :p :z .\n:y :p :z .\n[] a owl:Axiom ; owl:annotatedSource :y, :x ; owl:annotatedProperty :p ; owl:annotatedTarget :z ; rdfs:comment \"two\" .\n",
        // two imports that are literals, and an import that is a blank node
        "@prefix : <http://ex.org/> .\n@prefix owl: <http://www.w3.org/2002/07/owl#> .\n:a a owl:Ontology ; owl:imports \"x\" .\n:b owl:imports \"x\" .\n",
        "@prefix : <http://ex.org/> .\n@prefix owl: <http://www.w3.org/2002/07/owl#> .\n:a a owl:Ontology ; owl:imports _:i .\n:b owl:imports _:i .\n",
        // a composite literal that names a blank node
        "@prefix : <http://ex.org/> .\n_:s :p \"[_:s]\"^^<http://w3id.org/awslabs/neptune/SPARQL-CDTs/List> .\n",
    ];
    for (i, c) in ttl.iter().enumerate() {
        compare_or_refuse(&format!("case{i}.ttl"), c, true);
    }
}
