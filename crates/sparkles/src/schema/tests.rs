use super::*;
use crate::io::{RdfFormat, Source};
use crate::sparql::QueryOptions;
use crate::store::{Store, StoreOptions};

const PREFIXES: &str = "@prefix ex: <http://ex.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
";

const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema#";

fn ex(l: &str) -> String {
    format!("http://ex.org/{l}")
}

fn store_with(trig: &str) -> Store {
    store_opts(trig, StoreOptions::default())
}

fn store_opts(trig: &str, opts: StoreOptions) -> Store {
    let s = Store::in_memory(opts);
    s.load(&[Source::from_bytes(
        format!("{PREFIXES}{trig}").into_bytes(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    s
}

fn update(s: &Store, u: &str) {
    let text = format!(
        "PREFIX ex: <http://ex.org/>\nPREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>\n{u}"
    );
    crate::sparql::update::update(s, &text, &QueryOptions::default()).unwrap();
}

fn report(s: &Store, opts: &SchemaOptions) -> SchemaReport {
    discover(&s.snapshot(), opts).unwrap()
}

fn class<'a>(r: &'a SchemaReport, iri: &str) -> &'a ClassEntry {
    r.classes
        .iter()
        .find(|c| c.iri == iri)
        .unwrap_or_else(|| panic!("no class {iri}"))
}

fn pred<'a>(r: &'a SchemaReport, iri: &str) -> &'a PredicateEntry {
    r.predicates
        .iter()
        .find(|c| c.iri == iri)
        .unwrap_or_else(|| panic!("no predicate {iri}"))
}

fn graph(s: &str) -> GraphSelection {
    GraphSelection::parse(s).unwrap()
}

#[test]
fn mixed_datatypes_and_kinds() {
    let s = store_with(
        r#"ex:a ex:val 1, "1.5"^^xsd:decimal .
           ex:b ex:val "x", "x"@en .
           ex:c ex:val ex:d, _:n, <<( ex:a ex:val 1 )>> ."#,
    );
    let r = report(&s, &SchemaOptions::default());
    let v = &pred(&r, &ex("val")).observed;
    assert_eq!(
        (
            v.triples,
            v.distinct_subjects,
            v.distinct_objects,
            v.max_per_subject,
            v.subjects_with_multiple
        ),
        (7, 3, 7, 3, 3)
    );
    let one = Some(KindCount {
        triples: 1,
        distinct: 1,
    });
    assert_eq!(v.objects.iri, one);
    assert_eq!(v.objects.blank, one);
    assert_eq!(v.objects.triple_term, one);
    let lits: Vec<(&str, u64, u64)> = v
        .objects
        .literals
        .iter()
        .map(|g| (g.datatype.as_str(), g.triples, g.distinct))
        .collect();
    assert_eq!(
        lits,
        vec![
            (RDF_LANG_STRING, 1, 1),
            ("http://www.w3.org/2001/XMLSchema#decimal", 1, 1),
            ("http://www.w3.org/2001/XMLSchema#integer", 1, 1),
            (XSD_STRING, 1, 1),
        ]
    );
    let langs = v.objects.literals[0].languages.as_ref().unwrap();
    assert_eq!((langs[0].lang.as_str(), langs[0].triples), ("en", 1));
    assert!(v.objects.literals[1].languages.is_none());
    // the JSON shape
    let j = serde_json::to_value(pred(&r, &ex("val"))).unwrap();
    assert_eq!(j["observed"]["objects"]["tripleTerm"]["triples"], 1);
    assert_eq!(j["observed"]["maxPerSubject"], 3);
    assert!(
        j["observed"]["objects"]["literals"][1]
            .get("languages")
            .is_none()
    );
}

#[test]
fn directional_and_non_canonical_literals() {
    let s = store_with(
        r#"ex:a ex:v "a"@en--rtl, "b"@EN, "01"^^xsd:integer, 1, 2.5e0, true .
           ex:b ex:v "c"@en--rtl ."#,
    );
    let r = report(&s, &SchemaOptions::default());
    let v = &pred(&r, &ex("v")).observed;
    let dir = v
        .objects
        .literals
        .iter()
        .find(|g| g.datatype == RDF_DIR_LANG_STRING)
        .unwrap();
    assert_eq!((dir.triples, dir.distinct), (2, 2));
    let l = &dir.languages.as_ref().unwrap()[0];
    assert_eq!(
        (l.lang.as_str(), l.direction.as_deref(), l.triples),
        ("en", Some("rtl"), 2)
    );
    let int = v
        .objects
        .literals
        .iter()
        .find(|g| g.datatype == format!("{XSD_NS}integer"))
        .unwrap();
    // "01" and 1 are two distinct terms
    assert_eq!((int.triples, int.distinct), (2, 2));
    for dt in ["double", "boolean"] {
        assert!(
            v.objects
                .literals
                .iter()
                .any(|g| g.datatype == format!("{XSD_NS}{dt}")),
            "{dt}"
        );
    }
}

