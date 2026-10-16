//! Shapes drafted from the data (`sparkles_core::schema::draft`) checked by validating them:
//! at support 1 the data conforms, and below 1 each drafted constraint excludes exactly
//! the focus nodes the validator reports for it.

use sparkles_core::io::RdfFormat;
use sparkles_core::schema::draft::{ConstraintDraft, DraftOptions, ShapesDraft};
use sparkles_core::schema::{GraphSelection, draft_shapes};
use sparkles_core::store::Store;
use sparkles_shacl::{Shapes, ValidateOptions, ValidationReport, validate};
use std::collections::{BTreeMap, BTreeSet};

#[path = "draft_fixture/mod.rs"]
mod draft_fixture;
use draft_fixture::{EX, extended, fixture, random_data, store};

pub fn draft(s: &Store, support: f64, closed: bool, classes: &[&str]) -> ShapesDraft {
    let mut o = DraftOptions {
        dataset: "t".into(),
        support,
        closed,
        classes: classes.iter().map(|c| format!("{EX}{c}")).collect(),
        prefixes: vec![("ex".into(), EX.into())],
        ..Default::default()
    };
    o.schema.graph = GraphSelection::Default;
    draft_shapes(&s.snapshot(), &o).unwrap_or_else(|e| panic!("{e}"))
}

fn canonical(mut g: oxrdf::Graph) -> oxrdf::Graph {
    use oxrdf::dataset::{CanonicalizationAlgorithm, CanonicalizationHashAlgorithm};
    g.canonicalize(CanonicalizationAlgorithm::Rdfc10 {
        hash_algorithm: CanonicalizationHashAlgorithm::Sha256,
    });
    g
}

fn check(s: &Store, d: &ShapesDraft) -> ValidationReport {
    let shapes = Shapes::parse(&d.shacl, RdfFormat::Turtle, None)
        .unwrap_or_else(|e| panic!("{e:#}\n{}", d.shacl));
    // the SHACLC draft is the same shapes graph
    let turtle = Shapes::read_graph(&d.shacl, RdfFormat::Turtle, None).unwrap();
    let compact = sparkles_shacl::compact::parse(&d.shaclc, None)
        .unwrap_or_else(|e| panic!("{e}\n{}", d.shaclc));
    assert!(
        canonical(turtle) == canonical(compact.graph),
        "the SHACLC draft differs from the Turtle draft\n{}\n{}",
        d.shacl,
        d.shaclc
    );
    validate(&s.snapshot(), &shapes, &ValidateOptions::default()).unwrap()
}

fn component_iri(c: &str) -> String {
    let mut s = c.to_string();
    s[..1].make_ascii_uppercase();
    format!("http://www.w3.org/ns/shacl#{s}ConstraintComponent")
}

fn constraint<'a>(d: &'a ShapesDraft, class: &str, path: &str, c: &str) -> &'a ConstraintDraft {
    d.shapes
        .iter()
        .find(|s| s.class == format!("{EX}{class}"))
        .and_then(|s| {
            s.properties
                .iter()
                .find(|p| p.path == path && !p.inverse && p.then.is_none())
        })
        .and_then(|p| p.constraints.iter().find(|x| x.component == c))
        .unwrap_or_else(|| panic!("no {c} on {path} of {class}:\n{}", d.shacl))
}

fn rejected<'a>(d: &'a ShapesDraft, class: &str, path: &str, c: &str) -> &'a ConstraintDraft {
    d.shapes
        .iter()
        .find(|s| s.class == format!("{EX}{class}"))
        .and_then(|s| {
            s.properties
                .iter()
                .find(|p| p.path == path && !p.inverse && p.then.is_none())
        })
        .and_then(|p| p.rejected.iter().find(|x| x.component == c))
        .unwrap_or_else(|| panic!("no rejected {c} on {path} of {class}:\n{}", d.shacl))
}

#[test]
fn drafts_at_full_support_conform() {
    let s = store(&fixture());
    for closed in [false, true] {
        let d = draft(&s, 1.0, closed, &[]);
        let r = check(&s, &d);
        assert!(
            r.conforms,
            "closed={closed}: {:#?}\n{}",
            r.results.iter().take(5).collect::<Vec<_>>(),
            d.shacl
        );
        // every class outside the built-in namespaces has a shape
        let classes: BTreeSet<&str> = d.shapes.iter().map(|s| s.class.as_str()).collect();
        for c in ["Person", "Employee", "Manager", "Org", "Doc", "Agent"] {
            assert!(classes.contains(format!("{EX}{c}").as_str()), "{c}");
        }
    }
}

