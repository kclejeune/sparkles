//! Reads of the terminology ([`Scope::Terminology`](super::Scope)), an experimental,
//! crate-private scope that is not yet a fail-closed reader (roadmap item 1207). Its
//! tests live in the crate, since nothing outside it can construct the scope.

use crate::read::{self, Mode, ReadOptions};
use crate::*;
use oxrdf::Triple;
use oxrdfio::{RdfFormat, RdfParser};
use std::collections::BTreeSet;

const PREFIXES: &str = "@prefix : <http://example.org/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
";

fn ttl(body: &str) -> Vec<Triple> {
    let src = format!("{PREFIXES}{body}");
    RdfParser::from_format(RdfFormat::Turtle)
        .for_reader(src.as_bytes())
        .map(|q| q.expect("turtle").into())
        .collect()
}

fn lenient(body: &str) -> read::Ontology {
    read::read_graph(
        ttl(body),
        ReadOptions {
            mode: Mode::Lenient,
            ..Default::default()
        },
    )
}

fn ex(local: &str) -> String {
    format!("http://example.org/{local}")
}

fn terminology(body: &str) -> read::Ontology {
    read::read_graph_scoped(ttl(body), ReadOptions::default(), read::Scope::Terminology)
}

/// The logical axioms that are not assertions.
fn tbox(o: &read::Ontology) -> BTreeSet<Axiom> {
    o.axiom_set()
        .filter(|a| {
            !a.kind.axiom_type().is_assertion()
                && !matches!(
                    a.kind,
                    AxiomKind::Declaration(_) | AxiomKind::AnnotationAssertion(..)
                )
        })
        .cloned()
        .collect()
}

#[test]
fn every_structural_term_has_a_role() {
    for p in vocab::STRUCTURAL_PREDICATES
        .iter()
        .chain(vocab::FACETS.iter())
    {
        assert!(read::predicate_role(p).is_some(), "{p} has no role");
    }
    for t in vocab::STRUCTURAL_TYPES {
        assert!(read::type_role(t).is_some(), "{t} has no role");
    }
    // assertion and annotation predicates are no part of the terminology
    for p in [
        vocab::OWL_SAME_AS,
        vocab::OWL_DIFFERENT_FROM,
        vocab::RDFS_LABEL,
    ] {
        assert!(read::predicate_role(p).is_none(), "{p}");
    }
}

const TERMINOLOGY: &str = ":A a owl:Class . :B a owl:Class . :C a owl:Class .
:r a owl:ObjectProperty ; rdfs:domain :B ; a owl:TransitiveProperty .
:A owl:disjointWith :B .
:C rdfs:subClassOf :A, [ a owl:Restriction ; owl:onProperty :r ; owl:someValuesFrom :B ] .
:D owl:equivalentClass [ a owl:Class ; owl:intersectionOf ( :A :C ) ] .
:E owl:intersectionOf ( :A :B ) .
[] a owl:AllDisjointClasses ; owl:members ( :A :C :D ) .
[] a owl:Axiom ; owl:annotatedSource :C ; owl:annotatedProperty rdfs:subClassOf ;
   owl:annotatedTarget :A ; rdfs:comment \"c\" .
:C rdfs:label \"C\" .
";

#[test]
fn a_terminology_read_maps_what_a_full_read_maps() {
    let full = lenient(TERMINOLOGY);
    let t = terminology(TERMINOLOGY);
    assert_eq!(t.error_count, 0, "{:?}", t.diagnostics);
    assert_eq!(tbox(&t), tbox(&full));
    assert!(!tbox(&t).is_empty());
    // the full read is unchanged by the option's existence: no errors, same axioms
    assert_eq!(full.error_count, 0, "{:?}", full.diagnostics);
}