#[test]
fn cycles_and_roots() {
    let s = store_with(
        "ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:A . ex:C rdfs:subClassOf ex:A .
         ex:D rdfs:subClassOf ex:D . ex:x a ex:C .",
    );
    let r = report(&s, &SchemaOptions::default());
    assert_eq!(r.hierarchy.roots, vec![ex("A"), ex("D")]);
    assert_eq!(r.hierarchy.cycles, vec![vec![ex("A"), ex("B")]]);
    assert!(class(&r, &ex("D")).declared.super_classes.is_empty());
    assert_eq!(class(&r, &ex("C")).observed.instances, 1);
    assert_eq!(r.totals.classes, 4);
}

#[test]
fn hierarchy_components() {
    // a cycle below an ordinary root is not a root; two independent cycles are
    let s = store_with(
        "ex:T a owl:Class . ex:P rdfs:subClassOf ex:Q, ex:T . ex:Q rdfs:subClassOf ex:P .
         ex:X rdfs:subClassOf ex:Y . ex:Y rdfs:subClassOf ex:Z . ex:Z rdfs:subClassOf ex:X .",
    );
    let r = report(&s, &SchemaOptions::default());
    // owl:Class is an rdf:type object, so a (builtin) class and a root of its own
    assert_eq!(
        r.hierarchy.roots,
        vec![
            ex("T"),
            ex("X"),
            "http://www.w3.org/2002/07/owl#Class".into()
        ]
    );
    assert_eq!(
        r.hierarchy.cycles,
        vec![vec![ex("P"), ex("Q")], vec![ex("X"), ex("Y"), ex("Z")]]
    );
    assert!(class(&r, "http://www.w3.org/2002/07/owl#Class").builtin);
    assert_eq!(
        class(&r, &ex("T")).declared.types,
        vec![format!("{OWL}Class")]
    );
}

#[test]
fn undeclared_classes() {
    let s = store_with("ex:x a ex:Person . ex:Employee rdfs:subClassOf ex:Person .");
    let r = report(&s, &SchemaOptions::default());
    let p = class(&r, &ex("Person"));
    assert_eq!(p.observed.instances, 1);
    assert!(p.declared.types.is_empty() && p.declared.super_classes.is_empty());
    let e = class(&r, &ex("Employee"));
    assert_eq!(e.observed.instances, 0);
    assert_eq!(e.declared.super_classes, vec![ex("Person")]);
}

#[test]
fn delta_changes_and_compaction() {
    let s = store_with("ex:x a ex:P . ex:y a ex:P .");
    let before = s.snapshot().version;
    update(
        &s,
        r#"DELETE DATA { ex:x a ex:P } ;
           INSERT DATA { ex:z a ex:P ; ex:age "x" . GRAPH ex:g1 { ex:y a ex:P } }"#,
    );
    let check = |s: &Store| {
        let r = report(s, &SchemaOptions::default());
        assert_eq!(class(&r, &ex("P")).observed.instances, 2);
        let age = &pred(&r, &ex("age")).observed.objects.literals;
        assert_eq!(age.len(), 1);
        assert_eq!((age[0].datatype.as_str(), age[0].triples), (XSD_STRING, 1));
        let union = SchemaOptions {
            graph: GraphSelection::Union,
            ..Default::default()
        };
        let r = report(s, &union);
        assert_eq!(class(&r, &ex("P")).observed.instances, 2);
        assert_eq!(pred(&r, RDF_TYPE).observed.triples, 2);
        let g1 = SchemaOptions {
            graph: graph("http://ex.org/g1"),
            ..Default::default()
        };
        let r = report(s, &g1);
        assert_eq!(class(&r, &ex("P")).observed.instances, 1);
        r.snapshot.version
    };
    assert!(check(&s) > before);
    s.compact().unwrap();
    check(&s);
}