#[test]
fn instances_include_subclasses() {
    let s = store(&fixture());
    let d = draft(&s, 1.0, false, &[]);
    let n = |c: &str| {
        d.shapes
            .iter()
            .find(|s| s.class == format!("{EX}{c}"))
            .unwrap()
            .instances
    };
    // 20 people, 6 anonymous people, 6 employees and a manager
    assert_eq!(n("Person"), 33);
    assert_eq!(n("Employee"), 7);
    assert_eq!(n("Manager"), 1);
}

#[test]
fn constraints_and_counts() {
    let s = store(&fixture());
    let d = draft(&s, 1.0, false, &[]);
    let name = format!("{EX}name");
    let c = constraint(&d, "Person", &name, "minCount");
    assert_eq!((c.applicable, c.satisfied, c.excluded), (33, 33, 0));
    // p5 has two names
    let r = rejected(&d, "Person", &name, "maxCount");
    assert_eq!((r.applicable, r.satisfied, r.excluded), (33, 32, 1));
    let c = constraint(&d, "Person", &name, "datatype");
    assert_eq!(c.satisfied, 33);
    // the misspelt status keeps sh:in out at support 1
    let status = format!("{EX}status");
    let r = rejected(&d, "Person", &status, "in");
    assert_eq!((r.applicable, r.satisfied, r.excluded), (33, 32, 1));
    // ages: one ill-formed integer
    let age = format!("{EX}age");
    let r = rejected(&d, "Person", &age, "datatype");
    assert_eq!(r.excluded, 1);
    // every person knows a person (blank ones too)
    let knows = format!("{EX}knows");
    let c = constraint(&d, "Person", &knows, "class");
    assert_eq!(
        c.value,
        sparkles_core::schema::draft::ConstraintValue::Iri(format!("{EX}Person"))
    );
    let c = constraint(&d, "Person", &knows, "nodeKind");
    assert_eq!(
        c.value,
        sparkles_core::schema::draft::ConstraintValue::Iri(
            "http://www.w3.org/ns/shacl#BlankNodeOrIRI".into()
        )
    );
    // labels: two languages; p9 has two English labels
    let label = "http://www.w3.org/2000/01/rdf-schema#label";
    let c = constraint(&d, "Person", label, "languageIn");
    assert_eq!(c.satisfied, 20);
    let r = rejected(&d, "Person", label, "uniqueLang");
    assert_eq!(r.excluded, 1);
    // flags are booleans: no sh:in
    let flag = format!("{EX}flag");
    assert!(
        d.shapes
            .iter()
            .flat_map(|s| &s.properties)
            .filter(|p| p.path == flag)
            .all(|p| p
                .constraints
                .iter()
                .chain(&p.rejected)
                .all(|c| c.component != "in"))
    );
    // a named graph is not part of the default graph
    assert!(!d.shacl.contains("secret"), "{}", d.shacl);
}

/// Below support 1, each drafted constraint excludes exactly the focus nodes that
/// validation reports for its path and component.
#[test]
fn excluded_counts_match_validation() {
    let s = store(&fixture());
    for support in [0.9, 0.75, 0.5] {
        for class in ["Person", "Employee", "Org", "Doc", "Manager"] {
            let d = draft(&s, support, false, &[class]);
            assert_eq!(d.shapes.len(), 1);
            let r = check(&s, &d);
            assert_eq!(
                reported(&r),
                promised(&d),
                "{class} at support {support}:\n{}",
                d.shacl
            );
        }
    }
}

#[test]
fn lower_support_drafts_the_enumeration() {
    let s = store(&fixture());
    let d = draft(&s, 0.9, false, &["Person"]);
    let c = constraint(&d, "Person", &format!("{EX}status"), "in");
    assert_eq!((c.applicable, c.satisfied, c.excluded), (33, 32, 1));
    let r = check(&s, &d);
    let bad: BTreeSet<String> = r
        .results
        .iter()
        .filter(|x| {
            x.source_constraint_component
                .as_str()
                .ends_with("InConstraintComponent")
        })
        .map(|x| x.focus_node.to_string())
        .collect();
    assert_eq!(bad, BTreeSet::from([format!("<{EX}p7>")]));
}

#[test]
fn graph_views_limit_the_draft() {
    let s = store(&fixture());
    let mut o = DraftOptions {
        dataset: "t".into(),
        ..Default::default()
    };
    o.schema.graph = GraphSelection::Union;
    let all = draft_shapes(&s.snapshot(), &o).unwrap();
    assert!(all.shacl.contains("secret"), "{}", all.shacl);
    let only_default = sparkles_core::access::Graphs::Only(sparkles_core::access::GraphRule::new(
        ["default"],
        &[],
    ));
    o.schema.graphs = Some(std::sync::Arc::new(sparkles_core::access::GraphAccess {
        read: only_default.clone(),
        write: only_default,
        triples: None,
    }));
    let view = draft_shapes(&s.snapshot(), &o).unwrap();
    assert!(!view.shacl.contains("secret"), "{}", view.shacl);
}

