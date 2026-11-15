use super::*;

const EX: &str = "http://ex.org/";

fn tbox(ttl: &str) -> Tbox {
    let triples: Vec<oxrdf::Triple> = oxttl::TurtleParser::new()
        .for_slice(ttl.as_bytes())
        .map(|t| t.unwrap())
        .collect();
    Tbox::from_triples(triples)
}

fn parse(q: &str) -> Query {
    spargebra::SparqlParser::new().parse_query(q).unwrap()
}

fn rewritten(ttl: &str, q: &str, max: usize) -> Rewritten {
    let t = tbox(&format!(
        "@prefix ex: <{EX}> . @prefix owl: <http://www.w3.org/2002/07/owl#> . \
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . \
         ex:teaches a owl:ObjectProperty . ex:advises a owl:ObjectProperty . \
         ex:taughtBy a owl:ObjectProperty . ex:lectures a owl:ObjectProperty . \
         ex:headOf a owl:ObjectProperty . {ttl}"
    ));
    rewrite_query(
        &parse(&format!("PREFIX ex: <{EX}> {q}")),
        &[],
        &t,
        max,
        None,
        &|| Ok(()),
    )
    .unwrap()
    .unwrap()
}

fn branches(r: &Rewritten) -> Vec<u64> {
    r.nodes
        .iter()
        .filter_map(|n| n.counters.as_ref()?.get("branches")?.as_u64())
        .collect()
}

#[test]
fn a_class_takes_its_subclasses_and_the_domains_and_ranges_that_imply_it() {
    let r = rewritten(
        "ex:Professor rdfs:subClassOf ex:Person . ex:teaches rdfs:domain ex:Person . \
         ex:advises rdfs:range ex:Person .",
        "SELECT ?x { ?x a ex:Person }",
        1024,
    );
    // Person(x), Professor(x), teaches(x, _), advises(_, x)
    assert_eq!(branches(&r), vec![4]);
    assert!(r.derive.is_empty());
}

#[test]
fn an_existential_answers_an_undistinguished_variable_only() {
    let onto = "ex:Professor rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:teaches ; \
                owl:someValuesFrom owl:Thing ] .";
    // ASK: ?c is not distinguished, so Professor(ann) implies teaches(ann, _)
    let r = rewritten(onto, "ASK { ex:ann ex:teaches ?c }", 1024);
    assert_eq!(branches(&r), vec![2]);
    // SELECT ?c: ?c is distinguished, and no inclusion applies to teaches(ann, ?c)
    let r = rewritten(onto, "SELECT ?c { ex:ann ex:teaches ?c }", 1024);
    assert!(branches(&r).is_empty());
    // a filter makes ?c distinguished too
    let r = rewritten(
        onto,
        "SELECT ?x { ?x ex:teaches ?c FILTER(?c != ex:a) }",
        1024,
    );
    assert!(branches(&r).is_empty());
    // COUNT(*) counts every solution, so every variable is distinguished
    let r = rewritten(onto, "SELECT (COUNT(*) AS ?n) { ?x ex:teaches ?c }", 1024);
    assert!(branches(&r).is_empty());
}

#[test]
fn a_qualified_existential_joins_through_its_filler_and_drops_the_fresh_property() {
    let r = rewritten(
        "ex:Professor rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:teaches ; \
         owl:someValuesFrom ex:Course ] .",
        "SELECT ?x { ?x ex:teaches ?c . ?c a ex:Course }",
        1024,
    );
    // teaches(x, c) ∧ Course(c), and Professor(x) through the fresh property
    assert_eq!(branches(&r), vec![2]);
    let sse = r.query.to_sse();
    assert!(sse.contains("<http://ex.org/Professor>"), "{sse}");
}

#[test]
fn inverse_and_sub_properties_swap_and_rename_positions() {
    let r = rewritten(
        "ex:taughtBy owl:inverseOf ex:teaches . ex:lectures rdfs:subPropertyOf ex:teaches .",
        "SELECT ?x ?c { ?x ex:teaches ?c }",
        1024,
    );
    assert_eq!(branches(&r), vec![3]);
    let sse = r.query.to_sse();
    assert!(
        sse.contains("(triple ?c <http://ex.org/taughtBy> ?x)"),
        "{sse}"
    );
}

