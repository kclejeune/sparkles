//! Supplemental source identity for the frozen numeric/XML baseline.
//! XML lexical/profile acceptance does not imply canonical XML value equality.
use sparkles_owl::classify::{Classifier, Excluded, Gap, Limits};
use sparkles_owl::profiles::{self, CheckOptions, Profile};
use sparkles_owl::read::{Mode, Ontology};
use sparkles_owl::{model::*, ofn_read, vocab};
use std::time::{Duration, Instant};

const VALID_XML: &str = "<p:a xmlns:p='urn:mixed:element'/>";
const INVALID_XML: &str = "<p:a/>";
const OPAQUE: &str = "literals and datatypes without values the classifier compares";

fn class(name: &str) -> Class {
    Class::new(format!("urn:mixed-numeric-xml:{name}"))
}

fn ontology(text: &str) -> Ontology {
    let doc = ofn_read::parse(text).expect("frozen mixed OFN fixture");
    Ontology::from_axioms(doc.header, doc.axioms, Mode::Strict)
}

fn xml_axiom(xml: &str) -> Axiom {
    Axiom::sub_class_of(
        class("Xml"),
        ClassExpression::DataHasValue(
            DataProperty::new("urn:mixed-numeric-xml:xml"),
            Literal::typed(xml, vocab::RDF_XML_LITERAL),
        ),
    )
}

#[test]
fn mixed_valid_xml_is_partial_while_huge_numeric_equality_entails_named_class() {
    let ontology = ontology(include_str!("fixtures/mixed_numeric_xml/valid.ofn"));
    let expected_xml = xml_axiom(VALID_XML);
    assert!(ontology.axioms.contains_key(&expected_xml));
    let reports = profiles::check(
        &ontology,
        &[Profile::Dl, Profile::El],
        CheckOptions::default(),
    );
    assert_eq!(
        reports
            .iter()
            .map(|report| report.profile)
            .collect::<Vec<_>>(),
        vec![Profile::El, Profile::Dl]
    );
    for report in reports {
        assert!(
            report.conforms,
            "{:?}: {:?}",
            report.profile, report.violations
        );
        assert!(!report.truncated);
        assert!(report.violations.is_empty());
    }

    let mut classifier = Classifier::of(ontology.axiom_set());
    let result = classifier
        .classify(&Limits {
            el_only: true,
            threads: 1,
            deadline: Some(Instant::now() + Duration::from_secs(10)),
            max_memory: Some(64 * 1024 * 1024),
            ..Limits::default()
        })
        .expect("small mixed EL classification must finish within its real limits");
    // complete describes termination here; the exact XML gap below remains present.
    assert!(result.complete);
    assert!(result.consistent);
    assert!(result.taxonomy.is_subclass(&class("A"), &class("B")));
    assert!(
        !result
            .taxonomy
            .is_subclass(&class("A"), &class("Different"))
    );
    assert!(!result.taxonomy.unsatisfiable().any(|c| c == &class("A")));
    assert_eq!(
        classifier.excluded(),
        vec![Excluded {
            axiom: expected_xml.kind,
            gap: Gap::Partial(OPAQUE),
        }]
    );
}

#[test]
fn mixed_unbound_xml_prefix_reports_the_specific_lexical_diagnostic() {
    let ontology = ontology(include_str!("fixtures/mixed_numeric_xml/invalid.ofn"));
    let expected_xml = xml_axiom(INVALID_XML);
    assert!(ontology.axioms.contains_key(&expected_xml));
    let reports = profiles::check(
        &ontology,
        &[Profile::Dl, Profile::El],
        CheckOptions::default(),
    );
    assert_eq!(
        reports
            .iter()
            .map(|report| report.profile)
            .collect::<Vec<_>>(),
        vec![Profile::El, Profile::Dl]
    );
    for report in reports {
        assert!(!report.conforms, "{:?}", report.profile);
        assert!(!report.truncated);
        let [violation] = report.violations.as_slice() else {
            panic!("unexpected diagnostics: {:?}", report.violations);
        };
        assert_eq!(violation.axiom.as_ref(), Some(&expected_xml));
        assert_eq!(violation.graph, Some(0));
        assert_eq!(violation.rule, "OWL 2 Structural Specification §5.7");
        assert_eq!(
            violation.message,
            "the lexical form of \"<p:a/>\"^^<http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral> is not in the lexical space of <http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral>"
        );
    }
}
