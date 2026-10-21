//! Give pure UNION branches the selective graph pattern joined outside them.
//!
//! The usual join-order search cannot reorder an opaque UNION's interior. Distribute
//! a small BGP into its branches so that search can constrain their joins. Filters
//! cross the join only when the new bindings cannot change their original scope.
use spargebra::algebra::{Expression, Function, GraphPattern};
use spargebra::term::{TermPattern, TriplePattern, Variable};

const MAX_TRIPLES: usize = 8;
const MAX_BRANCHES: usize = 8;
/// Do not duplicate broad anchor reads merely because they have a constant object.
pub(super) const MAX_ANCHOR_ROWS: f64 = 32.0;

pub(super) fn rewrite<'a>(
    gp: &'a GraphPattern,
    calls: &dyn Fn(&[TriplePattern]) -> bool,
) -> Option<(&'a GraphPattern, GraphPattern)> {
    let GraphPattern::Join { left, right } = gp else {
        return None;
    };
    let (anchor, union) = match (&**left, &**right) {
        (anchor @ GraphPattern::Bgp { .. }, union @ GraphPattern::Union { .. })
        | (union @ GraphPattern::Union { .. }, anchor @ GraphPattern::Bgp { .. }) => {
            (anchor, union)
        }
        _ => return None,
    };
    let GraphPattern::Bgp { patterns } = anchor else {
        unreachable!()
    };
    if patterns.is_empty()
        || patterns.len() > MAX_TRIPLES
        || calls(patterns)
        // A constant predicate alone need not constrain the shared variables.
        || !patterns.iter().any(|t| {
            matches!(t.object, TermPattern::NamedNode(_) | TermPattern::Literal(_))
        })
    {
        return None;
    }
    let moved = variables(anchor);
    push(union, patterns, &moved, calls, &mut 0).map(|pattern| (anchor, pattern))
}

fn variables(gp: &GraphPattern) -> Vec<Variable> {
    let mut out = Vec::new();
    gp.on_in_scope_variable(|v| {
        if !out.contains(v) {
            out.push(v.clone());
        }
    });
    out
}

/// Only BGPs and their FILTER wrappers have certainly-bound named variables here.
/// In particular an OPTIONAL or UNION's in-scope variables would not prove this.
fn bound(gp: &GraphPattern) -> Option<Vec<Variable>> {
    match gp {
        GraphPattern::Bgp { .. } => Some(variables(gp)),
        GraphPattern::Filter { inner, .. } => bound(inner),
        _ => None,
    }
}

fn push(
    gp: &GraphPattern,
    anchor: &[TriplePattern],
    moved: &[Variable],
    calls: &dyn Fn(&[TriplePattern]) -> bool,
    branches: &mut usize,
) -> Option<GraphPattern> {
    match gp {
        GraphPattern::Union { left, right } => Some(GraphPattern::Union {
            left: Box::new(push(left, anchor, moved, calls, branches)?),
            right: Box::new(push(right, anchor, moved, calls, branches)?),
        }),
        GraphPattern::Filter { expr, inner } => {
            let bound = bound(inner)?;
            if !safe_expression(expr, moved, &bound) {
                return None;
            }
            Some(GraphPattern::Filter {
                expr: expr.clone(),
                inner: Box::new(push(inner, anchor, moved, calls, branches)?),
            })
        }
        GraphPattern::Bgp { patterns } => {
            *branches += 1;
            if *branches > MAX_BRANCHES
                || calls(patterns)
                || !variables(gp).iter().any(|v| moved.contains(v))
            {
                return None;
            }
            // Conjunction is a bag join. Keep every pattern and every matching
            // assignment, including anchor variables hidden by an outer projection.
            let mut combined = Vec::with_capacity(anchor.len() + patterns.len());
            combined.extend_from_slice(anchor);
            combined.extend_from_slice(patterns);
            Some(GraphPattern::Bgp { patterns: combined })
        }
        // No scope or effect assumptions about OPTIONAL, subqueries, BIND, SERVICE,
        // LATERAL, paths or registered property functions are needed for this rule.
        _ => None,
    }
}

