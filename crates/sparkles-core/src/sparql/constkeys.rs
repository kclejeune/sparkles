//! The vocabulary keys of the constants of a query (spec X07 §5.1).
//!
//! The planner resolves each constant to an id with one vocabulary lookup, one after the
//! other. On a cold server each lookup in a vocabulary group not read before waits for that
//! group. Collecting the keys first lets [`Vocab::prefetch_keys`](crate::vocab::Vocab) ask for
//! every group at once.

use crate::id;
use oxrdf::Term;
use spargebra::algebra::{Expression, GraphPattern, PropertyPathExpression};
use spargebra::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};

/// The keys of the IRIs and the non-inline literals that `pattern` names, with duplicates.
/// Constants of expressions are included for the usual comparisons and calls.
pub(crate) fn constant_keys(pattern: &GraphPattern) -> Vec<Vec<u8>> {
    let mut out = Keys::default();
    out.pattern(pattern);
    out.keys
}

#[derive(Default)]
struct Keys {
    keys: Vec<Vec<u8>>,
}

impl Keys {
    fn term(&mut self, t: &Term) {
        if id::inline_id(t).is_none() && !matches!(t, Term::BlankNode(_) | Term::Triple(_)) {
            self.keys.push(id::term_key(t));
        }
    }

    fn iri(&mut self, iri: &str) {
        self.keys.push(id::iri_key(iri));
    }

    fn term_pattern(&mut self, t: &TermPattern) {
        match t {
            TermPattern::NamedNode(n) => self.iri(n.as_str()),
            TermPattern::Literal(l) => self.term(&Term::Literal(l.clone())),
            _ => {}
        }
    }

    fn named(&mut self, n: &NamedNodePattern) {
        if let NamedNodePattern::NamedNode(n) = n {
            self.iri(n.as_str());
        }
    }

    fn triple(&mut self, t: &TriplePattern) {
        self.term_pattern(&t.subject);
        self.named(&t.predicate);
        self.term_pattern(&t.object);
    }

    fn path(&mut self, p: &PropertyPathExpression) {
        match p {
            PropertyPathExpression::NamedNode(n) => self.iri(n.as_str()),
            PropertyPathExpression::Reverse(a)
            | PropertyPathExpression::ZeroOrMore(a)
            | PropertyPathExpression::OneOrMore(a)
            | PropertyPathExpression::ZeroOrOne(a) => self.path(a),
            PropertyPathExpression::Sequence(a, b) | PropertyPathExpression::Alternative(a, b) => {
                self.path(a);
                self.path(b);
            }
            PropertyPathExpression::NegatedPropertySet(ns) => {
                ns.iter().for_each(|n| self.iri(n.as_str()))
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &Expression) {
        match e {
            Expression::NamedNode(n) => self.iri(n.as_str()),
            Expression::Literal(l) => self.term(&Term::Literal(l.clone())),
            Expression::Or(a, b)
            | Expression::And(a, b)
            | Expression::Equal(a, b)
            | Expression::SameTerm(a, b)
            | Expression::Greater(a, b)
            | Expression::GreaterOrEqual(a, b)
            | Expression::Less(a, b)
            | Expression::LessOrEqual(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            Expression::In(a, list) => {
                self.expr(a);
                list.iter().for_each(|x| self.expr(x));
            }
            Expression::Not(a) => self.expr(a),
            _ => {}
        }
    }

    fn pattern(&mut self, p: &GraphPattern) {
        match p {
            GraphPattern::Bgp { patterns } => patterns.iter().for_each(|t| self.triple(t)),
            GraphPattern::Path {
                subject,
                path,
                object,
            } => {
                self.term_pattern(subject);
                self.path(path);
                self.term_pattern(object);
            }
            GraphPattern::Join { left, right }
            | GraphPattern::Lateral { left, right }
            | GraphPattern::Union { left, right }
            | GraphPattern::Minus { left, right }
            | GraphPattern::SemiJoin { left, right }
            | GraphPattern::AntiJoin { left, right } => {
                self.pattern(left);
                self.pattern(right);
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => {
                self.pattern(left);
                self.pattern(right);
                if let Some(e) = expression {
                    self.expr(e);
                }
            }
            GraphPattern::Filter { expr, inner } => {
                self.expr(expr);
                self.pattern(inner);
            }
            GraphPattern::Graph { name, inner } => {
                self.named(name);
                self.pattern(inner);
            }
            GraphPattern::Extend { inner, .. }
            | GraphPattern::Assign { inner, .. }
            | GraphPattern::Unfold { inner, .. }
            | GraphPattern::OrderBy { inner, .. }
            | GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner }
            | GraphPattern::Reduced { inner }
            | GraphPattern::Slice { inner, .. }
            | GraphPattern::Group { inner, .. } => self.pattern(inner),
            GraphPattern::Values { bindings, .. } => {
                for row in bindings {
                    for t in row.iter().flatten() {
                        match t {
                            GroundTerm::NamedNode(n) => self.iri(n.as_str()),
                            GroundTerm::Literal(l) => self.term(&Term::Literal(l.clone())),
                            _ => {}
                        }
                    }
                }
            }
            // a remote endpoint's constants are not in this vocabulary
            GraphPattern::Service { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(q: &str) -> Vec<String> {
        let parsed = spargebra::SparqlParser::new().parse_query(q).unwrap();
        let spargebra::Query::Select { pattern, .. } = parsed else {
            panic!()
        };
        constant_keys(&pattern)
            .into_iter()
            .map(|k| String::from_utf8_lossy(&k).into_owned())
            .collect()
    }

    #[test]
    fn collects_patterns_values_paths_and_filters() {
        let k = keys(
            "SELECT * WHERE { ?p <urn:worksFor> <urn:org7> ; <urn:name>|<urn:alt>+ ?n \
             VALUES ?p { <urn:a> <urn:b> } FILTER(?n = \"Ada\") GRAPH <urn:g> { ?p <urn:x> 7 } }",
        );
        for want in [
            "urn:worksFor",
            "urn:org7",
            "urn:name",
            "urn:alt",
            "urn:a",
            "urn:b",
            "urn:g",
            "urn:x",
            "Ada",
        ] {
            assert!(k.iter().any(|s| s.contains(want)), "{want} in {k:?}");
        }
        // 7 is an inline integer and needs no key
        assert!(!k.iter().any(|s| s.contains('7') && !s.contains("org7")));
    }
}
