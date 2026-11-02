//! The built-in SKOS shapes (`sparkles::skos`) against the examples of the W3C SKOS
//! Reference: each example the Reference calls not consistent breaks the integrity
//! condition it illustrates, and each one it calls consistent passes. The example numbers
//! are the Reference's. The shapes also lay out forms with `shapes=skos`.
#![cfg(feature = "shacl")]

use oxrdf::Term;
use sparkles::Dataset;
use sparkles::forms::{
    FieldChange, FormChange, FormRequest, FormSaved, FormSettings, ShapesSource,
};
use sparkles_shacl::ValidateOptions;

const PREFIXES: &str = "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
@prefix skosxl: <http://www.w3.org/2008/05/skos-xl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@base <http://ex/> .
";

/// The messages of the violations the shapes report for `turtle`, each with its focus.
fn violations(turtle: &str) -> Vec<(String, String)> {
    let ds = Dataset::memory();
    ds.load_str(
        &format!("{PREFIXES}{turtle}"),
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    let report = ds
        .validation()
        .shacl(&sparkles::skos::shapes(), &ValidateOptions::default())
        .unwrap();
    report
        .results
        .iter()
        .filter(|r| r.severity.as_str().ends_with("#Violation"))
        .map(|r| {
            (
                r.focus_node.to_string(),
                r.message().unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// The conditions (`S13`, `S14`, …) the violations name.
fn conditions(turtle: &str) -> Vec<String> {
    let mut out: Vec<String> = violations(turtle)
        .into_iter()
        .map(|(_, m)| m.split(':').next().unwrap_or("").to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn breaks(turtle: &str, condition: &str) {
    let c = conditions(turtle);
    assert!(
        c.iter().any(|x| x == condition),
        "expected {condition} for\n{turtle}\nbut got {c:?}"
    );
}

fn consistent(turtle: &str) {
    let v = violations(turtle);
    assert!(
        v.is_empty(),
        "expected no violations for\n{turtle}\nbut got {v:?}"
    );
}

#[test]
fn the_shapes_parse_and_name_their_namespace() {
    let shapes = sparkles::skos::shapes();
    assert!(!shapes.is_empty());
    assert!(sparkles::skos::SHAPES.contains(sparkles::skos::SHAPES_NS));
}

#[test]
fn s9_a_concept_is_not_a_concept_scheme() {
    breaks("<A> a skos:Concept , skos:ConceptScheme .", "S9");
    consistent("<A> a skos:Concept . <S> a skos:ConceptScheme . <A> skos:inScheme <S> .");
}

#[test]
fn s13_labels_are_pairwise_disjoint() {
    // Examples 13 to 15
    breaks(
        r#"<Love> skos:prefLabel "love"@en ; skos:altLabel "love"@en ."#,
        "S13",
    );
    breaks(
        r#"<Love> skos:altLabel "love"@en ; skos:hiddenLabel "love"@en ."#,
        "S13",
    );
    breaks(
        r#"<Love> skos:prefLabel "love"@en ; skos:hiddenLabel "love"@en ."#,
        "S13",
    );
    // the same text in another language is another label
    consistent(r#"<Love> skos:prefLabel "love"@en ; skos:altLabel "love"@fr ."#);
}

#[test]
fn s14_one_preferred_label_per_language() {
    // Example 12
    breaks(
        r#"<Love> skos:prefLabel "love"@en ; skos:prefLabel "adoration"@en ."#,
        "S14",
    );
    // two untagged preferred labels
    breaks(r#"<Love> skos:prefLabel "love" , "adoration" ."#, "S14");
    // Examples 10 and 11 of the Reference are consistent
    consistent(r#"<Love> skos:prefLabel "love"@en ; skos:altLabel "adoration"@en ."#);
    consistent(r#"<Love> skos:prefLabel "love"@en ; skos:prefLabel "amour"@fr ."#);
}

#[test]
fn s27_related_is_disjoint_with_broader_transitive() {
    // Example 25 is consistent, Examples 26 to 29 are not
    consistent("<A> skos:broader <B> ; skos:related <C> .");
    breaks("<A> skos:broader <B> ; skos:related <B> .", "S27");
    breaks(
        "<A> skos:broader <B> ; skos:related <C> . <B> skos:broader <C> .",
        "S27",
    );
    breaks("<A> skos:broaderTransitive <C> ; skos:related <C> .", "S27");
    breaks(
        "<A> skos:narrower <B> ; skos:related <C> . <B> skos:narrower <C> .",
        "S27",
    );
}

#[test]
fn s37_collections_are_not_concepts() {
    // Examples 45 to 47 are not consistent, Example 48 is
    breaks(
        "<A> skos:narrower <B> . <B> rdf:type skos:Collection .",
        "S37",
    );
    breaks(
        "<A> skos:broader <B> . <B> rdf:type skos:Collection .",
        "S37",
    );
    breaks(
        "<A> skos:related <B> . <B> rdf:type skos:Collection .",
        "S37",
    );
    consistent(
        r#"<A> skos:narrower <B> , <C> , <D> .
        <ResourceCollection> rdfs:label "Resource Collection"@en ; skos:member <B> , <C> , <D> ."#,
    );
    breaks("<A> a skos:Concept , skos:Collection .", "S37");
    breaks("<A> a skos:ConceptScheme , skos:Collection .", "S37");
}

#[test]
fn s46_exact_match_is_disjoint_with_broad_and_related_match() {
    // Example 49 is consistent, Examples 52 and 53 are not
    consistent("<A> skos:exactMatch <B> .");
    breaks("<A> skos:exactMatch <B> ; skos:broadMatch <B> .", "S46");
    breaks("<A> skos:exactMatch <B> ; skos:relatedMatch <B> .", "S46");
    // through the symmetry of exactMatch and the inverse narrowMatch
    breaks(
        "<A> skos:exactMatch <B> . <B> skos:narrowMatch <A> .",
        "S46",
    );
    breaks(
        "<B> skos:exactMatch <A> . <B> skos:relatedMatch <A> .",
        "S46",
    );
    consistent("<A> skos:exactMatch <B> ; skos:closeMatch <B> .");
}

#[test]
fn skos_xl_labels_count_for_s13_and_s14() {
    // Examples 84 to 87 of SKOS-XL
    breaks(
        r#"<Love> skosxl:prefLabel <A> ; skosxl:prefLabel <B> .
        <A> skosxl:literalForm "love"@en . <B> skosxl:literalForm "adoration"@en ."#,
        "S14",
    );
    breaks(
        r#"<Love> skosxl:prefLabel <A> ; skosxl:altLabel <B> .
        <A> skosxl:literalForm "love"@en . <B> skosxl:literalForm "love"@en ."#,
        "S13 with S55 to S57",
    );
    breaks(
        r#"<Love> skosxl:altLabel <A> ; skosxl:hiddenLabel <B> .
        <A> skosxl:literalForm "love"@en . <B> skosxl:literalForm "love"@en ."#,
        "S13 with S55 to S57",
    );
    breaks(
        r#"<Love> skosxl:prefLabel <A> ; skosxl:hiddenLabel <B> .
        <A> skosxl:literalForm "love"@en . <B> skosxl:literalForm "love"@en ."#,
        "S13 with S55 to S57",
    );
    // a plain and an XL preferred label in one language
    breaks(
        r#"<Love> skos:prefLabel "love"@en ; skosxl:prefLabel <A> .
        <A> skosxl:literalForm "adoration"@en ."#,
        "S14",
    );
    // the same label kept in both forms is consistent, as S55 makes it
    consistent(
        r#"<Love> skos:prefLabel "love"@en ; skosxl:prefLabel <A> ; skosxl:altLabel <B> .
        <A> a skosxl:Label ; skosxl:literalForm "love"@en .
        <B> a skosxl:Label ; skosxl:literalForm "adoration"@en ."#,
    );
}

#[test]
fn skos_xl_labels_have_one_plain_literal_form() {
    breaks("<Love> skosxl:prefLabel <A> . <A> a skosxl:Label .", "S52");
    breaks(
        r#"<A> a skosxl:Label ; skosxl:literalForm "love"@en , "amour"@fr ."#,
        "S52",
    );
    breaks("<A> a skosxl:Label ; skosxl:literalForm 42 .", "S51");
    // S58: one label cannot be both preferred and alternative
    breaks(
        r#"<Love> skosxl:prefLabel <A> ; skosxl:altLabel <A> . <A> skosxl:literalForm "love"@en ."#,
        "S58",
    );
}

#[test]
fn an_untyped_linked_concept_is_information_only() {
    // the concept B has no type: a write guard with the default threshold lets it through
    consistent("<A> a skos:Concept ; skos:broader <B> .");
    let ds = Dataset::memory();
    ds.load_str(
        &format!("{PREFIXES}<A> a skos:Concept ; skos:broader <B> ."),
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    let report = ds
        .validation()
        .shacl(&sparkles::skos::shapes(), &ValidateOptions::default())
        .unwrap();
    assert!(
        report
            .results
            .iter()
            .any(|r| r.severity.as_str().ends_with("#Info"))
    );
}

fn skos_settings() -> FormSettings {
    FormSettings {
        shapes: ShapesSource::Skos,
        ..Default::default()
    }
}

#[test]
fn forms_read_the_built_in_shapes_with_shapes_skos() {
    let ds = Dataset::memory();
    ds.load_str(
        &format!(
            r#"{PREFIXES}
            <S> a skos:ConceptScheme ; skos:prefLabel "Animals"@en .
            <A> a skos:Concept ; skos:prefLabel "Mammals"@en ; skos:inScheme <S> .
            <B> a skos:Concept ; skos:prefLabel "Birds"@en ; skos:inScheme <S> .
            <R> a skos:OrderedCollection ; skos:memberList ( <A> <B> ) ."#
        ),
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    let m = ds
        .forms()
        .model(
            &FormRequest::new("http://ex/A", skos_settings()),
            &Default::default(),
        )
        .unwrap();
    let ids: Vec<&str> = m.fields.iter().map(|f| f.id.as_str()).collect();
    for p in [
        "prefLabel",
        "altLabel",
        "definition",
        "scopeNote",
        "broader",
        "exactMatch",
    ] {
        let id = format!("<http://www.w3.org/2004/02/skos/core#{p}>");
        assert!(ids.contains(&id.as_str()), "{id} missing from {ids:?}");
    }
    let def = m
        .fields
        .iter()
        .find(|f| f.id.ends_with("#definition>"))
        .unwrap();
    assert_eq!(def.group.as_deref(), Some("urn:x-sparkles:skos:Notes"));

    // the ordered collection's memberList is a list field
    let m = ds
        .forms()
        .model(
            &FormRequest::new("http://ex/R", skos_settings()),
            &Default::default(),
        )
        .unwrap();
    let list = m
        .fields
        .iter()
        .find(|f| f.id.ends_with("#memberList>"))
        .unwrap();
    assert_eq!(list.widget, "list");

    // a note saved through the form, checked against the built-in shapes
    let c = FormChange {
        focus: "http://ex/A".into(),
        changes: vec![FieldChange {
            field: "<http://www.w3.org/2004/02/skos/core#definition>".into(),
            add: vec![Term::Literal(
                oxrdf::Literal::new_language_tagged_literal_unchecked(
                    "Animals that nurse their young.",
                    "en",
                ),
            )],
            ..Default::default()
        }],
        expect: std::collections::BTreeMap::from([(
            "<http://www.w3.org/2004/02/skos/core#definition>".to_string(),
            vec![],
        )]),
        settings: skos_settings(),
        ..Default::default()
    };
    match ds.forms().save(&c, &Default::default()).unwrap() {
        FormSaved::Saved(_) => {}
        FormSaved::Conflict(c) => panic!("conflict {c:?}"),
    }
    assert!(
        ds.ask(
            r#"ASK { <http://ex/A> <http://www.w3.org/2004/02/skos/core#definition> "Animals that nurse their young."@en }"#
        )
        .unwrap()
    );
}

/// Form checks find the resources an edit affects from each shape's reads, which needs every
/// SPARQL constraint to be one path or pattern read from the focus. A check on a concept in
/// related links, mappings and SKOS-XL labels runs, and reports the clashes an edit makes.
#[test]
fn form_checks_admit_every_built_in_constraint() {
    let ds = Dataset::memory();
    ds.load_str(
        &format!(
            r#"{PREFIXES}
            <A> a skos:Concept ; skos:prefLabel "Mammals"@en ; skos:broader <V> ;
              skos:related <B> ; skos:exactMatch <M> ; skosxl:altLabel <L> .
            <L> a skosxl:Label ; skosxl:literalForm "Mammalia"@la .
            <B> a skos:Concept ; skos:prefLabel "Birds"@en .
            <V> a skos:Concept ; skos:prefLabel "Vertebrates"@en ."#
        ),
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    let change = |field: &str, value: Term, expect: Vec<Term>| FormChange {
        focus: "http://ex/A".into(),
        changes: vec![FieldChange {
            field: field.into(),
            add: vec![value],
            ..Default::default()
        }],
        expect: std::collections::BTreeMap::from([(field.to_string(), expect)]),
        settings: skos_settings(),
        ..Default::default()
    };
    let lit = |v: &str, l: &str| {
        Term::Literal(oxrdf::Literal::new_language_tagged_literal_unchecked(v, l))
    };
    let pref = "<http://www.w3.org/2004/02/skos/core#prefLabel>";
    let messages =
        |c: &sparkles::forms::FormCheck| -> String { serde_json::to_string(&c.results).unwrap() };

    let c = ds
        .forms()
        .check(
            &change(
                "<http://www.w3.org/2004/02/skos/core#definition>",
                lit("Animals that nurse their young.", "en"),
                vec![],
            ),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(c.conforms, Some(true), "{}", messages(&c));

    // a second English preferred label: S14
    let c = ds
        .forms()
        .check(
            &change(pref, lit("Mammalia", "en"), vec![lit("Mammals", "en")]),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(c.conforms, Some(false));
    assert!(messages(&c).contains("S14"), "{}", messages(&c));

    // the SKOS-XL alternative label's form as a preferred label: S13 with S55 to S57
    let c = ds
        .forms()
        .check(
            &change(pref, lit("Mammalia", "la"), vec![lit("Mammals", "en")]),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(c.conforms, Some(false));
    assert!(messages(&c).contains("S13 with S55"), "{}", messages(&c));

    // the related concept made broader: S27
    let c = ds
        .forms()
        .check(
            &change(
                "<http://www.w3.org/2004/02/skos/core#broader>",
                Term::NamedNode(oxrdf::NamedNode::new_unchecked("http://ex/B")),
                vec![Term::NamedNode(oxrdf::NamedNode::new_unchecked(
                    "http://ex/V",
                ))],
            ),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(c.conforms, Some(false));
    assert!(messages(&c).contains("S27"), "{}", messages(&c));

    // the exact match made a broad match too: S46
    let c = ds
        .forms()
        .check(
            &change(
                "<http://www.w3.org/2004/02/skos/core#broadMatch>",
                Term::NamedNode(oxrdf::NamedNode::new_unchecked("http://ex/M")),
                vec![],
            ),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(c.conforms, Some(false));
    assert!(messages(&c).contains("S46"), "{}", messages(&c));
}

#[test]
fn stored_forms_settings_refuse_the_built_in_shapes() {
    let e = skos_settings().validate().unwrap_err();
    assert!(e.contains("per request"), "{e}");
}
