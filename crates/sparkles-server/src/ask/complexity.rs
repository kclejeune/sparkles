//! The complexity check of C18 §5.5: a score from a parsed query's syntax alone, with
//! no planner. Phase 1 reports it with each answer and the evaluation buckets
//! questions by it. Phase 2 routes on it.

use serde_json::{Value, json};
use spargebra::Query;
use spargebra::algebra::GraphPattern;
use spargebra::term::{NamedNodePattern, TermPattern};
use std::collections::BTreeSet;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// The five counts of §5.5 and the score they give.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Complexity {
    pub joins: u32,
    pub aggregates: u32,
    pub subqueries: u32,
    pub negations: u32,
    pub terms: u32,
}

impl Complexity {
    /// `joins + 2·aggregates + 3·subqueries + 2·negations + max(0, terms − 6)`.
    pub fn score(&self) -> u32 {
        self.joins
            + 2 * self.aggregates
            + 3 * self.subqueries
            + 2 * self.negations
            + self.terms.saturating_sub(6)
    }

    /// The bucket the evaluation reports by: `simple` under 4, `medium` under the
    /// default threshold of 8, `complex` from it.
    pub fn bucket(&self) -> &'static str {
        match self.score() {
            0..=3 => "simple",
            4..=7 => "medium",
            _ => "complex",
        }
    }

    pub fn json(&self) -> Value {
        json!({
            "score": self.score(),
            "bucket": self.bucket(),
            "joins": self.joins,
            "aggregates": self.aggregates,
            "subqueries": self.subqueries,
            "negations": self.negations,
            "terms": self.terms,
        })
    }
}

/// The complexity of a parsed query.
pub fn of(q: &Query) -> Complexity {
    let pattern = match q {
        Query::Select { pattern, .. }
        | Query::Construct { pattern, .. }
        | Query::Describe { pattern, .. }
        | Query::Ask { pattern, .. } => pattern,
    };
    let mut w = Walk::default();
    // the query's own projection and modifiers are not a subquery
    let top = strip_modifiers(pattern);
    w.group(top);
    w.c.terms = w.terms.len() as u32;
    w.c
}

