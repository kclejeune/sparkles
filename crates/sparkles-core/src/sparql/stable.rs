//! Whether a query has the same answer every time it runs on one snapshot (spec F13
//! §6.1), for the patterns of protections that grant containment compares.
//!
//! The test is a list of what is known to be stable, not a list of what is known not to
//! be. A construct missing from it counts as one that may change between runs, so the
//! comparison of grant sets refuses whatever relies on it.

use spargebra::Query;
use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression, Function, GraphPattern, OrderExpression,
    PropertyPathExpression,
};
use spargebra::term::{NamedNodePattern, TriplePattern};

/// Whether `query`, a SELECT without a dataset clause, parses and holds only constructs
/// whose results depend on the snapshot and the query's own text.
pub(crate) fn query_is_stable(query: &str) -> bool {
    match super::parse_query(query, None, &[]) {
        Ok(Query::Select {
            dataset: None,
            pattern,
            ..
        }) => pattern_is_stable(&pattern, &property_functions()),
        _ => false,
    }
}

fn property_functions() -> Vec<String> {
    super::catalog::property_functions()
}

fn pattern_is_stable(p: &GraphPattern, pfs: &[String]) -> bool {
    let pattern = |p: &GraphPattern| pattern_is_stable(p, pfs);
    let expr = |e: &Expression| expr_is_stable(e, pfs);
    match p {
        GraphPattern::Bgp { patterns } => bgp_is_stable(patterns, pfs),
        GraphPattern::Path { path, .. } => path_is_stable(path, pfs),
        GraphPattern::Join { left, right }
        | GraphPattern::Union { left, right }
        | GraphPattern::Minus { left, right } => pattern(left) && pattern(right),
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } => pattern(left) && pattern(right) && expression.as_ref().is_none_or(expr),
        GraphPattern::Filter { expr: e, inner } => expr(e) && pattern(inner),
        GraphPattern::Graph { inner, .. } => pattern(inner),
        GraphPattern::Extend {
            inner, expression, ..
        } => expr(expression) && pattern(inner),
        GraphPattern::Values { .. } => true,
        GraphPattern::OrderBy { inner, expression } => {
            pattern(inner)
                && expression.iter().all(|k| match k {
                    OrderExpression::Asc(e) | OrderExpression::Desc(e) => expr(e),
                })
        }
        GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner } => pattern(inner),
        GraphPattern::Group {
            inner, aggregates, ..
        } => {
            pattern(inner)
                && aggregates.iter().all(|(_, a)| match a {
                    AggregateExpression::CountSolutions { .. } => true,
                    AggregateExpression::FunctionCall { name, expr: e, .. } => {
                        matches!(
                            name,
                            AggregateFunction::Count
                                | AggregateFunction::Min
                                | AggregateFunction::Max
                        ) && expr(e)
                    }
                    // the order of a group's solutions, which FOLD, SAMPLE and
                    // GROUP_CONCAT observe, and a sum's rounding are not proven
                    _ => false,
                })
        }
        // a SERVICE, a LIMIT or OFFSET, LATERAL, and the extensions
        _ => false,
    }
}

fn bgp_is_stable(patterns: &[TriplePattern], pfs: &[String]) -> bool {
    !super::arqpf::has_calls(patterns)
        && patterns.iter().all(|t| match &t.predicate {
            NamedNodePattern::NamedNode(n) => !pfs.iter().any(|f| f == n.as_str()),
            NamedNodePattern::Variable(_) => true,
        })
}

fn path_is_stable(p: &PropertyPathExpression, pfs: &[String]) -> bool {
    use PropertyPathExpression as PP;
    match p {
        PP::NamedNode(n) => !pfs.iter().any(|f| f == n.as_str()),
        PP::NegatedPropertySet(_) => true,
        PP::Reverse(a) | PP::ZeroOrMore(a) | PP::OneOrMore(a) | PP::ZeroOrOne(a) => {
            path_is_stable(a, pfs)
        }
        PP::Sequence(a, b) | PP::Alternative(a, b) => {
            path_is_stable(a, pfs) && path_is_stable(b, pfs)
        }
        PP::Range { path, .. } => path_is_stable(path, pfs),
        // a path that picks some of its matches
        _ => false,
    }
}

fn expr_is_stable(e: &Expression, pfs: &[String]) -> bool {
    let expr = |e: &Expression| expr_is_stable(e, pfs);
    match e {
        Expression::NamedNode(_)
        | Expression::Literal(_)
        | Expression::Variable(_)
        | Expression::Bound(_) => true,
        Expression::Or(a, b)
        | Expression::And(a, b)
        | Expression::Equal(a, b)
        | Expression::SameTerm(a, b)
        | Expression::Greater(a, b)
        | Expression::GreaterOrEqual(a, b)
        | Expression::Less(a, b)
        | Expression::LessOrEqual(a, b)
        | Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Divide(a, b) => expr(a) && expr(b),
        Expression::In(a, l) => expr(a) && l.iter().all(expr),
        Expression::UnaryPlus(a) | Expression::UnaryMinus(a) | Expression::Not(a) => expr(a),
        Expression::If(a, b, c) => expr(a) && expr(b) && expr(c),
        Expression::Coalesce(l) => l.iter().all(expr),
        Expression::Exists(p) => pattern_is_stable(p, pfs),
        Expression::FunctionCall(f, args) => function_is_stable(f) && args.iter().all(expr),
    }
}

