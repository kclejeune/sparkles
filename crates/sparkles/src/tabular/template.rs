//! CONSTRUCT templates in the style of Tarql: the table's rows are a `VALUES` block at
//! the start of the WHERE clause, and the template's triples are written for each
//! solution. Rows are evaluated in batches with the engine's SPARQL evaluator against an
//! empty dataset (spec C05 §5).

use crate::error::{Error, Result};
use crate::sparql::QueryOptions;
use crate::store::{Snapshot, Store};
use oxrdf::{BlankNode, NamedOrBlankNode, Term, Triple, Variable};
use spargebra::Query;
use spargebra::algebra::GraphPattern;
use spargebra::term::GroundTerm;
use std::sync::Arc;

/// A parsed and checked CONSTRUCT template.
pub struct Template {
    query: Query,
    prefixes: Vec<(String, String)>,
}

impl Template {
    /// Parse a template. Refuses anything that is not a CONSTRUCT, dataset clauses,
    /// top-level modifiers and aggregates, and SERVICE.
    pub fn parse(text: &str, base: Option<&str>) -> Result<Template> {
        let query = crate::sparql::parse_query(text, base, &[])?;
        let refuse = |what: &str| {
            Err(Error::invalid(format!(
                "a CSV template cannot use {what}: rows are mapped in batches, one solution per row"
            )))
        };
        let Query::Construct {
            dataset,
            pattern,
            graph_templates,
            ..
        } = &query
        else {
            return Err(Error::invalid("a CSV template must be a CONSTRUCT query"));
        };
        if !graph_templates.is_empty() {
            return Err(Error::invalid(
                "a CSV template cannot use GRAPH in its template: the rows are loaded into the \
                 graph the load names",
            ));
        }
        if dataset.is_some() {
            return Err(Error::invalid(
                "a CSV template cannot use FROM or FROM NAMED: it reads the table only",
            ));
        }
        let mut top = pattern;
        loop {
            match top {
                GraphPattern::OrderBy { .. } => return refuse("ORDER BY"),
                GraphPattern::Slice { .. } => return refuse("LIMIT or OFFSET"),
                GraphPattern::Group { .. } => return refuse("GROUP BY or an aggregate"),
                GraphPattern::Distinct { inner }
                | GraphPattern::Reduced { inner }
                | GraphPattern::Project { inner, .. } => top = inner,
                _ => break,
            }
        }
        if uses_service(pattern) {
            return Err(Error::invalid(
                "a CSV template cannot use SERVICE: an import does not reach other endpoints",
            ));
        }
        Ok(Template {
            query,
            prefixes: declared_prefixes(text),
        })
    }

    /// The `PREFIX` declarations of the query text, for Turtle output.
    pub fn prefixes(&self) -> &[(String, String)] {
        &self.prefixes
    }

    /// The query with `values` at the start of its WHERE clause.
    fn with_values(&self, values: GraphPattern) -> Query {
        match &self.query {
            Query::Construct {
                template,
                graph_templates,
                dataset,
                pattern,
                base_iri,
            } => Query::Construct {
                template: template.clone(),
                graph_templates: graph_templates.clone(),
                dataset: dataset.clone(),
                // the parser projects a CONSTRUCT's pattern onto the WHERE clause's
                // variables: the values go inside that projection, with their own
                // variables, which the template may use without the WHERE clause naming
                // them: they are projected too
                pattern: match pattern {
                    GraphPattern::Project { inner, variables } => {
                        let mut variables = variables.clone();
                        if let GraphPattern::Values { variables: vs, .. } = &values {
                            for v in vs {
                                if !variables.contains(v) {
                                    variables.push(v.clone());
                                }
                            }
                        }
                        GraphPattern::Project {
                            inner: Box::new(prepend((**inner).clone(), values)),
                            variables,
                        }
                    }
                    p => prepend(p.clone(), values),
                },
                base_iri: base_iri.clone(),
            },
            _ => unreachable!("checked by parse"),
        }
    }
}

/// Runs a template over batches of rows.
pub(crate) struct Runner<'a> {
    template: &'a Template,
    snap: Arc<Snapshot>,
    opts: QueryOptions,
    batch: u64,
    part: usize,
}