/// Distinct focus nodes per (path, component) of a report, and the exclusions a draft
/// of one class promises for them.
type Counts = BTreeMap<(String, String), usize>;

fn reported(r: &ValidationReport) -> Counts {
    let mut focus: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for x in &r.results {
        let path = x
            .result_path
            .as_ref()
            .map(|p| p.to_string())
            .unwrap_or_default();
        focus
            .entry((path, x.source_constraint_component.as_str().to_string()))
            .or_default()
            .insert(x.focus_node.to_string());
    }
    focus.into_iter().map(|(k, v)| (k, v.len())).collect()
}

fn promised(d: &ShapesDraft) -> Counts {
    let mut expected = Counts::new();
    for p in &d.shapes[0].properties {
        let path = match (&p.then, p.inverse) {
            (Some(q), _) => format!("<{}>/<{}>", p.path, q),
            (None, true) => format!("^<{}>", p.path),
            (None, false) => format!("<{}>", p.path),
        };
        for c in p.constraints.iter().filter(|c| c.excluded > 0) {
            expected.insert(
                (path.clone(), component_iri(c.component)),
                c.excluded as usize,
            );
        }
    }
    expected
}

#[test]
fn random_drafts_conform_and_count_exactly() {
    for seed in 1..=60u64 {
        let data = random_data(seed);
        let s = store(&data);
        for closed in [false, true] {
            let d = draft(&s, 1.0, closed, &[]);
            let r = check(&s, &d);
            assert!(
                r.conforms,
                "seed {seed} closed={closed}: {:#?}\n{}\n{data}",
                r.results.iter().take(3).collect::<Vec<_>>(),
                d.shacl
            );
        }
        for support in [0.95, 0.8, 0.6] {
            for class in ["C0", "C1", "C2", "C3"] {
                let d = draft(&s, support, false, &[class]);
                let r = check(&s, &d);
                assert_eq!(
                    reported(&r),
                    promised(&d),
                    "seed {seed} {class} at {support}:\n{}\n{data}",
                    d.shacl
                );
            }
        }
    }
}

fn property<'a>(
    d: &'a ShapesDraft,
    class: &str,
    path: &str,
    inverse: bool,
    then: Option<&str>,
) -> &'a sparkles_core::schema::draft::PropertyDraft {
    d.shapes
        .iter()
        .find(|s| s.class == format!("{EX}{class}"))
        .and_then(|s| {
            s.properties.iter().find(|p| {
                p.path == format!("{EX}{path}")
                    && p.inverse == inverse
                    && p.then.as_deref() == then.map(|t| format!("{EX}{t}")).as_deref()
            })
        })
        .unwrap_or_else(|| {
            panic!(
                "no property {path} (inverse {inverse}, then {then:?}) of {class}:\n{}",
                d.shacl
            )
        })
}

/// Datatype alternatives, inverse paths, a sequence through a blank node, patterns,
/// ranges and references to the shape of a class, all of which the data conforms to at
/// full support.
#[test]
fn richer_drafts_at_full_support_conform() {
    let s = store(&extended());
    for closed in [false, true] {
        let d = draft(&s, 1.0, closed, &[]);
        let r = check(&s, &d);
        assert!(
            r.conforms,
            "closed={closed}: {:#?}\n{}",
            r.results.iter().take(5).collect::<Vec<_>>(),
            d.shacl
        );
    }
    let d = draft(&s, 1.0, false, &[]);
    // integers and decimals together
    let c = constraint(&d, "Doc", &format!("{EX}rating"), "or");
    assert_eq!(
        c.value,
        sparkles_core::schema::draft::ConstraintValue::List(vec![
            "http://www.w3.org/2001/XMLSchema#decimal".into(),
            "http://www.w3.org/2001/XMLSchema#integer".into()
        ])
    );
    assert_eq!((c.applicable, c.satisfied), (4, 4));
    assert!(
        d.shacl
            .contains("sh:or ( [ sh:datatype xsd:decimal ] [ sh:datatype xsd:integer ] )"),
        "{}",
        d.shacl
    );
    // the address blank node has no class, so its city is drafted as a path
    let seq = property(&d, "Org", "address", false, Some("city"));
    assert_eq!(seq.instances, 5);
    assert!(
        d.shacl.contains("sh:path ( ex:address ex:city )"),
        "{}",
        d.shacl
    );
    assert!(d.shaclc.contains("ex:address/ex:city"), "{}", d.shaclc);
    // the teams are pointed at by every organisation
    let inv = property(&d, "Team", "team", true, None);
    assert_eq!(inv.instances, 2);
    assert!(
        d.shacl.contains("sh:path [ sh:inversePath ex:team ]"),
        "{}",
        d.shacl
    );
    assert!(d.shaclc.contains("^ex:team"), "{}", d.shaclc);
    // the class of the values has a shape, and the property refers to it
    let node = constraint(&d, "Org", &format!("{EX}team"), "node");
    let team = d
        .shapes
        .iter()
        .find(|s| s.class == format!("{EX}Team"))
        .unwrap();
    assert_eq!(
        node.value,
        sparkles_core::schema::draft::ConstraintValue::Iri(team.shape.clone())
    );
    assert!(d.shacl.contains("sh:node shape:TeamShape"), "{}", d.shacl);
    // a code of two letters and four digits, and the observed weights
    let pattern = constraint(&d, "Team", &format!("{EX}code"), "pattern");
    assert_eq!(
        pattern.value,
        sparkles_core::schema::draft::ConstraintValue::Text("^[A-Z]{2}[0-9]{4}$".into())
    );
    let min = constraint(&d, "Org", &format!("{EX}weight"), "minInclusive");
    assert_eq!((min.satisfied, min.excluded), (5, 0));
    assert!(
        d.shacl.contains("sh:minInclusive \"10\"^^xsd:integer"),
        "{}",
        d.shacl
    );
    assert!(
        d.shacl.contains("sh:maxInclusive \"30\"^^xsd:integer"),
        "{}",
        d.shacl
    );
}

