//! VALUES pushed into the branches of a UNION it is joined with.
//!
//! `{ VALUES ?l { "Berlin"@en } { ?s ?p ?l } UNION { GRAPH ?g { ?s ?p ?l } } }` joins a
//! constant table with a UNION. Planned as written, each branch is planned on its own and
//! reads every quad of `?s ?p ?l`, and the join with the table comes after. On a billion
//! quads that passes any memory budget, although each branch would be a few index lookups
//! with the table's values bound. The join distributes over the union, as a multiset join
//! does over a multiset union, so the table can move into each branch:
//! `{ VALUES … { ?s ?p ?l } } UNION { VALUES … GRAPH ?g { ?s ?p ?l } }`. In a branch, the
//! table also moves inside a `BIND` and a `FILTER` whose expressions do not mention its
//! variables, where the branch's own group joins it with the patterns by index.

use spargebra::algebra::{Expression, GraphPattern};
use spargebra::term::Variable;

/// The most rows of the tables moved into the branches.
const MAX_ROWS: usize = 10_000;

/// The pattern with the VALUES tables of a join moved into the branches of a UNION of the
/// same join that shares variables with them, or `None` when the join has no such pair.
pub(super) fn rewrite(gp: &GraphPattern) -> Option<GraphPattern> {
    if !matches!(gp, GraphPattern::Join { .. }) {
        return None;
    }
    let mut operands = Vec::new();
    flatten(gp, &mut operands);
    let union = operands
        .iter()
        .position(|o| matches!(o, GraphPattern::Union { .. }))?;
    let mut scope: Vec<Variable> = Vec::new();
    operands[union].on_in_scope_variable(|v| scope.push(v.clone()));
    let mut moved = Vec::new();
    let mut taken = vec![false; operands.len()];
    let mut vars: Vec<Variable> = Vec::new();
    let mut rows = 0usize;
    for (i, o) in operands.iter().enumerate() {
        if let GraphPattern::Values {
            variables,
            bindings,
        } = o
            && variables.iter().any(|v| scope.contains(v))
            && rows + bindings.len() <= MAX_ROWS
        {
            rows += bindings.len();
            moved.push((*o).clone());
            vars.extend(variables.iter().cloned());
            taken[i] = true;
        }
    }
    if moved.is_empty() {
        return None;
    }
    let pushed = push(&moved, &vars, operands[union].clone());
    // the other operands keep their order, with the union where it was
    let mut out: Option<GraphPattern> = None;
    for (i, o) in operands.iter().enumerate() {
        if taken[i] {
            continue;
        }
        let next = if i == union {
            pushed.clone()
        } else {
            (*o).clone()
        };
        out = Some(match out {
            None => next,
            Some(l) => GraphPattern::Join {
                left: Box::new(l),
                right: Box::new(next),
            },
        });
    }
    out
}

/// The operands of a tree of joins, left to right.
fn flatten<'g>(gp: &'g GraphPattern, out: &mut Vec<&'g GraphPattern>) {
    match gp {
        GraphPattern::Join { left, right } if !super::enhancer::is_loop(right) => {
            flatten(left, out);
            flatten(right, out);
        }
        other => out.push(other),
    }
}