impl<'a> Runner<'a> {
    pub fn new(template: &'a Template, opts: &QueryOptions, part: usize) -> Runner<'a> {
        let mut opts = opts.clone();
        opts.allow_service = false;
        opts.forbid_service = true;
        opts.no_cache = true;
        opts.initial_bindings.clear();
        Runner {
            template,
            snap: Store::in_memory(Default::default()).snapshot(),
            opts,
            batch: 0,
            part,
        }
    }

    /// Evaluate one batch: `rows` bound to `vars`. Blank nodes are relabelled so that
    /// no two batches share one.
    pub fn run(
        &mut self,
        vars: &[Variable],
        rows: Vec<Vec<Option<GroundTerm>>>,
        sink: &mut dyn FnMut(Triple) -> Result<()>,
    ) -> Result<u64> {
        self.batch += 1;
        let q = self.template.with_values(GraphPattern::Values {
            variables: vars.to_vec(),
            bindings: rows,
        });
        let r = crate::sparql::execute_query(self.snap.clone(), &q, &self.opts, 0.0)?;
        let (p, b) = (self.part, self.batch);
        let relabel = |n: &BlankNode| BlankNode::new_unchecked(format!("t{p}b{b}x{}", n.as_str()));
        let mut n = 0;
        for t in r.triples {
            sink(relabel_triple(t, &relabel))?;
            n += 1;
        }
        Ok(n)
    }
}

fn relabel_triple(t: Triple, f: &dyn Fn(&BlankNode) -> BlankNode) -> Triple {
    let s = match t.subject {
        NamedOrBlankNode::BlankNode(b) => NamedOrBlankNode::BlankNode(f(&b)),
        s => s,
    };
    let o = match t.object {
        Term::BlankNode(b) => Term::BlankNode(f(&b)),
        Term::Triple(inner) => Term::Triple(Box::new(relabel_triple(*inner, f))),
        o => o,
    };
    Triple::new(s, t.predicate, o)
}

/// Put `values` before the first element of the group `p`: the leftmost operand of the
/// joins, left joins, minuses, filters and extensions that SPARQL's translation of a
/// group builds from left to right.
fn prepend(p: GraphPattern, values: GraphPattern) -> GraphPattern {
    match p {
        GraphPattern::Join { left, right } => GraphPattern::Join {
            left: Box::new(prepend(*left, values)),
            right,
        },
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } => GraphPattern::LeftJoin {
            left: Box::new(prepend(*left, values)),
            right,
            expression,
        },
        GraphPattern::Minus { left, right } => GraphPattern::Minus {
            left: Box::new(prepend(*left, values)),
            right,
        },
        GraphPattern::Filter { expr, inner } => GraphPattern::Filter {
            expr,
            inner: Box::new(prepend(*inner, values)),
        },
        GraphPattern::Extend {
            inner,
            variable,
            expression,
        } => GraphPattern::Extend {
            inner: Box::new(prepend(*inner, values)),
            variable,
            expression,
        },
        GraphPattern::Bgp { patterns } if patterns.is_empty() => values,
        other => GraphPattern::Join {
            left: Box::new(values),
            right: Box::new(other),
        },
    }
}

fn uses_service(p: &GraphPattern) -> bool {
    match p {
        GraphPattern::Service { .. } => true,
        GraphPattern::Join { left, right }
        | GraphPattern::LeftJoin { left, right, .. }
        | GraphPattern::Union { left, right }
        | GraphPattern::Minus { left, right } => uses_service(left) || uses_service(right),
        GraphPattern::Filter { inner, .. }
        | GraphPattern::Graph { inner, .. }
        | GraphPattern::Extend { inner, .. }
        | GraphPattern::OrderBy { inner, .. }
        | GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. }
        | GraphPattern::Group { inner, .. } => uses_service(inner),
        _ => false,
    }
}

fn declared_prefixes(text: &str) -> Vec<(String, String)> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?i)\bPREFIX\s+([A-Za-z][\w.-]*)?:\s*<([^>\s]*)>").expect("a regex")
    });
    re.captures_iter(text)
        .map(|c| {
            (
                c.get(1).map_or("", |m| m.as_str()).to_string(),
                c[2].to_string(),
            )
        })
        .collect()
}

/// The SPARQL variable for a column: its name with every character a variable name
/// cannot hold replaced by `_`.
pub fn var_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || (!c.is_ascii() && c.is_alphanumeric()) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() || Variable::new(s.as_str()).is_err() {
        "_".into()
    } else {
        s
    }
}

/// Tarql's names for the columns of a table without a header: `a` … `z`, `aa`, `ab` ….
pub fn letter_name(i: usize) -> String {
    let mut n = i + 1;
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push(b'a' + (n % 26) as u8);
        n /= 26;
    }
    s.reverse();
    String::from_utf8(s).expect("ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(var_name("Person ID"), "Person_ID");
        assert_eq!(var_name("née"), "née");
        assert_eq!(var_name("a-b.c"), "a_b_c");
        assert_eq!(letter_name(0), "a");
        assert_eq!(letter_name(25), "z");
        assert_eq!(letter_name(26), "aa");
        assert_eq!(letter_name(27), "ab");
    }

    #[test]
    fn refusals() {
        for q in [
            "SELECT * WHERE {}",
            "CONSTRUCT { ?s ?p ?o } FROM <http://e/g> WHERE { BIND(1 AS ?o) }",
            "CONSTRUCT { ?s ?p ?o } WHERE { BIND(1 AS ?o) } LIMIT 10",
            "CONSTRUCT { ?s ?p ?o } WHERE { BIND(1 AS ?o) } ORDER BY ?o",
            "CONSTRUCT { ?s ?p ?o } WHERE { SERVICE <http://e/sparql> { ?s ?p ?o } }",
        ] {
            assert!(Template::parse(q, None).is_err(), "{q}");
        }
        let t = Template::parse(
            "PREFIX ex: <http://e/>\nprefix : <http://d/> CONSTRUCT { ?s ex:p ?o } WHERE { BIND(1 AS ?o) }",
            None,
        )
        .unwrap();
        assert_eq!(
            t.prefixes(),
            [
                ("ex".to_string(), "http://e/".to_string()),
                (String::new(), "http://d/".to_string())
            ]
        );
    }
}

#[cfg(test)]
mod graph_template_tests {
    #[test]
    fn graph_blocks_in_the_template_are_refused() {
        let e = super::Template::parse(
            "CONSTRUCT { GRAPH <http://ex.org/g> { ?s <http://ex.org/p> ?o } } WHERE {}",
            None,
        )
        .err()
        .expect("refused")
        .to_string();
        assert!(e.contains("GRAPH"), "{e}");
    }
}