/// Below full support, the richer constraints exclude exactly the focus nodes that
/// validation reports.
#[test]
fn richer_excluded_counts_match_validation() {
    let mut data = extended();
    // a rating that is a string, a code and a weight that break the rule, an organisation
    // without a team
    data.push_str("ex:d1 ex:rating \"high\" .\nex:t0 ex:code \"xyz\" .\nex:o3 ex:weight 500 .\nex:o9 a ex:Org ; ex:weight 12 .\nex:t2 a ex:Team ; ex:code \"EF9012\" .\nex:t3 a ex:Team ; ex:code \"GH3456\" .\n");
    let s = store(&data);
    let mut seen = BTreeSet::new();
    for support in [0.95, 0.8, 0.6] {
        for class in ["Doc", "Team", "Org"] {
            let d = draft(&s, support, false, &[class]);
            let r = check(&s, &d);
            assert_eq!(
                reported(&r),
                promised(&d),
                "{class} at support {support}:\n{}",
                d.shacl
            );
            seen.extend(promised(&d).into_keys().map(|(path, c)| {
                let kind = if path.starts_with('^') {
                    "inverse "
                } else if path.contains(">/<") {
                    "sequence "
                } else {
                    ""
                };
                format!("{kind}{}", c.rsplit('#').next().unwrap_or(&c))
            }));
        }
    }
    // the draft excluded something with each new kind of constraint
    for kind in [
        "OrConstraintComponent",
        "PatternConstraintComponent",
        "MinInclusiveConstraintComponent",
        "MaxInclusiveConstraintComponent",
    ] {
        assert!(seen.contains(kind), "{kind} in {seen:?}");
    }
}

/// The references to other shapes are drafted at full support only: below it, a value
/// the shape of its class excludes would exclude the instance that has it, and the draft
/// could not count those.
#[test]
fn references_need_full_support() {
    let s = store(&extended());
    let d = draft(&s, 0.9, false, &[]);
    assert!(
        d.shapes
            .iter()
            .flat_map(|s| &s.properties)
            .all(|p| p.constraints.iter().all(|c| c.component != "node"))
    );
}

/// A `p` value that a drafted class describes is left out of the `p/q` profile, yet a
/// drafted `p/q` shape would reach it, so the source data would fail the support-1 draft.
#[test]
fn sequence_paths_skip_partly_typed_intermediate_nodes() {
    let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..3 {
        data.push_str(&format!(
            "ex:x{i} a ex:X ; ex:p [ ex:q \"a{i}\" ] , [ a ex:Y ; ex:q \"b{i}\" ] .\n"
        ));
    }
    let s = store(&data);
    let d = draft(&s, 1.0, false, &[]);
    let r = check(&s, &d);
    assert!(r.conforms, "{}\n{r}", d.shacl);
    let x = d
        .shapes
        .iter()
        .find(|s| s.class == format!("{EX}X"))
        .expect("a shape for ex:X");
    assert!(
        x.properties
            .iter()
            .all(|p| p.then.as_deref() != Some("http://ex.org/q")),
        "{}",
        d.shacl
    );
}