/// The pattern under the outermost Slice, Distinct, Reduced, OrderBy and Project.
fn strip_modifiers(p: &GraphPattern) -> &GraphPattern {
    match p {
        GraphPattern::Slice { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::OrderBy { inner, .. }
        | GraphPattern::Project { inner, .. } => strip_modifiers(inner),
        p => p,
    }
}

#[derive(Default)]
struct Walk {
    c: Complexity,
    terms: BTreeSet<String>,
}

impl Walk {
    /// A group: its elements joined, counted as `elements − 1` joins.
    fn group(&mut self, p: &GraphPattern) {
        let n = self.elements(p);
        self.c.joins += n.saturating_sub(1);
    }

    /// The elements of one group, counting the nested groups once each and walking them
    /// as groups of their own.
    fn elements(&mut self, p: &GraphPattern) -> u32 {
        match p {
            GraphPattern::Bgp { patterns } => {
                for t in patterns {
                    if let NamedNodePattern::NamedNode(n) = &t.predicate {
                        self.terms.insert(n.as_str().to_string());
                        if n.as_str() == RDF_TYPE
                            && let TermPattern::NamedNode(o) = &t.object
                        {
                            self.terms.insert(o.as_str().to_string());
                        }
                    }
                }
                patterns.len() as u32
            }
            GraphPattern::Path { .. } | GraphPattern::Values { .. } => 1,
            GraphPattern::Join { left, right } => self.elements(left) + self.elements(right),
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => {
                if let Some(e) = expression {
                    self.expression(&e.to_string(), true);
                }
                self.group(right);
                self.elements(left) + 1
            }
            GraphPattern::Filter { expr, inner } => {
                let s = expr.to_string();
                // FILTER(!BOUND(?v)) over an OPTIONAL is a negation
                let over_optional = has_left_join(inner);
                self.expression(&s, over_optional);
                if matches!(strip_extend(inner), GraphPattern::Group { .. }) {
                    // HAVING
                    self.c.aggregates += 1;
                }
                self.elements(inner)
            }
            // the projected aggregates of a grouped query are not BINDs
            GraphPattern::Extend {
                inner, expression, ..
            } if matches!(strip_extend(inner), GraphPattern::Group { .. }) => {
                self.expression(&expression.to_string(), false);
                self.elements(inner)
            }
            GraphPattern::Extend {
                inner, expression, ..
            }
            | GraphPattern::Assign {
                inner, expression, ..
            } => {
                self.expression(&expression.to_string(), false);
                self.elements(inner) + 1
            }
            GraphPattern::Unfold { inner, .. } => self.elements(inner) + 1,
            GraphPattern::Minus { left, right } | GraphPattern::AntiJoin { left, right } => {
                self.c.negations += 1;
                self.group(right);
                self.elements(left) + 1
            }
            GraphPattern::SemiJoin { left, right } | GraphPattern::Lateral { left, right } => {
                self.group(right);
                self.elements(left) + 1
            }
            GraphPattern::Union { left, right } => {
                self.group(left);
                self.group(right);
                1
            }
            GraphPattern::Graph { inner, .. } | GraphPattern::Service { inner, .. } => {
                self.group(inner);
                1
            }
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => {
                self.c.aggregates += aggregates.len() as u32 + u32::from(!variables.is_empty());
                self.elements(inner)
            }
            GraphPattern::Project { inner, .. } => {
                // a nested SELECT
                self.c.subqueries += 1;
                self.group(strip_modifiers(inner));
                1
            }
            GraphPattern::Slice { inner, .. }
            | GraphPattern::Distinct { inner }
            | GraphPattern::Reduced { inner }
            | GraphPattern::OrderBy { inner, .. } => self.elements(inner),
        }
    }

    /// Negations written in an expression: `NOT EXISTS`, and `!BOUND` when the pattern
    /// has an OPTIONAL.
    fn expression(&mut self, s: &str, optional: bool) {
        let n = s.matches("NOT EXISTS").count() as u32;
        self.c.negations += n;
        if optional && s.contains("!BOUND(") {
            self.c.negations += 1;
        }
    }
}

fn strip_extend(p: &GraphPattern) -> &GraphPattern {
    match p {
        GraphPattern::Extend { inner, .. } => strip_extend(inner),
        p => p,
    }
}

fn has_left_join(p: &GraphPattern) -> bool {
    match p {
        GraphPattern::LeftJoin { .. } => true,
        GraphPattern::Join { left, right } => has_left_join(left) || has_left_join(right),
        GraphPattern::Filter { inner, .. } | GraphPattern::Extend { inner, .. } => {
            has_left_join(inner)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(q: &str) -> Complexity {
        let q = format!(
            "PREFIX ex: <http://example.org/ontology#>\nPREFIX foaf: <http://xmlns.com/foaf/0.1/>\n{q}"
        );
        of(&spargebra::SparqlParser::new().parse_query(&q).unwrap())
    }

    /// A39: one triple pattern scores 0; two aggregates, a subquery and a MINUS score
    /// at least 8.
    #[test]
    fn scores() {
        let one = score("SELECT ?p WHERE { ?p a ex:Person }");
        assert_eq!(one.score(), 0, "{one:?}");
        assert_eq!(one.bucket(), "simple");
        let hard = score(
            "SELECT ?team (COUNT(?p) AS ?n) (AVG(?s) AS ?avg) WHERE {
               ?p ex:memberOf ?team ; ex:salary ?s .
               { SELECT ?team WHERE { ?team a ex:Team } }
               MINUS { ?p a ex:Manager }
             } GROUP BY ?team",
        );
        assert!(hard.score() >= 8, "{hard:?}");
        assert_eq!(
            (hard.aggregates, hard.subqueries, hard.negations),
            (3, 1, 1)
        );
        let neg = score(
            "SELECT ?p WHERE { ?p a ex:Person OPTIONAL { ?p ex:manages ?t } FILTER(!BOUND(?t)) FILTER NOT EXISTS { ?p ex:memberOf ?x } }",
        );
        assert_eq!(neg.negations, 2, "{neg:?}");
        let having = score(
            "SELECT ?t (COUNT(?p) AS ?n) WHERE { ?p ex:memberOf ?t } GROUP BY ?t HAVING (COUNT(?p) > 3)",
        );
        assert_eq!(having.aggregates, 3, "{having:?}");
    }
}