fn safe_expression(expr: &Expression, moved: &[Variable], bound: &[Variable]) -> bool {
    use Expression as E;
    match expr {
        E::NamedNode(_) | E::Literal(_) => true,
        E::Variable(v) | E::Bound(v) => !moved.contains(v) || bound.contains(v),
        E::Exists(_) => false,
        E::Or(a, b)
        | E::And(a, b)
        | E::Equal(a, b)
        | E::SameTerm(a, b)
        | E::Greater(a, b)
        | E::GreaterOrEqual(a, b)
        | E::Less(a, b)
        | E::LessOrEqual(a, b)
        | E::Add(a, b)
        | E::Subtract(a, b)
        | E::Multiply(a, b)
        | E::Divide(a, b) => safe_expression(a, moved, bound) && safe_expression(b, moved, bound),
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => safe_expression(a, moved, bound),
        E::In(a, rest) => {
            safe_expression(a, moved, bound)
                && rest.iter().all(|x| safe_expression(x, moved, bound))
        }
        E::If(a, b, c) => {
            safe_expression(a, moved, bound)
                && safe_expression(b, moved, bound)
                && safe_expression(c, moved, bound)
        }
        E::Coalesce(args) => args.iter().all(|x| safe_expression(x, moved, bound)),
        E::FunctionCall(f, args) => {
            !matches!(
                f,
                Function::Rand
                    | Function::Uuid
                    | Function::StrUuid
                    | Function::BNode
                    | Function::Custom(_)
            ) && args.iter().all(|x| safe_expression(x, moved, bound))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(query: &str) -> GraphPattern {
        let spargebra::Query::Select { pattern, .. } =
            spargebra::SparqlParser::new().parse_query(query).unwrap()
        else {
            unreachable!()
        };
        match pattern {
            GraphPattern::Project { inner, .. } => *inner,
            other => other,
        }
    }

    fn rewrite_query(query: &str) -> Option<GraphPattern> {
        rewrite(&body(query), &|_| false).map(|(_, pattern)| pattern)
    }

    #[test]
    fn bound_filters_and_both_join_orientations_are_supported() {
        let anchor = "?root <urn:anchor> <urn:target>";
        let union = "{ ?root <urn:p> ?v FILTER(?root != ?v) } UNION { ?root <urn:q> ?v FILTER(BOUND(?root)) }";
        for query in [
            format!("SELECT * WHERE {{ {anchor} {{ {union} }} }}"),
            format!("SELECT * WHERE {{ {{ {union} }} {anchor} }}"),
        ] {
            let got = rewrite_query(&query).unwrap();
            assert!(matches!(got, GraphPattern::Union { .. }), "{got}");
        }
    }

    #[test]
    fn newly_bound_filter_variables_and_effectful_branches_stay_outside() {
        for branch in [
            "?root <urn:p> ?v FILTER(BOUND(?outer))",
            "?root <urn:p> ?v FILTER(!BOUND(?outer))",
            "?root <urn:p> ?v FILTER(?outer = <urn:target>)",
            "?root <urn:p> ?v FILTER(RAND() < 1)",
            "?root <urn:p> ?v FILTER(STR(UUID()) != \"\")",
            "?root <urn:p> ?v FILTER(STRUUID() != \"\")",
            "?root <urn:p> ?v FILTER(isBlank(BNODE()))",
            "?root <urn:p> ?v FILTER(<urn:custom>(?v))",
            "?root <urn:p> ?v FILTER EXISTS { ?v <urn:q> ?root }",
            "?root <urn:p> ?v OPTIONAL { ?root <urn:q> ?outer }",
            "?root <urn:p> ?v BIND(?v AS ?extra)",
            "VALUES (?root ?v) { (<urn:s> UNDEF) }",
            "SERVICE <http://example.org/sparql> { ?root <urn:p> ?v }",
            "{ SELECT ?root WHERE { ?root <urn:p> ?v } LIMIT 1 }",
        ] {
            let query = format!(
                "SELECT * WHERE {{ ?root <urn:anchor> <urn:target>; <urn:bind> ?outer . {{ {branch} }} UNION {{ ?root <urn:q> ?v }} }}"
            );
            assert!(rewrite_query(&query).is_none(), "{query}");
        }
    }

    #[test]
    fn property_functions_are_not_duplicated() {
        let gp = body(
            "SELECT * WHERE { ?root <urn:anchor> <urn:target> { ?root <urn:effect> ?v } UNION { ?root <urn:q> ?v } }",
        );
        let calls = |patterns: &[TriplePattern]| {
            patterns.iter().any(|t| match &t.predicate {
                spargebra::term::NamedNodePattern::NamedNode(p) => p.as_str() == "urn:effect",
                _ => false,
            })
        };
        assert!(rewrite(&gp, &calls).is_none());
        assert!(rewrite(&gp, &|_| true).is_none());
    }

    #[test]
    fn lateral_branches_are_not_reordered() {
        let mut gp = body(
            "SELECT * WHERE { ?root <urn:anchor> <urn:target> { ?root <urn:p> ?v } UNION { ?root <urn:q> ?v } }",
        );
        let GraphPattern::Join { right, .. } = &mut gp else {
            panic!("join");
        };
        let GraphPattern::Union { left, .. } = &mut **right else {
            panic!("union");
        };
        **left = GraphPattern::Lateral {
            left: Box::new((**left).clone()),
            right: Box::new(GraphPattern::Bgp {
                patterns: Vec::new(),
            }),
        };
        assert!(rewrite(&gp, &|_| false).is_none());
    }

    #[test]
    fn expansion_and_unconstrained_anchors_are_bounded() {
        for count in [MAX_BRANCHES, MAX_BRANCHES + 1] {
            let branches = std::iter::repeat_n("{ ?root <urn:p> ?v }", count)
                .collect::<Vec<_>>()
                .join(" UNION ");
            let query =
                format!("SELECT * WHERE {{ ?root <urn:anchor> <urn:target> {{ {branches} }} }}");
            assert_eq!(rewrite_query(&query).is_some(), count == MAX_BRANCHES);
        }
        for anchor in [
            "?root ?p ?o".to_string(),
            (0..9)
                .map(|i| format!("?root <urn:p{i}> <urn:target> ."))
                .collect::<String>(),
        ] {
            let query = format!(
                "SELECT * WHERE {{ {anchor} {{ ?root <urn:p> ?v }} UNION {{ ?root <urn:q> ?v }} }}"
            );
            assert!(rewrite_query(&query).is_none());
        }
    }
}