#[test]
fn a_terminology_read_reports_each_root_it_cannot_map() {
    let base = ":A a owl:Class . :B a owl:Class . :C a owl:Class . :A owl:disjointWith :B .";
    for (body, what) in [
        (
            ":C owl:intersectionOf _:l1 . _:l1 rdf:first :A ; rdf:rest _:l2 . _:l2 rdf:first :B .",
            "a named intersection whose list has no end",
        ),
        (
            ":C owl:unionOf _:l1 . _:l1 rdf:first :A .",
            "a named union whose list has no end",
        ),
        (
            ":C owl:oneOf _:l1 . _:l1 rdf:first :a .",
            "a named enumeration",
        ),
        (
            ":C owl:complementOf [ a owl:Restriction ; owl:onProperty :r ] .",
            "a named complement of a broken restriction",
        ),
        (
            "rdfs:subClassOf a owl:AnnotationProperty . :C rdfs:subClassOf :A .",
            "a subclass axiom read as an annotation",
        ),
        (
            "[] a owl:AllDisjointClasses ; owl:members ( :C ) .",
            "a disjointness with only one member",
        ),
        (
            ":C rdfs:subClassOf _:a . _:a owl:intersectionOf ( :A _:a ) .",
            "an expression that contains itself",
        ),
        (
            ":C rdfs:subClassOf [ owl:intersectionOf _:l ] . _:l rdf:first :A ; rdf:rest _:l .",
            "a list whose rest loops",
        ),
        (
            ":r a owl:ObjectProperty . :C rdfs:subClassOf [ a owl:Restriction ;
               owl:onProperty :r ; owl:someValuesFrom :A ; owl:allValuesFrom :B ] .",
            "a restriction with two fillers",
        ),
        (
            ":C rdfs:subClassOf [ a owl:Class ; owl:intersectionOf ( :A :B ) ;
               owl:unionOf ( :A :B ) ] .",
            "an expression with two constructors",
        ),
        (
            ":r a owl:ObjectProperty . :C rdfs:subClassOf [ a owl:Restriction ;
               owl:onProperty :r ; owl:someValuesFrom :A ; owl:intersectionOf ( :A :B ) ] .",
            "a restriction with a constructor beside it",
        ),
        (
            ":d a owl:DatatypeProperty . :C rdfs:subClassOf [ a owl:Restriction ;
               owl:onProperty :d ; owl:someValuesFrom [ a rdfs:Datatype ;
                 owl:onDatatype xsd:integer ;
                 owl:withRestrictions ( [ xsd:minInclusive 1 ; xsd:maxInclusive 5 ] ) ] ] .",
            "a facet node with two facets",
        ),
        (
            ":N a owl:Restriction ; owl:onProperty :r ; owl:someValuesFrom :A .",
            "a restriction on a named node",
        ),
    ] {
        let o = terminology(&format!("{base} {body}"));
        assert!(o.error_count > 0, "{what}: {:?}", o.diagnostics);
        assert!(
            o.errors().any(|d| d.code == "unmapped"),
            "{what}: {:?}",
            o.diagnostics
        );
        // the full lenient read drops the same root without an error
        let full = lenient(&format!("{base} {body}"));
        assert_eq!(full.error_count, 0, "{what}: {:?}", full.diagnostics);
    }
    // a tautology, a typed rdf:nil and repeated disjointness operands map without losing meaning
    for body in [
        ":C owl:intersectionOf ( :C ) .",
        "rdf:nil a rdf:List .",
        ":C owl:disjointWith :C .",
        "[] a owl:AllDisjointClasses ; owl:members ( :C :C ) .",
        "[] a owl:AllDisjointClasses ; owl:members ( :A :A :C ) .",
    ] {
        let o = terminology(&format!("{base} {body}"));
        assert_eq!(o.error_count, 0, "{body}: {:?}", o.diagnostics);
    }
    // a structure two roots share is read for each
    let o = terminology(&format!(
        "{base} :r a owl:ObjectProperty . :C rdfs:subClassOf _:s . :A rdfs:subClassOf _:s .
         _:s a owl:Restriction ; owl:onProperty :r ; owl:someValuesFrom :B ."
    ));
    assert_eq!(o.error_count, 0, "{:?}", o.diagnostics);
    let subs = tbox(&o)
        .into_iter()
        .filter(|a| {
            matches!(
                a.kind,
                AxiomKind::SubClassOf(_, ClassExpression::ObjectSomeValuesFrom(..))
            )
        })
        .count();
    assert_eq!(subs, 2);
}