/// Whether `f` returns the same value for the same arguments every time.
fn function_is_stable(f: &Function) -> bool {
    use Function as F;
    match f {
        F::Str
        | F::Lang
        | F::LangMatches
        | F::Datatype
        | F::Iri
        | F::Abs
        | F::Ceil
        | F::Floor
        | F::Round
        | F::Concat
        | F::SubStr
        | F::StrLen
        | F::Replace
        | F::UCase
        | F::LCase
        | F::EncodeForUri
        | F::Contains
        | F::StrStarts
        | F::StrEnds
        | F::StrBefore
        | F::StrAfter
        | F::Year
        | F::Month
        | F::Day
        | F::Hours
        | F::Minutes
        | F::Seconds
        | F::Timezone
        | F::Tz
        | F::Md5
        | F::Sha1
        | F::Sha256
        | F::Sha384
        | F::Sha512
        | F::StrLang
        | F::StrDt
        | F::IsIri
        | F::IsBlank
        | F::IsLiteral
        | F::IsNumeric
        | F::Regex
        | F::Triple
        | F::Subject
        | F::Predicate
        | F::Object
        | F::IsTriple
        | F::LangDir
        | F::HasLang
        | F::HasLangDir
        | F::StrLangDir => true,
        // of the extension functions, only the casts are known
        F::Custom(name) => super::expr::is_cast(name.as_str()),
        // the clock, randomness, fresh identifiers, and ADJUST, which reads the
        // system's timezone without one
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::query_is_stable;

    fn select(body: &str) -> String {
        format!("SELECT ?s WHERE {{ {body} }}")
    }

    #[test]
    fn stable_constructs_are_accepted() {
        for body in [
            "?s <http://ex/owner> ?user",
            "?s <http://ex/a>/<http://ex/b>* ?o FILTER(?o > 3 && STRSTARTS(STR(?s), \"http://ex/\"))",
            "{ ?s a <http://ex/C> } UNION { ?s <http://ex/p> ?o } MINUS { ?s <http://ex/q> ?o }",
            "OPTIONAL { ?s <http://ex/p> ?o } BIND(xsd:integer(?o) AS ?n) FILTER EXISTS { ?s <http://ex/q> ?n }",
            "{ SELECT ?s (COUNT(?o) AS ?n) WHERE { ?s <http://ex/p> ?o } GROUP BY ?s HAVING (MAX(?o) > 1) }",
            "GRAPH ?g { ?s ?p ?o } VALUES ?g { <http://ex/g> }",
        ] {
            let q = format!(
                "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> {}",
                select(body)
            );
            assert!(query_is_stable(&q), "{body}");
        }
    }

    #[test]
    fn unstable_constructs_are_refused() {
        for body in [
            "?s <http://ex/until> ?t FILTER(?t > NOW())",
            "?s ?p ?o FILTER(RAND() < 0.5)",
            "?s ?p ?o BIND(UUID() AS ?u)",
            "?s ?p ?o BIND(STRUUID() AS ?u)",
            "?s ?p ?o BIND(BNODE() AS ?b)",
            "?s ?p ?o FILTER(<http://ex/fn>(?o))",
            "?s ?p ?o FILTER(<http://jena.apache.org/ARQ/function#now>() > ?o)",
            "SERVICE <http://ex/sparql> { ?s ?p ?o }",
            "{ SELECT ?s WHERE { ?s ?p ?o } LIMIT 1 }",
            "{ SELECT ?s (SAMPLE(?o) AS ?x) WHERE { ?s ?p ?o } GROUP BY ?s }",
            "{ SELECT ?s (GROUP_CONCAT(?o) AS ?x) WHERE { ?s ?p ?o } GROUP BY ?s }",
            "{ SELECT ?s (SUM(?o) AS ?x) WHERE { ?s ?p ?o } GROUP BY ?s }",
            "?s <http://jena.apache.org/text#query> \"x\"",
            "?s ?p ?o FILTER NOT EXISTS { ?s ?q ?x FILTER(?x < NOW()) }",
        ] {
            assert!(!query_is_stable(&select(body)), "{body}");
        }
        assert!(!query_is_stable("not sparql"));
        assert!(!query_is_stable("ASK { ?s ?p ?o }"));
        assert!(!query_is_stable(
            "SELECT ?s FROM <http://ex/g> WHERE { ?s ?p ?o }"
        ));
    }
}