#[test]
fn a_rewriting_past_the_bound_is_left_for_derivation() {
    let mut onto = String::new();
    for i in 0..40 {
        onto.push_str(&format!(
            "ex:C{i} rdfs:subClassOf ex:A . ex:D{i} rdfs:subClassOf ex:B . "
        ));
    }
    let r = rewritten(&onto, "SELECT ?x ?y { ?x a ex:A . ?y a ex:B }", 100);
    assert!(branches(&r).is_empty());
    assert_eq!(r.derive.len(), 1);
    assert!(
        r.warnings.iter().any(|w| w.contains("budget.maxUnion")),
        "{:?}",
        r.warnings
    );
    // under the bound it is rewritten: 41 × 41 branches
    let r = rewritten(&onto, "SELECT ?x ?y { ?x a ex:A . ?y a ex:B }", 2_000);
    assert_eq!(branches(&r), vec![41 * 41]);
}

#[test]
fn patterns_outside_the_procedure_are_left_for_derivation() {
    let onto = "ex:Professor rdfs:subClassOf ex:Person .";
    for q in [
        "SELECT ?x ?p { ?x ?p ex:Person }",
        "SELECT ?x ?c { ?x a ?c }",
        "SELECT ?x ?y { ?x rdfs:subClassOf ?y }",
        "SELECT ?x { ?x a owl:Thing }",
    ] {
        let q = format!(
            "PREFIX owl: <http://www.w3.org/2002/07/owl#> \
             PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> {q}"
        );
        let r = rewritten(onto, &q, 1024);
        assert_eq!(r.derive.len(), 1, "{q}");
    }
    // a pattern no inclusion touches stays as written and is not derived
    let r = rewritten(onto, "SELECT ?x ?y { ?x ex:knows ?y }", 1024);
    assert!(r.derive.is_empty() && branches(&r).is_empty());
    assert!(
        r.query
            .to_sse()
            .contains("(bgp (triple ?x <http://ex.org/knows> ?y))")
    );
}

#[test]
fn axioms_outside_owl_2_ql_are_listed_and_ignored() {
    let r = rewritten(
        "ex:Chair owl:equivalentClass [ owl:intersectionOf ( ex:Person \
         [ a owl:Restriction ; owl:onProperty ex:headOf ; owl:someValuesFrom ex:Department ] ) ] . \
         ex:Professor rdfs:subClassOf ex:Person .",
        "SELECT ?x { ?x a ex:Person }",
        1024,
    );
    assert_eq!(branches(&r), vec![2]);
    assert!(
        r.warnings.iter().any(|w| w.contains("outside OWL 2 QL")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn unification_binds_distinguished_variables_again() {
    // R(x, y) ∧ R(x, z) with y, z distinguished reduce to one atom with z bound to y
    let t = tbox(&format!(
        "@prefix ex: <{EX}> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . \
         @prefix owl: <http://www.w3.org/2002/07/owl#> . ex:q a owl:ObjectProperty . \
         ex:r a owl:ObjectProperty . ex:q rdfs:subPropertyOf ex:r ."
    ));
    let q = parse(&format!(
        "PREFIX ex: <{EX}> SELECT ?y ?z {{ ex:a ex:r ?y . ex:a ex:r ?z }}"
    ));
    let r = rewrite_query(&q, &[], &t, 1024, None, &|| Ok(()))
        .unwrap()
        .unwrap();
    let sse = r.query.to_sse();
    assert!(
        sse.contains("(extend ((?z ?y))") || sse.contains("(extend ((?y ?z))"),
        "{sse}"
    );
}

#[test]
fn terminology_that_is_no_owl_axiom_is_reported() {
    // an undeclared property's subproperty triple maps to no OWL axiom
    let r = rewritten(
        "ex:q rdfs:subPropertyOf ex:r .",
        "SELECT ?x ?y { ?x ex:r ?y }",
        1024,
    );
    assert!(branches(&r).is_empty());
    assert!(
        r.warnings.iter().any(|w| w.contains("not OWL axioms")),
        "{:?}",
        r.warnings
    );
}

/// A rewriting that passes `budget.maxUnion` gives its memory back, so many such patterns
/// fall back under a budget that each fits.
#[test]
fn an_abandoned_rewriting_releases_its_memory() {
    let mut onto = String::from(
        "@prefix ex: <http://ex.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> . \
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . ex:A a owl:Class . ",
    );
    for i in 0..12 {
        onto.push_str(&format!("ex:C{i} a owl:Class ; rdfs:subClassOf ex:A . "));
    }
    let t = tbox(&onto);
    let groups: Vec<String> = (0..20)
        .map(|i| format!("{{ ?x{i} a ex:A . ?y{i} a ex:A }}"))
        .collect();
    let q = parse(&format!(
        "PREFIX ex: <{EX}> SELECT * {{ {} }}",
        groups.join(" UNION ")
    ));
    let r = rewrite_query(&q, &[], &t, 50, Some(64 * 1024), &|| Ok(()))
        .unwrap()
        .unwrap();
    assert_eq!(r.derive.len(), 20);
}