#[test]
fn a_terminology_read_leaves_out_every_assertion() {
    let deep = {
        let mut t = String::new();
        let n = read::MAX_NESTING + 2;
        for i in 0..n {
            let next = if i + 1 == n {
                ":F".to_string()
            } else {
                format!("_:e{}", i + 1)
            };
            t.push_str(&format!(
                "_:e{i} a owl:Restriction ; owl:onProperty :r ; owl:someValuesFrom {next} .\n"
            ));
        }
        t
    };
    let abox = format!(
        ":x a :A, _:e0 . :x :r :y . :x owl:sameAs :z . :x owl:differentFrom :y .
         [] a owl:AllDifferent ; owl:distinctMembers ( :x :y ) .
         [] a owl:NegativePropertyAssertion ; owl:sourceIndividual :x ;
            owl:assertionProperty :q ; owl:targetIndividual :y .
         :x a [ a owl:Restriction ; owl:onProperty :s ; owl:someValuesFrom :G ] .
         [] a owl:Axiom ; owl:annotatedSource :x ; owl:annotatedProperty rdf:type ;
            owl:annotatedTarget :F ; rdfs:comment \"c\" . :x a :F .
         {deep}"
    );
    let alone = terminology(TERMINOLOGY);
    let with = terminology(&format!("{TERMINOLOGY} {abox}"));
    assert_eq!(with.error_count, 0, "{:?}", with.diagnostics);
    assert!(
        with.axiom_set()
            .all(|a| !a.kind.axiom_type().is_assertion()),
        "an assertion was read"
    );
    assert_eq!(tbox(&with), tbox(&alone));
    // the assertions do not type the terminology: :q, :s, :F and :G stay unknown
    for iri in ["q", "s", "F", "G"] {
        assert!(
            with.typing.kinds(&ex(iri)).is_empty(),
            "{iri} was typed from an assertion"
        );
    }
    // an expression a terminology axiom shares is read, with its error
    let shared = terminology(&format!("{TERMINOLOGY} {abox} :C rdfs:subClassOf _:e0 ."));
    assert!(shared.error_count > 0, "{:?}", shared.diagnostics);
}

#[test]
fn a_terminology_read_reads_a_repeated_statement_once() {
    // the same structure in two graphs, as a graph copy leaves it
    let ts = ttl(TERMINOLOGY);
    let n = ts.len();
    let mut triples = ts.clone();
    triples.extend(ts);
    let mut graphs = vec![0; n];
    graphs.extend(vec![1; n]);
    let o = read::read_scoped(
        triples,
        graphs,
        &[0],
        ReadOptions::default(),
        read::Scope::Terminology,
    );
    assert_eq!(o.error_count, 0, "{:?}", o.diagnostics);
    assert_eq!(tbox(&o), tbox(&terminology(TERMINOLOGY)));
}

#[test]
fn admitted_parse_charges_each_reuse_of_a_blank_inverse_property() {
    let iri = format!("urn:{}", "p".repeat(32 << 10));
    let mut body = format!("<{iri}> a owl:ObjectProperty . _:inverse owl:inverseOf <{iri}> .");
    for i in 0..32 {
        body.push_str(&format!(
            " _:inverse rdfs:domain :C{i} . :C{i} a owl:Class ."
        ));
    }
    let triples = ttl(&body);
    let flat: usize = triples
        .iter()
        .map(|triple| 1024 + 8 * read::triple_payload(triple))
        .sum();
    let count_domains = |o: &read::Ontology| {
        o.axiom_set()
            .filter(|a| {
                matches!(
                    a.kind,
                    AxiomKind::ObjectPropertyDomain(ObjectPropertyExpression::InverseOf(_), _)
                )
            })
            .count()
    };
    assert_eq!(count_domains(&lenient(&body)), 32);
    let graph_of = vec![0; triples.len()];
    let used = std::cell::Cell::new(0);
    let answer = read::read_admitted(triples, graph_of, &[0], ReadOptions::default(), &|bytes| {
        let next = used.get() + bytes;
        used.set(next);
        next <= flat + 3 * iri.len()
    });
    assert!(answer.is_none());
    assert!(used.get() > flat);
}

#[test]
fn admitted_parse_can_refuse_the_first_facet_copy_before_memoization() {
    let body = format!(
        ":p a owl:DatatypeProperty ; rdfs:range _:range .
        _:range a rdfs:Datatype ; owl:onDatatype xsd:string ; owl:withRestrictions (_:facet) .
        _:facet xsd:minInclusive \"{}\" .",
        "x".repeat(32 << 10),
    );
    let triples = ttl(&body);
    assert!(lenient(&body).axiom_set().any(|ax| matches!(
        ax.kind,
        AxiomKind::DataPropertyRange(_, DataRange::DatatypeRestriction(..))
    )));
    let facet = triples
        .iter()
        .find(|t| t.predicate.as_str().ends_with("#minInclusive"))
        .unwrap();
    let bytes = facet.predicate.as_str().len() + super::term_payload(&facet.object);
    let graph_of = vec![0; triples.len()];
    let refused = std::cell::Cell::new(false);
    let result = read::read_admitted(triples, graph_of, &[0], ReadOptions::default(), &|n| {
        if n == bytes {
            refused.set(true);
            return false;
        }
        true
    });
    assert!(refused.get());
    assert!(result.is_none());
}