/// Whether an expression may read one of `vars`: it names one, or it holds an EXISTS,
/// whose pattern this does not look into.
fn reads(e: &Expression, vars: &[Variable]) -> bool {
    let text = e.to_string();
    if text.contains("EXISTS") || text.contains("exists") {
        return true;
    }
    vars.iter().any(|v| {
        let name = format!("?{}", v.as_str());
        text.match_indices(&name).any(|(i, _)| {
            !text[i + name.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    })
}

/// `tables` joined with `gp`, moved as far in as BINDs, FILTERs and UNIONs allow.
fn push(tables: &[GraphPattern], vars: &[Variable], gp: GraphPattern) -> GraphPattern {
    match gp {
        GraphPattern::Union { left, right } => GraphPattern::Union {
            left: Box::new(push(tables, vars, *left)),
            right: Box::new(push(tables, vars, *right)),
        },
        GraphPattern::Extend {
            inner,
            variable,
            expression,
        } if !vars.contains(&variable) && !reads(&expression, vars) => GraphPattern::Extend {
            inner: Box::new(push(tables, vars, *inner)),
            variable,
            expression,
        },
        GraphPattern::Filter { expr, inner } if !reads(&expr, vars) => GraphPattern::Filter {
            expr,
            inner: Box::new(push(tables, vars, *inner)),
        },
        other => {
            let mut out = other;
            for t in tables.iter().rev() {
                out = GraphPattern::Join {
                    left: Box::new(t.clone()),
                    right: Box::new(out),
                };
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(q: &str) -> GraphPattern {
        match spargebra::SparqlParser::new().parse_query(q).unwrap() {
            spargebra::Query::Select { pattern, .. } => pattern,
            _ => unreachable!(),
        }
    }

    /// The WHERE pattern under the projection.
    fn body(q: &str) -> GraphPattern {
        match pattern(q) {
            GraphPattern::Project { inner, .. } => *inner,
            p => p,
        }
    }

    #[test]
    fn values_move_into_each_branch_and_inside_bind_and_filter() {
        let gp = body(
            "SELECT * WHERE { VALUES ?l { \"Berlin\"@en } { { ?s ?p ?l } BIND(<urn:d> AS ?g) } UNION { GRAPH ?g { ?s ?p ?l } FILTER(?g != <urn:i>) } }",
        );
        let want = body(
            "SELECT * WHERE { { { VALUES ?l { \"Berlin\"@en } ?s ?p ?l } BIND(<urn:d> AS ?g) } UNION { { VALUES ?l { \"Berlin\"@en } GRAPH ?g { ?s ?p ?l } } FILTER(?g != <urn:i>) } }",
        );
        let got = rewrite(&gp).unwrap();
        assert_eq!(got.to_string(), want.to_string());
    }

    #[test]
    fn values_stay_outside_what_reads_them() {
        // the BIND reads ?l and the FILTER holds EXISTS: the table joins outside them
        let gp = body(
            "SELECT * WHERE { VALUES ?l { 1 } { { ?s ?p ?l } BIND(?l + 1 AS ?m) } UNION { ?s ?p ?l FILTER EXISTS { ?s ?q ?l } } }",
        );
        let got = rewrite(&gp).unwrap().to_string();
        let want = body(
            "SELECT * WHERE { { VALUES ?l { 1 } { { ?s ?p ?l } BIND(?l + 1 AS ?m) } } UNION { VALUES ?l { 1 } { ?s ?p ?l FILTER EXISTS { ?s ?q ?l } } } }",
        );
        assert_eq!(got, want.to_string());
    }

    /// The rows of a query, sorted.
    fn rows(s: &crate::store::Store, q: &str, budget: Option<u64>) -> crate::Result<Vec<String>> {
        let opts = crate::sparql::QueryOptions {
            no_cache: true,
            max_memory_bytes: budget,
            ..Default::default()
        };
        let r = crate::sparql::query(s.snapshot(), q, &opts)?;
        let mut v: Vec<String> = r.rows().into_iter().map(|row| format!("{row:?}")).collect();
        v.sort();
        Ok(v)
    }

    #[test]
    fn a_table_outside_a_union_of_graphs_reads_only_its_rows() {
        // 300,000 quads in the default graph and in two named graphs
        let s = crate::store::Store::in_memory(Default::default());
        let mut trig = String::new();
        for i in 0..100_000 {
            trig.push_str(&format!("<urn:s{i}> <urn:p> \"v{i}\" .\n"));
        }
        for g in ["urn:g1", "urn:g2"] {
            trig.push_str(&format!("<{g}> {{\n"));
            for i in 0..50_000 {
                trig.push_str(&format!(
                    "<urn:t{i}> <urn:p> \"v{i}\" . <urn:t{i}> <urn:q> {i} .\n"
                ));
            }
            trig.push_str("}\n");
        }
        s.load(&[crate::io::Source::from_bytes(
            trig.into_bytes(),
            crate::io::RdfFormat::TriG,
            None,
        )])
        .unwrap();
        let outside = "SELECT ?s ?g WHERE { VALUES ?p { <urn:p> } VALUES ?l { \"v7\" \"v70000\" } { { ?s ?p ?l } BIND(<urn:x-arq:DefaultGraph> AS ?g) } UNION { GRAPH ?g { ?s ?p ?l } FILTER(?g != <urn:i>) } }";
        let inside = "SELECT ?s ?g WHERE { { { VALUES ?p { <urn:p> } VALUES ?l { \"v7\" \"v70000\" } ?s ?p ?l } BIND(<urn:x-arq:DefaultGraph> AS ?g) } UNION { { VALUES ?p { <urn:p> } VALUES ?l { \"v7\" \"v70000\" } GRAPH ?g { ?s ?p ?l } } FILTER(?g != <urn:i>) } }";
        let want = rows(&s, inside, None).unwrap();
        assert_eq!(want.len(), 4, "{want:?}");
        assert_eq!(rows(&s, outside, None).unwrap(), want);
        // a budget that the whole branches would pass
        assert_eq!(rows(&s, outside, Some(1 << 20)).unwrap(), want);
    }

    #[test]
    fn nothing_to_move() {
        // no union, a table without the union's variables, no table
        for q in [
            "SELECT * WHERE { VALUES ?l { 1 } ?s ?p ?l }",
            "SELECT * WHERE { VALUES ?z { 1 } { ?s ?p ?l } UNION { ?l ?p ?s } }",
            "SELECT * WHERE { ?x ?y ?z { ?s ?p ?l } UNION { ?l ?p ?s } }",
        ] {
            assert!(rewrite(&body(q)).is_none(), "{q}");
        }
    }

    #[test]
    fn other_operands_keep_their_place() {
        let gp = body(
            "SELECT * WHERE { ?a ?b ?l VALUES ?l { 1 2 } { ?s ?p ?l } UNION { ?l ?p ?s } ?l ?c ?d }",
        );
        let got = rewrite(&gp).unwrap().to_string();
        let want = body(
            "SELECT * WHERE { ?a ?b ?l { VALUES ?l { 1 2 } ?s ?p ?l } UNION { VALUES ?l { 1 2 } ?l ?p ?s } ?l ?c ?d }",
        );
        assert_eq!(got, want.to_string());
    }
}