#[test]
fn observation_is_not_a_contract() {
    let s = store_with(
        r#"ex:name a owl:FunctionalProperty . ex:a ex:name "A" . ex:b ex:name "B1", "B2" ."#,
    );
    let r = report(&s, &SchemaOptions::default());
    let p = pred(&r, &ex("name"));
    assert!(
        p.declared
            .types
            .contains(&format!("{OWL}FunctionalProperty"))
    );
    assert_eq!(
        (
            p.observed.max_per_subject,
            p.observed.subjects_with_multiple
        ),
        (2, 1)
    );
    update(&s, r#"DELETE DATA { ex:b ex:name "B2" }"#);
    let r = report(&s, &SchemaOptions::default());
    assert_eq!(pred(&r, &ex("name")).observed.max_per_subject, 1);
}

#[test]
fn inferred_graph_toggle() {
    // materialized inferences as the reasoner writes them
    let s = store_with(
        "ex:C rdfs:subClassOf ex:B . ex:x a ex:C .
         <urn:x-sparkles:inferred> { ex:x a ex:B . ex:C rdfs:subClassOf rdfs:Resource . }",
    );
    let with = SchemaOptions {
        inferred_graph: Some("urn:x-sparkles:inferred".into()),
        ..Default::default()
    };
    let r = report(&s, &with);
    assert_eq!(class(&r, &ex("B")).observed.instances, 1);
    assert_eq!(class(&r, &ex("C")).declared.super_classes, vec![ex("B")]);
    let without = SchemaOptions {
        include_inferred: false,
        ..with.clone()
    };
    let r = report(&s, &without);
    assert_eq!(class(&r, &ex("B")).observed.instances, 0);
    assert_eq!(class(&r, &ex("C")).declared.super_classes, vec![ex("B")]);
    // union without inferences leaves the inferred graph out too
    let r = report(
        &s,
        &SchemaOptions {
            graph: GraphSelection::Union,
            ..without.clone()
        },
    );
    assert_eq!(class(&r, &ex("B")).observed.instances, 0);
    // declared=all reads the inferred super too
    let r = report(
        &s,
        &SchemaOptions {
            declared_from_inferred: true,
            ..with
        },
    );
    assert_eq!(
        class(&r, &ex("C")).declared.super_classes,
        vec![ex("B"), format!("{RDFS}Resource")]
    );
}

#[test]
fn union_default_graph_counts_every_graph() {
    let s = store_opts(
        "ex:a a ex:K . ex:g { ex:b a ex:K . ex:a a ex:K . }",
        StoreOptions {
            union_default_graph: true,
            ..Default::default()
        },
    );
    let r = report(&s, &SchemaOptions::default());
    assert_eq!(class(&r, &ex("K")).observed.instances, 2);
    assert_eq!(r.totals.triples, 2);
}

#[test]
fn declared_graph_and_labels() {
    let s = store_with(
        r#"ex:x a ex:Person ; ex:knows ex:y .
           ex:onto {
             ex:O a owl:Ontology ; rdfs:label "Onto"@en, "Onto" ; owl:versionInfo "1.0" .
             ex:Person a owl:Class ; rdfs:label "Person"@en, "Personne"@fr ; rdfs:comment "A human" ;
               owl:disjointWith ex:Rock ; rdfs:subClassOf [ a owl:Restriction ] .
             ex:knows a owl:ObjectProperty ; rdfs:domain ex:Person ; rdfs:range ex:Person ;
               rdfs:subPropertyOf ex:related ; owl:inverseOf ex:knownBy .
             ex:unused rdfs:domain [ owl:unionOf (ex:A ex:B) ] .
           }"#,
    );
    // declarations are in ex:onto, data in the default graph
    let r = report(&s, &SchemaOptions::default());
    assert!(class(&r, &ex("Person")).declared.types.is_empty());
    assert!(r.ontology.is_empty());
    let opts = SchemaOptions {
        declared_graph: Some(graph("<http://ex.org/onto>")),
        ..Default::default()
    };
    let r = report(&s, &opts);
    assert_eq!(r.selection.declared_graph, "http://ex.org/onto");
    let p = class(&r, &ex("Person"));
    assert_eq!(p.observed.instances, 1);
    assert_eq!(p.declared.types, vec![format!("{OWL}Class")]);
    assert_eq!(p.declared.disjoint_with, vec![ex("Rock")]);
    assert!(p.declared.super_classes.is_empty());
    assert_eq!(
        p.declared.labels,
        vec![
            Lit {
                value: "Person".into(),
                lang: Some("en".into())
            },
            Lit {
                value: "Personne".into(),
                lang: Some("fr".into())
            },
        ]
    );
    assert_eq!(p.declared.comments[0].value, "A human");
    assert_eq!(r.totals.anonymous_class_expressions, 2);
    let k = pred(&r, &ex("knows"));
    assert_eq!(k.observed.triples, 1);
    assert_eq!(k.declared.types, vec![format!("{OWL}ObjectProperty")]);
    assert_eq!(
        (&k.declared.domains, &k.declared.ranges),
        (&vec![ex("Person")], &vec![ex("Person")])
    );
    assert_eq!(k.declared.super_properties, vec![ex("related")]);
    assert_eq!(k.declared.inverse_of, vec![ex("knownBy")]);
    // declared-only predicates appear with zero observed triples
    assert_eq!(pred(&r, &ex("related")).observed.triples, 0);
    assert_eq!(pred(&r, &ex("unused")).observed.triples, 0);
    assert_eq!(r.ontology.len(), 1);
    assert_eq!(r.ontology[0].iri, ex("O"));
    assert_eq!(r.ontology[0].labels.len(), 2);
    assert_eq!(r.ontology[0].version_info[0].value, "1.0");
}

#[test]
fn anonymous_type_targets_and_graph_dedupe() {
    let s = store_with(
        "ex:a a [ a owl:Restriction ] . ex:b a _:k . ex:c a _:k .
         ex:g1 { ex:s ex:p ex:o . } ex:g2 { ex:s ex:p ex:o . ex:s ex:p ex:o2 . }",
    );
    let r = report(&s, &SchemaOptions::default());
    assert_eq!(r.totals.anonymous_type_targets, 2);
    let r = report(
        &s,
        &SchemaOptions {
            graph: GraphSelection::Union,
            ..Default::default()
        },
    );
    let p = &pred(&r, &ex("p")).observed;
    assert_eq!(
        (
            p.triples,
            p.distinct_subjects,
            p.distinct_objects,
            p.max_per_subject
        ),
        (2, 1, 2, 2)
    );
    assert_eq!(p.objects.iri.unwrap().triples, 2);
}

#[test]
fn sorted_by_iri_and_paginated() {
    let data: String = (0..25)
        .map(|i| format!("ex:i{i} a ex:C{i:04} .\n"))
        .collect();
    let s = store_with(&data);
    let r = report(&s, &SchemaOptions::default());
    assert_eq!(r.classes.len(), 25);
    assert!(r.classes.windows(2).all(|w| w[0].iri < w[1].iri));
    let (p1, more) = page_after(&r.classes, None, 10);
    assert!(more && p1.len() == 10);
    let (p2, more) = page_after(&r.classes, Some(&p1[9].iri), 10);
    assert!(more && p2[0].iri == ex("C0010"));
    let (p3, more) = page_after(&r.classes, Some(&p2[9].iri), 10);
    assert!(!more && p3.len() == 5);
    let (p4, more) = page_after(&r.classes, Some("http://ex.org/C0003x"), 1);
    assert!(more && p4[0].iri == ex("C0004"));
}

#[test]
fn budgets() {
    let s = store_with("ex:a a ex:A . ex:b a ex:B . ex:c a ex:C . ex:c ex:p 1 .");
    let e = discover(
        &s.snapshot(),
        &SchemaOptions {
            max_entries: 2,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.to_string(), "dataset has 3 classes (limit 2)");
    let e = discover(
        &s.snapshot(),
        &SchemaOptions {
            deadline: Some(Instant::now() - std::time::Duration::from_millis(1)),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(matches!(e, SchemaError::Timeout { .. }), "{e}");
    let cancel = Arc::new(AtomicBool::new(true));
    let e = discover(
        &s.snapshot(),
        &SchemaOptions {
            cancel: Some(cancel),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(matches!(e, SchemaError::Cancelled));
    let e = discover(
        &s.snapshot(),
        &SchemaOptions {
            graph: graph("http://ex.org/missing"),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(matches!(e, SchemaError::NoSuchGraph(_)));
}

#[test]
fn many_vocabulary_literals() {
    // more distinct base-vocabulary literals than one batched key lookup
    let data: String = (0..10_000)
        .map(|i| match i % 3 {
            0 => format!("ex:s{i} ex:v \"s{i}\" .\n"),
            1 => format!("ex:s{i} ex:v \"l{i}\"@de .\n"),
            _ => format!("ex:s{i} ex:v \"{i}\"^^xsd:gYear .\n"),
        })
        .collect();
    let s = store_with(&data);
    let r = report(&s, &SchemaOptions::default());
    let v = &pred(&r, &ex("v")).observed;
    assert_eq!((v.triples, v.distinct_objects), (10_000, 10_000));
    let groups: Vec<(&str, u64, u64)> = v
        .objects
        .literals
        .iter()
        .map(|g| (g.datatype.as_str(), g.triples, g.distinct))
        .collect();
    assert_eq!(
        groups,
        vec![
            (RDF_LANG_STRING, 3333, 3333),
            ("http://www.w3.org/2001/XMLSchema#gYear", 3333, 3333),
            (XSD_STRING, 3334, 3334),
        ]
    );
}

#[test]
fn graph_selection_parsing() {
    assert_eq!(graph("default"), GraphSelection::Default);
    assert_eq!(graph("urn:x-arq:DefaultGraph"), GraphSelection::Default);
    assert_eq!(graph("urn:x-arq:UnionGraph"), GraphSelection::Union);
    assert_eq!(graph("<http://ex.org/g>").name(), "http://ex.org/g");
    assert!(GraphSelection::parse("not an iri").is_err());
}

#[test]
fn empty_store() {
    let s = Store::in_memory(StoreOptions::default());
    let r = report(&s, &SchemaOptions::default());
    assert_eq!(r.totals.triples, 0);
    assert!(r.classes.is_empty() && r.predicates.is_empty());
    assert!(r.hierarchy.roots.is_empty());
}

/// The objects of `(s, p)`, with `p` a VoID local name or a full IRI.
fn objects(g: &[oxrdf::Triple], s: &oxrdf::NamedOrBlankNode, p: &str) -> Vec<Term> {
    let p = if p.contains(':') {
        p.to_string()
    } else {
        format!("{VOID_NS}{p}")
    };
    g.iter()
        .filter(|t| &t.subject == s && t.predicate.as_str() == p)
        .map(|t| t.object.clone())
        .collect()
}

/// The single integer object of `(s, p)`.
fn number(g: &[oxrdf::Triple], s: &oxrdf::NamedOrBlankNode, p: &str) -> u64 {
    match objects(g, s, p).as_slice() {
        [Term::Literal(l)] => {
            assert_eq!(l.datatype().as_str(), format!("{XSD_NS}integer"));
            l.value().parse().unwrap()
        }
        o => panic!("{p}: {o:?}"),
    }
}

fn iri_term(iri: &str) -> Term {
    Term::NamedNode(NamedNode::new_unchecked(iri))
}

fn iri_subject(iri: &str) -> oxrdf::NamedOrBlankNode {
    NamedNode::new_unchecked(iri).into()
}

/// The partition node whose `key` (`class` or `property`) is `iri`.
fn partition(g: &[oxrdf::Triple], key: &str, iri: &str) -> oxrdf::NamedOrBlankNode {
    let key = format!("{VOID_NS}{key}");
    g.iter()
        .find(|t| t.predicate.as_str() == key && t.object == iri_term(iri))
        .unwrap_or_else(|| panic!("no partition of {iri}"))
        .subject
        .clone()
}

fn parse_rdf(text: &str, format: RdfFormat) -> Vec<oxrdf::Triple> {
    oxrdfio::RdfParser::from_format(format)
        .for_slice(text.as_bytes())
        .map(|q| oxrdf::Triple::from(q.unwrap()))
        .collect()
}

#[test]
fn void_description() {
    let s = store_with(
        r#"ex:Person a owl:Class ; rdfs:label "Person"@en .
           ex:Student rdfs:subClassOf ex:Person .
           ex:alice a ex:Student ; ex:knows ex:bob, _:x ; ex:name "Alice" .
           ex:bob a ex:Person ; ex:name "Bob" .
           _:x a ex:Person .
           ex:knows rdfs:domain ex:Person .
           ex:g1 { ex:carol a ex:Person ; ex:name "Carol" . }"#,
    );
    let r = report(
        &s,
        &SchemaOptions {
            term_totals: true,
            ..Default::default()
        },
    );
    assert_eq!(
        r.term_totals,
        Some(TermTotals {
            distinct_subjects: 6,
            distinct_objects: 8,
            entities: 5,
        })
    );
    // the JSON document does not change
    assert!(
        serde_json::to_value(&r)
            .unwrap()
            .get("termTotals")
            .is_none()
    );
    let opts = VoidOptions {
        dataset: "my ds",
        declarations: false,
        prefixes: vec![("ex".into(), "http://ex.org/".into())],
    };
    let text = void_text(&r, &opts, RdfFormat::Turtle);
    assert!(
        text.contains("@prefix void: <http://rdfs.org/ns/void#>"),
        "{text}"
    );
    assert!(text.contains("ex:Person"), "{text}");
    let g = parse_rdf(&text, RdfFormat::Turtle);
    let iri = description_iri("my ds", r.snapshot.version);
    assert!(iri.starts_with("urn:x-sparkles:schema:my%20ds:"), "{iri}");
    let d = iri_subject(&iri);
    assert_eq!(
        objects(&g, &d, RDF_TYPE),
        [iri_term(&format!("{VOID_NS}Dataset"))]
    );
    for (p, n) in [
        ("triples", 11),
        ("entities", 5),
        ("classes", 3),
        ("properties", 6),
        ("distinctSubjects", 6),
        ("distinctObjects", 8),
    ] {
        assert_eq!(number(&g, &d, p), n, "{p}\n{text}");
    }
    assert_eq!(objects(&g, &d, "classPartition").len(), 3);
    assert_eq!(objects(&g, &d, "propertyPartition").len(), 6);
    let person = partition(&g, "class", &ex("Person"));
    assert_eq!(number(&g, &person, "entities"), 2);
    assert!(objects(&g, &d, "classPartition").contains(&Term::from(person)));
    let ty = partition(&g, "property", RDF_TYPE);
    assert_eq!(
        (
            number(&g, &ty, "triples"),
            number(&g, &ty, "distinctSubjects"),
            number(&g, &ty, "distinctObjects")
        ),
        (4, 4, 3)
    );
    assert_eq!(
        number(&g, &partition(&g, "property", &ex("name")), "triples"),
        2
    );
    // statistics only: no declaration
    let sub = format!("{RDFS}subClassOf");
    assert!(!g.iter().any(|t| t.predicate.as_str() == sub));

    // with the declarations, and in the other syntaxes
    let opts = VoidOptions {
        declarations: true,
        ..opts
    };
    let all = void_triples(&r, &opts);
    let person_iri = iri_term(&ex("Person"));
    assert_eq!(
        objects(&all, &iri_subject(&ex("Student")), &sub),
        std::slice::from_ref(&person_iri)
    );
    assert_eq!(
        objects(&all, &iri_subject(&ex("knows")), &format!("{RDFS}domain")),
        [person_iri]
    );
    assert_eq!(
        objects(&all, &iri_subject(&ex("Person")), RDFS_LABEL),
        [Term::Literal(
            oxrdf::Literal::new_language_tagged_literal("Person", "en").unwrap()
        )]
    );
    for format in [
        RdfFormat::NTriples,
        RdfFormat::RdfXml,
        RdfFormat::NQuads,
        RdfFormat::TriG,
    ] {
        let g = parse_rdf(&void_text(&r, &opts, format), format);
        assert_eq!(g.len(), all.len(), "{format:?}");
        assert_eq!(number(&g, &d, "triples"), 11, "{format:?}");
    }

    // without term totals, the description leaves out what was not counted
    let r = report(&s, &SchemaOptions::default());
    assert_eq!(r.term_totals, None);
    let g = void_triples(&r, &VoidOptions::default());
    let d = iri_subject(&description_iri("", r.snapshot.version));
    assert_eq!(number(&g, &d, "triples"), 11);
    for p in ["entities", "distinctSubjects", "distinctObjects"] {
        assert!(objects(&g, &d, p).is_empty(), "{p}");
    }
}

#[test]
fn term_totals_follow_the_selection() {
    let s = store_with(
        "ex:a ex:p ex:b . ex:g1 { ex:a ex:p ex:b . ex:c ex:p \"x\" . } ex:g2 { _:z ex:p ex:b . }",
    );
    let totals = |graph: &str| {
        report(
            &s,
            &SchemaOptions {
                graph: GraphSelection::parse(graph).unwrap(),
                term_totals: true,
                ..Default::default()
            },
        )
        .term_totals
        .unwrap()
    };
    let t = |s, o, e| TermTotals {
        distinct_subjects: s,
        distinct_objects: o,
        entities: e,
    };
    assert_eq!(totals("default"), t(1, 1, 1));
    assert_eq!(totals(&ex("g1")), t(2, 2, 2));
    // a triple in several graphs counts once; a blank subject is no entity
    assert_eq!(totals("union"), t(3, 2, 2));
}
