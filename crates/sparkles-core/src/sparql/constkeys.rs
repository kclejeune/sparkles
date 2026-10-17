//! The vocabulary keys of the constants of a query (spec X07 §5.1).
//!
//! The planner resolves each constant to an id with one vocabulary lookup, one after the
//! other. On a cold server each lookup in a vocabulary group not read before waits for that
//! group. Visiting the keys first lets [`KeyPrefetch`](crate::vocab::KeyPrefetch) ask for
//! every group at once.

use crate::id;
use oxrdf::Literal;
use spargebra::algebra::{Expression, GraphPattern, PropertyPathExpression};
use spargebra::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};

/// The most keys visited: the first groups the lookups of a `VALUES` list of thousands of
/// constants reach are the ones worth asking for.
pub(crate) const MAX_KEYS: usize = 4096;

/// Call `f` with the key of each IRI and non-inline literal that `pattern` names, with
/// duplicates, up to [`MAX_KEYS`]. Constants of expressions are included for the usual
/// comparisons and calls. The keys are written one after another into one buffer, so a
/// warm query allocates nothing for them.
pub(crate) fn for_each_key(pattern: &GraphPattern, f: &mut dyn FnMut(&[u8])) {
    let mut keys = Keys {
        buf: Vec::new(),
        n: 0,
        f,
    };
    keys.pattern(pattern);
}

/// The keys [`for_each_key`] visits, collected.
#[cfg(test)]
pub(crate) fn constant_keys(pattern: &GraphPattern) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for_each_key(pattern, &mut |k| out.push(k.to_vec()));
    out
}

struct Keys<'f> {
    buf: Vec<u8>,
    /// the keys visited so far
    n: usize,
    f: &'f mut dyn FnMut(&[u8]),
}

impl Keys<'_> {
    fn emit(&mut self) {
        (self.f)(&self.buf);
        self.n += 1;
    }

    fn literal(&mut self, l: &Literal) {
        if self.n < MAX_KEYS && id::inline_literal(l.value(), l.datatype().as_str()).is_none() {
            self.buf.clear();
            id::write_literal_key(l, &mut self.buf);
            self.emit();
        }
    }

    fn iri(&mut self, iri: &str) {
        if self.n < MAX_KEYS {
            self.buf.clear();
            self.buf.push(b'<');
            self.buf.extend_from_slice(iri.as_bytes());
            self.emit();
        }
    }

    fn term_pattern(&mut self, t: &TermPattern) {
        match t {
            TermPattern::NamedNode(n) => self.iri(n.as_str()),
            TermPattern::Literal(l) => self.literal(l),
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
            Expression::Literal(l) => self.literal(l),
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
                            GroundTerm::Literal(l) => self.literal(l),
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

    /// The keys written here are the vocabulary's own keys of the same terms, so the prefetch
    /// asks for the groups the lookups read.
    #[test]
    fn keys_are_the_vocabulary_keys_of_the_terms() {
        use oxrdf::{Literal, NamedNode, Term};
        let xsd =
            |t: &str| NamedNode::new(format!("http://www.w3.org/2001/XMLSchema#{t}")).unwrap();
        let iri = NamedNode::new("http://example.org/a").unwrap();
        let terms: Vec<Term> = vec![
            iri.clone().into(),
            Literal::new_simple_literal("plain").into(),
            Literal::new_language_tagged_literal("chat", "fr")
                .unwrap()
                .into(),
            Literal::new_typed_literal("2024-01-01T00:00:00Z", xsd("dateTimeStamp")).into(),
            Literal::new_typed_literal("007", xsd("integer")).into(),
            Literal::new_typed_literal("7", xsd("integer")).into(),
        ];
        let text = |t: &Term| match t {
            Term::NamedNode(n) => format!("<{}>", n.as_str()),
            Term::Literal(l) => l.to_string(),
            _ => unreachable!(),
        };
        let values: Vec<String> = terms.iter().map(text).collect();
        let parsed = spargebra::SparqlParser::new()
            .parse_query(&format!(
                "SELECT * WHERE {{ VALUES ?x {{ {} }} ?x <{}> ?y FILTER(?y = \"plain\") }}",
                values.join(" "),
                iri.as_str()
            ))
            .unwrap();
        let spargebra::Query::Select { pattern, .. } = parsed else {
            panic!()
        };
        let got = constant_keys(&pattern);
        // the canonical integer is inline and has no key, the others each have theirs
        let mut want: Vec<Vec<u8>> = terms
            .iter()
            .filter(|t| id::inline_id(t).is_none())
            .map(id::term_key)
            .collect();
        want.push(id::term_key(&iri.clone().into()));
        want.push(id::term_key(&Literal::new_simple_literal("plain").into()));
        let (mut got_sorted, mut want_sorted) = (got.clone(), want.clone());
        got_sorted.sort();
        want_sorted.sort();
        assert_eq!(got_sorted, want_sorted);
        assert!(id::inline_id(&terms[5]).is_some() && id::inline_id(&terms[4]).is_none());
    }

    #[test]
    fn collects_at_most_the_cap() {
        let values: String = (0..10_000).map(|i| format!("<urn:v{i}> ")).collect();
        let parsed = spargebra::SparqlParser::new()
            .parse_query(&format!("SELECT * WHERE {{ VALUES ?x {{ {values} }} }}"))
            .unwrap();
        let spargebra::Query::Select { pattern, .. } = parsed else {
            panic!()
        };
        assert_eq!(constant_keys(&pattern).len(), MAX_KEYS);
    }
}
