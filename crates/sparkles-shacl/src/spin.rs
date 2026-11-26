//! SPIN rules as SHACL-AF SPARQL rules.
//!
//! A SPIN rule is a SPARQL CONSTRUCT attached to a class with `spin:rule`, or with a
//! subproperty of it, and SPIN runs it with `?this` bound to each instance of the class.
//! [`translate`] writes each one as a `sh:SPARQLRule` of a new node shape whose target is
//! that class, which is the same evaluation. A rule that does not mention `?this` runs
//! once in SPIN, so its shape targets the class node alone and the query runs once.
//!
//! Queries are read from SPIN RDF syntax (`sp:Construct`, `sp:where`, `sp:templates`, and
//! all SPARQL 1.1 graph pattern, property path, expression, built-in and aggregate nodes),
//! from SPIN template calls (`spin:Template`, `spin:ConstructTemplate`, `spin:body`,
//! `spl:Argument` with defaults, optional arguments, value types and `rdfs:subClassOf`
//! inheritance), or from `sp:text`. Malformed, cyclic or over-budget SPIN graphs are
//! refused cleanly. SPIN's rule properties all run in one group, which iterates to a fixed
//! point as TopBraid's SPIN inferencing does.

mod rdf;
mod template;
mod vocab;

use crate::shapes::G;
use crate::vocab::{rdf as rdf_vocab, sh};
use anyhow::{Context as _, Result, bail};
use oxrdf::{BlankNode, Graph, Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::Query;

pub use vocab::{SP_NS, SPIN_NS, SPL_NS};

/// The prefixes a SPIN query may use without declaring them: those of RDF, RDFS, OWL,
/// XSD, SHACL and SPIN's own vocabularies.
pub const STANDARD_PREFIXES: &[(&str, &str)] = &[
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("sh", "http://www.w3.org/ns/shacl#"),
    ("sp", SP_NS),
    ("spin", SPIN_NS),
    ("spl", SPL_NS),
];

/// Whether a graph holds SPIN rules.
pub fn has_rules(graph: &Graph) -> bool {
    !rule_properties(graph).is_empty()
}

/// `spin:rule` and its subproperties (`rdfs:subPropertyOf*`).
fn rule_properties(graph: &Graph) -> Vec<NamedNode> {
    let mut out = vec![vocab::spin::RULE.into_owned()];
    let mut i = 0;
    while i < out.len() {
        let p = Term::NamedNode(out[i].clone());
        for t in graph.subjects_for_predicate_object(oxrdf::vocab::rdfs::SUB_PROPERTY_OF, &p) {
            if let NamedOrBlankNode::NamedNode(n) = t.into_owned()
                && !out.contains(&n)
            {
                out.push(n);
            }
        }
        i += 1;
    }
    out.retain(|p| graph.triples_for_predicate(p).next().is_some());
    out
}

/// Serialize a SPIN query node (in SPIN RDF syntax, as a SPIN template call, or via
/// `sp:text`) into SPARQL 1.1 query text.
pub fn serialize_query(graph: &Graph, query: &Term) -> Result<String> {
    let g = G { g: graph };
    query_to_sparql(&g, query)
}

fn query_to_sparql(g: &G<'_>, node: &Term) -> Result<String> {
    // 1. Template call (an instance of a spin:Template / spin:ConstructTemplate or subclass)
    if let Some(template) = template::template_of_call(g, node)? {
        return template::expand_template_call(g, node, &template);
    }
    // 2. SPIN RDF syntax (prefer structural RDF over sp:text so TopBraid files with full
    //    IRIs run even when Turtle prefix declarations are absent)
    if rdf::has_rdf_query_structure(g, node) {
        let empty = FxHashMap::default();
        return rdf::serialize_query_with_bindings(g, node, &empty);
    }
    // 3. Direct sp:text query
    let texts = g.objects(node, vocab::sp::TEXT);
    match texts.as_slice() {
        [Term::Literal(l)] => Ok(l.value().to_string()),
        [other] => bail!("the sp:text of the SPIN rule {node} is {other}, not a string"),
        [_, _, ..] => bail!("the SPIN rule {node} has multiple sp:text values"),
        [] => {
            let kind = g
                .object(node, rdf_vocab::TYPE)
                .map_or_else(|| "no type".to_string(), |t| t.to_string());
            bail!(
                "the SPIN rule {node} ({kind}) has neither SPIN RDF syntax, a SPIN template \
                 definition, nor sp:text"
            );
        }
    }
}

/// The SHACL-AF triples of the SPIN rules of `graph`: for each class with rules, a node
/// shape with the class as its target and one `sh:SPARQLRule` per rule. The queries get
/// the declarations of `prefixes` and of [`STANDARD_PREFIXES`] before their own text.
pub fn translate(graph: &Graph, prefixes: &[(String, String)]) -> Result<Graph> {
    let g = G { g: graph };
    let mut head = String::new();
    let mut declared = FxHashSet::default();
    for (p, ns) in prefixes
        .iter()
        .map(|(p, n)| (p.as_str(), n.as_str()))
        .chain(STANDARD_PREFIXES.iter().copied())
    {
        if declared.insert(p) {
            head.push_str(&format!("PREFIX {p}: <{ns}>\n"));
        }
    }
    let mut pairs: Vec<(Term, Term)> = Vec::new();
    for p in rule_properties(graph) {
        for t in graph.triples_for_predicate(&p) {
            pairs.push((Term::from(t.subject.into_owned()), t.object.into_owned()));
        }
    }
    pairs.sort_by_cached_key(|(c, r)| (c.to_string(), r.to_string()));
    pairs.dedup();
    let mut out = Graph::new();
    let mut n = 0usize;
    let mut fresh = || {
        n += 1;
        BlankNode::new_unchecked(format!("spinr{n}"))
    };
    for (class, rule) in pairs {
        let Term::NamedNode(class) = class else {
            bail!("the SPIN rule {rule} belongs to {class}, which is not an IRI");
        };
        let text = query_to_sparql(&g, &rule)
            .with_context(|| format!("translating SPIN rule {rule} of {class}"))?;
        let shape = fresh();
        let r = fresh();
        // a variable named this in the parsed query, not a word in a comment or a
        // longer variable name
        let full = format!("{head}{text}");
        let parsed = sparkles_core::sparql::parse_query(&full, None, &[])
            .map_err(|e| anyhow::anyhow!("{e}"))
            .map_err(|e| e.context(format!("the SPARQL query of the SPIN rule {rule}")))?;
        if !matches!(parsed, Query::Construct { .. }) {
            bail!("the SPIN rule {rule} of {class} must be a CONSTRUCT query");
        }
        let mentions_this = mentions_this(&parsed.to_string());
        let target = if mentions_this {
            sh::TARGET_CLASS
        } else {
            sh::TARGET_NODE
        };
        out.insert(&Triple::new(shape.clone(), rdf_vocab::TYPE, sh::NODE_SHAPE));
        out.insert(&Triple::new(shape.clone(), target, class.clone()));
        out.insert(&Triple::new(shape.clone(), sh::RULE, r.clone()));
        out.insert(&Triple::new(r.clone(), rdf_vocab::TYPE, sh::SPARQL_RULE));
        out.insert(&Triple::new(
            r,
            sh::CONSTRUCT,
            Literal::new_simple_literal(full),
        ));
    }
    Ok(out)
}

/// Whether the text of a parsed query, in which every variable is written `?name`, uses
/// the variable `this` outside its string literals and IRIs.
fn mentions_this(q: &str) -> bool {
    let b: Vec<char> = q.chars().collect();
    let word = |c: Option<&char>| c.is_some_and(|c| crate::sparql::varname_char(*c));
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < b.len() {
        match (quote, b[i]) {
            (Some(_), '\\') => i += 1,
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, c @ ('"' | '\'')) => quote = Some(c),
            // an IRI, which a comparison's `<` never starts, since it is followed by a
            // space or `=`
            (None, '<')
                if b.get(i + 1)
                    .is_some_and(|n| !n.is_whitespace() && *n != '=') =>
            {
                quote = Some('>');
            }
            (None, '?') if b[i + 1..].starts_with(&['t', 'h', 'i', 's']) && !word(b.get(i + 5)) => {
                return true;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{mentions_this, serialize_query, translate};
    use crate::shapes::Shapes;
    use crate::syntax::ShapesSyntax;
    use oxrdf::{BlankNode, Graph, Literal, NamedNode, Term, Triple};
    use sparkles_core::sparql::parse_query;

    const TTL_HEAD: &str = "
@prefix ex:   <http://example.org/> .
@prefix rdf:  <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd:  <http://www.w3.org/2001/XMLSchema#> .
@prefix sp:   <http://spinrdf.org/sp#> .
@prefix spin: <http://spinrdf.org/spin#> .
@prefix spl:  <http://spinrdf.org/spl#> .
";

    fn parse_ttl(body: &str) -> Graph {
        Shapes::read_graph(
            &format!("{TTL_HEAD}\n{body}"),
            ShapesSyntax::default(),
            None,
        )
        .unwrap()
    }

    fn ex(local: &str) -> Term {
        Term::NamedNode(NamedNode::new_unchecked(format!(
            "http://example.org/{local}"
        )))
    }

    #[test]
    fn only_the_variable_this_counts() {
        assert!(mentions_this(
            "CONSTRUCT { ?this ?p ?o } WHERE { ?this ?p ?o }"
        ));
        assert!(mentions_this("SELECT * WHERE { ?s ?p ?this . }"));
        assert!(!mentions_this(
            "CONSTRUCT { ?thisOne ?p ?o } WHERE { ?thisOne ?p ?o }"
        ));
        assert!(!mentions_this("SELECT * WHERE { ?this\u{B7} ?p ?o }"));
        assert!(!mentions_this("SELECT * WHERE { ?this\u{301} ?p ?o }"));
        assert!(!mentions_this("SELECT * WHERE { ?s ?p \"?this\" }"));
        assert!(!mentions_this("SELECT * WHERE { ?s <http://x/?this> ?o }"));
    }

    #[test]
    fn serialize_construct_with_expressions_filter_bind_optional_and_paths() {
        let g = parse_ttl(
            r#"
            ex:q a sp:Construct ;
              sp:templates (
                [ sp:subject spin:_this ; sp:predicate ex:area ; sp:object [ sp:varName "area" ] ]
                [ sp:subject spin:_this ; sp:predicate ex:tag ; sp:object _:tpl ]
                [ sp:subject _:tpl ; sp:predicate ex:label ; sp:object [ sp:varName "lbl" ] ]
              ) ;
              sp:where (
                [ sp:subject spin:_this ; sp:predicate ex:width ; sp:object [ sp:varName "w" ] ]
                [ a sp:TriplePath ;
                  sp:subject spin:_this ;
                  sp:path [ a sp:SeqPath ;
                            sp:path1 [ a sp:ReversePath ; sp:subPath ex:parent ] ;
                            sp:path2 [ a sp:ModPath ;
                                       sp:subPath [ a sp:AltPath ; sp:path1 ex:height ; sp:path2 ex:h ] ;
                                       sp:modMin 1 ; sp:modMax -2 ] ] ;
                  sp:object [ sp:varName "h" ] ]
                [ a sp:Bind ;
                  sp:expression [ a sp:mul ; sp:arg1 [ sp:varName "w" ] ; sp:arg2 [ sp:varName "h" ] ] ;
                  sp:variable [ sp:varName "area" ] ]
                [ a sp:Filter ;
                  sp:expression [ a sp:and ;
                                  sp:arg1 [ a sp:gt ; sp:arg1 [ sp:varName "area" ] ; sp:arg2 0 ] ;
                                  sp:arg2 [ a sp:not ; sp:arg1 [ a sp:eq ; sp:arg1 [ sp:varName "w" ] ; sp:arg2 0 ] ] ] ]
                [ a sp:Optional ;
                  sp:elements (
                    [ sp:subject spin:_this ; sp:predicate ex:name ; sp:object [ sp:varName "n" ] ]
                    [ a sp:Let ;
                      sp:expression [ a sp:ucase ; sp:arg1 [ a sp:concat ; sp:arg1 "R:" ; sp:arg2 [ a sp:str ; sp:arg1 [ sp:varName "n" ] ] ] ] ;
                      sp:variable [ sp:varName "lbl" ] ]
                  ) ]
              ) .
            "#,
        );
        let sparql = serialize_query(&g, &ex("q")).unwrap();
        let parsed = parse_query(&sparql, None, &[]).expect(&sparql);
        assert!(
            matches!(parsed, spargebra::Query::Construct { .. }),
            "{sparql}"
        );
        assert!(sparql.contains("?this"), "{sparql}");
        assert!(sparql.contains("BIND ("), "{sparql}");
        assert!(sparql.contains("OPTIONAL {"), "{sparql}");
        assert!(sparql.contains("UCASE(CONCAT("), "{sparql}");
    }

    #[test]
    fn serialize_select_modifiers_aggregates_union_subquery_values_minus_graph_service_exists() {
        let g = parse_ttl(
            r#"
            ex:q a sp:Select ;
              sp:distinct true ;
              sp:from ex:g1 ;
              sp:fromNamed ex:g2 ;
              sp:resultVariables (
                [ sp:varName "s" ]
                [ sp:varName "cnt" ; sp:expression [ a sp:Count ; sp:distinct true ; sp:expression [ sp:varName "o" ] ] ]
                [ sp:varName "all" ; sp:expression [ a sp:Count ] ]
                [ sp:varName "joined" ; sp:expression [ a sp:GroupConcat ; sp:expression [ sp:varName "o" ] ; sp:separator "," ] ]
              ) ;
              sp:where (
                [ a sp:Union ;
                  sp:elements (
                    ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:p ; sp:object [ sp:varName "o" ] ] )
                    ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:q ; sp:object [ sp:varName "o" ] ] )
                  ) ]
                [ a sp:SubQuery ;
                  sp:query [ a sp:Select ;
                             sp:reduced true ;
                             sp:where ( [ sp:subject [ sp:varName "s" ] ; sp:predicate rdf:type ; sp:object ex:Item ] ) ] ]
                [ a sp:Values ;
                  sp:varNames ( "k" "v" ) ;
                  sp:bindings ( ( 1 "a" ) ( 2 sp:undef ) ) ]
                [ a sp:Minus ;
                  sp:elements ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:skip ; sp:object true ] ) ]
                [ a sp:NamedGraph ;
                  sp:graphNameNode ex:g2 ;
                  sp:elements ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:seen ; sp:object [ sp:varName "k" ] ] ) ]
                [ a sp:Service ;
                  sp:serviceURI ex:remote ;
                  sp:silent true ;
                  sp:elements ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:ext ; sp:object [ sp:varName "e" ] ] ) ]
                [ a sp:NotExists ;
                  sp:elements ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:banned ; sp:object true ] ) ]
                [ a sp:Exists ;
                  sp:elements ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:active ; sp:object true ] ) ]
                [ a sp:Filter ;
                  sp:expression [ a sp:in ; sp:arg1 [ sp:varName "k" ] ; sp:arg2 1 ; sp:arg3 2 ] ]
              ) ;
              sp:groupBy ( [ sp:varName "s" ] ) ;
              sp:having ( [ a sp:ge ; sp:arg1 [ a sp:Count ] ; sp:arg2 1 ] ) ;
              sp:orderBy ( [ a sp:Desc ; sp:expression [ sp:varName "cnt" ] ] [ a sp:Asc ; sp:expression [ sp:varName "s" ] ] ) ;
              sp:limit 10 ;
              sp:offset 5 .
            "#,
        );
        let sparql = serialize_query(&g, &ex("q")).unwrap();
        let parsed = parse_query(&sparql, None, &[]).expect(&sparql);
        assert!(
            matches!(parsed, spargebra::Query::Select { .. }),
            "{sparql}"
        );
        assert!(sparql.contains("SELECT DISTINCT"), "{sparql}");
        assert!(sparql.contains("COUNT(DISTINCT ?o)"), "{sparql}");
        assert!(sparql.contains("COUNT(*)"), "{sparql}");
        assert!(
            sparql.contains("GROUP_CONCAT(?o ; SEPARATOR = \",\")"),
            "{sparql}"
        );
        assert!(sparql.contains("UNION"), "{sparql}");
        assert!(sparql.contains("VALUES (?k ?v)"), "{sparql}");
        assert!(sparql.contains("UNDEF"), "{sparql}");
        assert!(sparql.contains("MINUS"), "{sparql}");
        assert!(sparql.contains("GRAPH <http://example.org/g2>"), "{sparql}");
        assert!(
            sparql.contains("SERVICE SILENT <http://example.org/remote>"),
            "{sparql}"
        );
        assert!(sparql.contains("FILTER NOT EXISTS"), "{sparql}");
        assert!(sparql.contains("FILTER EXISTS"), "{sparql}");
        assert!(sparql.contains("GROUP BY ?s"), "{sparql}");
        assert!(sparql.contains("HAVING"), "{sparql}");
        assert!(sparql.contains("ORDER BY DESC(?cnt) ASC(?s)"), "{sparql}");
        assert!(sparql.contains("LIMIT 10 OFFSET 5"), "{sparql}");
    }

    #[test]
    fn serialize_ask_describe_and_builtins() {
        let g = parse_ttl(
            r#"
            ex:ask a sp:Ask ;
              sp:where (
                [ sp:subject [ sp:varName "x" ] ; sp:predicate ex:v ; sp:object [ sp:varName "v" ] ]
                [ a sp:Filter ;
                  sp:expression [ a sp:and ;
                                  sp:arg1 [ a sp:regex ; sp:arg1 [ a sp:str ; sp:arg1 [ sp:varName "v" ] ] ; sp:arg2 "^a" ; sp:arg3 "i" ] ;
                                  sp:arg2 [ a sp:notIn ; sp:arg1 [ sp:varName "v" ] ; sp:arg2 "bad" ] ] ]
                [ a sp:Filter ;
                  sp:expression [ a sp:Exists ;
                                  sp:elements ( [ sp:subject [ sp:varName "x" ] ; sp:predicate ex:ok ; sp:object true ] ) ] ]
                [ a sp:Bind ;
                  sp:expression [ a sp:if ;
                                  sp:arg1 [ a sp:isNumeric ; sp:arg1 [ sp:varName "v" ] ] ;
                                  sp:arg2 [ a sp:abs ; sp:arg1 [ a sp:unaryMinus ; sp:arg1 [ sp:varName "v" ] ] ] ;
                                  sp:arg3 [ a sp:coalesce ; sp:arg1 [ a sp:now ] ; sp:arg2 [ a sp:rand ] ] ] ;
                  sp:variable [ sp:varName "z" ] ]
              ) .
            ex:desc1 a sp:Describe ;
              sp:resultNodes ( ex:Alice [ sp:varName "b" ] ) ;
              sp:where ( [ sp:subject ex:Alice ; sp:predicate ex:knows ; sp:object [ sp:varName "b" ] ] ) .
            ex:desc2 a sp:Describe ;
              sp:resultNodes ( ex:Alice ) .
            "#,
        );
        let ask = serialize_query(&g, &ex("ask")).unwrap();
        assert!(matches!(
            parse_query(&ask, None, &[]).expect(&ask),
            spargebra::Query::Ask { .. }
        ));
        let d1 = serialize_query(&g, &ex("desc1")).unwrap();
        assert!(matches!(
            parse_query(&d1, None, &[]).expect(&d1),
            spargebra::Query::Describe { .. }
        ));
        let d2 = serialize_query(&g, &ex("desc2")).unwrap();
        assert!(matches!(
            parse_query(&d2, None, &[]).expect(&d2),
            spargebra::Query::Describe { .. }
        ));
    }

    #[test]
    fn template_expansion_handles_text_rdf_defaults_optional_and_inheritance() {
        let g = parse_ttl(
            r#"
            ex:BaseTmpl a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:targetClass ; spl:valueType rdfs:Class ] ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:minVal ; spl:valueType xsd:integer ; spl:defaultValue 10 ] ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:optTag ; spl:optional true ; spl:valueType xsd:string ] ;
              spin:body [
                a sp:Construct ;
                sp:text """
                  # ?targetClass in comment stays untouched
                  CONSTRUCT { ?this a ?targetClass ; <http://example.org/tag> ?label }
                  WHERE {
                    ?this <http://example.org/score> ?s .
                    FILTER (?s >= ?minVal && "?minVal" != "")
                    BIND (IF(BOUND(?optTag), ?optTag, "none") AS ?label)
                  }
                """
              ] .
            ex:SubTmpl a spin:ConstructTemplate ;
              rdfs:subClassOf ex:BaseTmpl ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:minVal ; spl:defaultValue 50 ] .

            ex:call1 a ex:SubTmpl ; ex:targetClass ex:HighScorer ; ex:optTag "gold" .
            ex:call2 a ex:SubTmpl ; ex:targetClass ex:HighScorer .
            "#,
        );
        let q1 = serialize_query(&g, &ex("call1")).unwrap();
        assert!(
            matches!(
                parse_query(&q1, None, &[]).expect(&q1),
                spargebra::Query::Construct { .. }
            ),
            "{q1}"
        );
        assert!(q1.contains("<http://example.org/HighScorer>"), "{q1}");
        assert!(
            q1.contains("\"50\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
            "{q1}"
        );
        assert!(q1.contains("IF(true, \"gold\", \"none\")"), "{q1}");
        assert!(q1.contains("\"?minVal\""), "string literal untouched: {q1}");

        let q2 = serialize_query(&g, &ex("call2")).unwrap();
        assert!(
            matches!(
                parse_query(&q2, None, &[]).expect(&q2),
                spargebra::Query::Construct { .. }
            ),
            "{q2}"
        );
        assert!(q2.contains("IF(BOUND(?optTag), ?optTag, \"none\")"), "{q2}");
    }

    #[test]
    fn template_rdf_body_and_diamond_inheritance_succeed() {
        let g = parse_ttl(
            r#"
            ex:Root a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:pred ] ;
              spin:body [
                a sp:Construct ;
                sp:templates ( [ sp:subject spin:_this ; sp:predicate [ sp:varName "pred" ] ; sp:object [ sp:varName "val" ] ] ) ;
                sp:where (
                  [ sp:subject spin:_this ; sp:predicate ex:src ; sp:object [ sp:varName "x" ] ]
                  [ a sp:Filter ; sp:expression [ a sp:bound ; sp:arg1 [ sp:varName "val" ] ] ]
                )
              ] .
            ex:Left rdfs:subClassOf ex:Root ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:val ; spl:defaultValue 1 ] .
            ex:Right rdfs:subClassOf ex:Root .
            ex:Leaf rdfs:subClassOf ex:Left, ex:Right .
            ex:call a ex:Leaf ; ex:pred ex:out .
            "#,
        );
        let q = serialize_query(&g, &ex("call")).unwrap();
        assert!(
            matches!(
                parse_query(&q, None, &[]).expect(&q),
                spargebra::Query::Construct { .. }
            ),
            "{q}"
        );
        assert!(q.contains("<http://example.org/out>"), "{q}");
        assert!(q.contains("FILTER (true)"), "{q}");
    }

    #[test]
    fn refuses_cycles_over_depth_missing_args_and_malformed_inputs() {
        // 1. Cyclic RDF list
        let mut g = Graph::new();
        let q = NamedNode::new_unchecked("http://example.org/q");
        let c1 = BlankNode::new_unchecked("c1");
        g.insert(&Triple::new(
            q.clone(),
            crate::vocab::rdf::TYPE,
            super::vocab::sp::ASK,
        ));
        g.insert(&Triple::new(q.clone(), super::vocab::sp::WHERE, c1.clone()));
        g.insert(&Triple::new(
            c1.clone(),
            crate::vocab::rdf::FIRST,
            Literal::from(1),
        ));
        g.insert(&Triple::new(c1.clone(), crate::vocab::rdf::REST, c1));
        let err = serialize_query(&g, &Term::NamedNode(q)).unwrap_err();
        assert!(format!("{err:#}").contains("cyclic RDF list"), "{err:#}");

        // 2. Cyclic expression
        let g = parse_ttl(
            r#"
            ex:q a sp:Ask ; sp:where ( [ a sp:Filter ; sp:expression ex:e1 ] ) .
            ex:e1 a sp:not ; sp:arg1 ex:e2 .
            ex:e2 a sp:not ; sp:arg1 ex:e1 .
            "#,
        );
        let err = serialize_query(&g, &ex("q")).unwrap_err();
        assert!(format!("{err:#}").contains("cyclic"), "{err:#}");

        // 3. Cyclic property path
        let g = parse_ttl(
            r#"
            ex:q a sp:Ask ;
              sp:where ( [ a sp:TriplePath ; sp:subject ex:a ; sp:path ex:p1 ; sp:object ex:b ] ) .
            ex:p1 a sp:ReversePath ; sp:subPath ex:p1 .
            "#,
        );
        let err = serialize_query(&g, &ex("q")).unwrap_err();
        assert!(format!("{err:#}").contains("cyclic"), "{err:#}");

        // 4. Over-depth expression (> 64)
        let mut ttl =
            String::from("ex:q a sp:Ask ; sp:where ( [ a sp:Filter ; sp:expression ex:e0 ] ) .\n");
        for i in 0..70 {
            ttl.push_str(&format!("ex:e{i} a sp:not ; sp:arg1 ex:e{} .\n", i + 1));
        }
        ttl.push_str("ex:e70 sp:varName \"x\" .\n");
        let g = parse_ttl(&ttl);
        let err = serialize_query(&g, &ex("q")).unwrap_err();
        assert!(
            format!("{err:#}").contains("maximum nesting depth"),
            "{err:#}"
        );

        // 5. Exponential DAG node-budget explosion (binary tree of depth 20 -> 2^20 nodes)
        let mut ttl =
            String::from("ex:q a sp:Ask ; sp:where ( [ a sp:Filter ; sp:expression ex:d0 ] ) .\n");
        for i in 0..20 {
            ttl.push_str(&format!(
                "ex:d{i} a sp:and ; sp:arg1 ex:d{} ; sp:arg2 ex:d{} .\n",
                i + 1,
                i + 1
            ));
        }
        ttl.push_str("ex:d20 sp:varName \"x\" .\n");
        let g = parse_ttl(&ttl);
        let err = serialize_query(&g, &ex("q")).unwrap_err();
        assert!(format!("{err:#}").contains("node budget"), "{err:#}");

        // 6. Cyclic template rdfs:subClassOf
        let g = parse_ttl(
            r#"
            ex:T1 a spin:ConstructTemplate ; rdfs:subClassOf ex:T2 ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this a <http://example.org/A> } WHERE { }" ] .
            ex:T2 rdfs:subClassOf ex:T1 .
            ex:c a ex:T1 .
            "#,
        );
        let err = serialize_query(&g, &ex("c")).unwrap_err();
        assert!(
            format!("{err:#}").contains("cyclic rdfs:subClassOf"),
            "{err:#}"
        );

        // 7. Cyclic nested template body
        let g = parse_ttl(
            r#"
            ex:T1 a spin:ConstructTemplate ; spin:body [ a ex:T2 ] .
            ex:T2 a spin:ConstructTemplate ; spin:body [ a ex:T1 ] .
            ex:c a ex:T1 .
            "#,
        );
        let err = serialize_query(&g, &ex("c")).unwrap_err();
        assert!(
            format!("{err:#}").contains("cyclic SPIN template"),
            "{err:#}"
        );

        // 8. Missing required argument
        let g = parse_ttl(
            r#"
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:req ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this a ?req } WHERE { }" ] .
            ex:c a ex:T .
            "#,
        );
        let err = serialize_query(&g, &ex("c")).unwrap_err();
        assert!(
            format!("{err:#}").contains("missing required argument"),
            "{err:#}"
        );

        // 9. Wrong spl:valueType
        let g = parse_ttl(
            r#"
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:n ; spl:valueType xsd:integer ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this <http://example.org/n> ?n } WHERE { }" ] .
            ex:c a ex:T ; ex:n "not-an-int" .
            "#,
        );
        let err = serialize_query(&g, &ex("c")).unwrap_err();
        assert!(format!("{err:#}").contains("datatype"), "{err:#}");

        // 10. Invalid sp:varName (SPARQL injection attempt)
        let g = parse_ttl(
            r#"
            ex:q a sp:Ask ;
              sp:where ( [ sp:subject [ sp:varName "x } UNION { ?a ?b ?c" ] ; sp:predicate ex:p ; sp:object 1 ] ) .
            "#,
        );
        let err = serialize_query(&g, &ex("q")).unwrap_err();
        assert!(format!("{err:#}").contains("invalid"), "{err:#}");

        // 11. Non-CONSTRUCT spin:rule refused by translate
        let g = parse_ttl(
            r#"
            ex:Person spin:rule [ a sp:Ask ; sp:where ( [ sp:subject spin:_this ; sp:predicate ex:p ; sp:object 1 ] ) ] .
            "#,
        );
        let err = translate(&g, &[]).unwrap_err();
        assert!(format!("{err:#}").contains("CONSTRUCT"), "{err:#}");

        // 12. Assigning to bound template argument with AS refused
        let g = parse_ttl(
            r#"
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:x ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this <http://example.org/p> ?x } WHERE { BIND(1 AS ?x) }" ] .
            ex:c a ex:T ; ex:x 2 .
            "#,
        );
        let err = serialize_query(&g, &ex("c")).unwrap_err();
        assert!(
            format!("{err:#}").contains("assigns to bound argument"),
            "{err:#}"
        );
    }
}
