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

use spargebra::algebra::{Expression, Function, GraphPattern};
use spargebra::term::{TriplePattern, Variable};

/// The most rows of the tables moved into the branches, over every table of the join.
const MAX_ROWS: usize = 10_000;

/// The most rows the branches receive in all: the rows moved times the copies made of
/// them, one per branch.
const MAX_COPIED_ROWS: usize = 2 * MAX_ROWS;

#[cfg(test)]
thread_local! {
    /// Turns the rewrite off in tests, to compare answers with and without it.
    pub static DISABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The pattern with the VALUES tables of a join moved into the branches of a UNION of the
/// same join that shares variables with them, or `None` when the join has no such pair.
/// `calls` says whether a basic graph pattern calls a property function: a UNION that
/// holds one, a SERVICE or a LATERAL keeps the tables outside, because a call there would
/// see the table's variables bound where it saw them unbound.
pub(super) fn rewrite(
    gp: &GraphPattern,
    calls: &dyn Fn(&[TriplePattern]) -> bool,
) -> Option<GraphPattern> {
    #[cfg(test)]
    if DISABLED.get() {
        return None;
    }
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
    // the candidates are borrowed until every limit and guard has passed, so a table that
    // stays outside is never copied
    let mut taken = vec![false; operands.len()];
    let mut rows = 0usize;
    for (i, o) in operands.iter().enumerate() {
        if let GraphPattern::Values {
            variables,
            bindings,
        } = o
            && variables.iter().any(|v| scope.contains(v))
        {
            rows += bindings.len();
            taken[i] = true;
        }
    }
    // all the tables or none: a table left behind would be moved by the pass that plans
    // the result, past the bound
    if !taken.contains(&true)
        || rows > MAX_ROWS
        || rows.saturating_mul(copies(operands[union])) > MAX_COPIED_ROWS
        || opaque(operands[union], calls)
    {
        return None;
    }
    let mut moved = Vec::new();
    let mut vars: Vec<Variable> = Vec::new();
    for (i, o) in operands.iter().enumerate() {
        if taken[i]
            && let GraphPattern::Values { variables, .. } = o
        {
            moved.push((*o).clone());
            vars.extend(variables.iter().cloned());
        }
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

/// How many copies of a table moving into `gp` can end up in its branches, here and in
/// later passes over the branches: each UNION in it splits one copy in two.
fn copies(gp: &GraphPattern) -> usize {
    let mut n = 1usize;
    walk(gp, &mut |p| {
        if matches!(p, GraphPattern::Union { .. }) {
            n += 1;
        }
        false
    });
    n
}

/// Whether `gp` holds a pattern whose answer may depend on the bindings it is evaluated
/// with beyond a join: a SERVICE, a LATERAL or a property function call.
fn opaque(gp: &GraphPattern, calls: &dyn Fn(&[TriplePattern]) -> bool) -> bool {
    walk(gp, &mut |p| match p {
        GraphPattern::Service { .. } => true,
        GraphPattern::Lateral { .. } => true,
        GraphPattern::Bgp { patterns } => calls(patterns),
        _ => false,
    })
}

/// Calls `f` on `gp` and the patterns under it, outside expressions, until it returns
/// true; returns whether it did.
fn walk(gp: &GraphPattern, f: &mut dyn FnMut(&GraphPattern) -> bool) -> bool {
    use GraphPattern as GP;
    if f(gp) {
        return true;
    }
    match gp {
        GP::Bgp { .. } | GP::Path { .. } | GP::Values { .. } => false,
        GP::Join { left, right }
        | GP::LeftJoin { left, right, .. }
        | GP::Union { left, right }
        | GP::Minus { left, right }
        | GP::SemiJoin { left, right }
        | GP::AntiJoin { left, right } => walk(left, f) || walk(right, f),
        GP::Lateral { left, right } => walk(left, f) || walk(right, f),
        GP::Filter { inner, .. }
        | GP::Graph { inner, .. }
        | GP::Extend { inner, .. }
        | GP::Assign { inner, .. }
        | GP::Unfold { inner, .. }
        | GP::OrderBy { inner, .. }
        | GP::Project { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. }
        | GP::Group { inner, .. }
        | GP::Service { inner, .. } => walk(inner, f),
    }
}

/// Whether evaluating an expression once per solution of the branch and once per solution
/// joined with the table may differ: it names one of `vars`, holds an EXISTS, whose
/// pattern this does not look into, or calls a function whose value is not fixed by its
/// arguments, such as `RAND` or `UUID`. Custom functions count as such, but for the XSD
/// casts. `NOW` has one value in a query.
fn reads(e: &Expression, vars: &[Variable]) -> bool {
    use Expression as E;
    match e {
        E::NamedNode(_) | E::Literal(_) => false,
        E::Variable(v) | E::Bound(v) => vars.contains(v),
        E::Exists(_) => true,
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
        | E::Divide(a, b) => reads(a, vars) || reads(b, vars),
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => reads(a, vars),
        E::In(a, list) => reads(a, vars) || list.iter().any(|x| reads(x, vars)),
        E::If(a, b, c) => reads(a, vars) || reads(b, vars) || reads(c, vars),
        E::Coalesce(list) => list.iter().any(|x| reads(x, vars)),
        E::FunctionCall(f, args) => {
            let fixed = match f {
                Function::Rand | Function::Uuid | Function::StrUuid | Function::BNode => false,
                Function::Custom(iri) => iri
                    .as_str()
                    .starts_with("http://www.w3.org/2001/XMLSchema#"),
                _ => true,
            };
            !fixed || args.iter().any(|x| reads(x, vars))
        }
    }
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

    fn no_calls(_: &[TriplePattern]) -> bool {
        false
    }

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
        let got = rewrite(&gp, &no_calls).unwrap();
        assert_eq!(got.to_string(), want.to_string());
    }

    #[test]
    fn values_stay_outside_what_reads_them() {
        // the BIND reads ?l and the FILTER holds EXISTS: the table joins outside them
        let gp = body(
            "SELECT * WHERE { VALUES ?l { 1 } { { ?s ?p ?l } BIND(?l + 1 AS ?m) } UNION { ?s ?p ?l FILTER EXISTS { ?s ?q ?l } } }",
        );
        let got = rewrite(&gp, &no_calls).unwrap().to_string();
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
        // without the rewrite the branches read every quad and pass the budget, which
        // shows that the switch of the differential test reaches the planner
        DISABLED.set(true);
        let off = rows(&s, outside, Some(1 << 20));
        DISABLED.set(false);
        assert!(off.is_err(), "{off:?}");
    }

    #[test]
    fn nothing_to_move() {
        // no union, a table without the union's variables, no table
        for q in [
            "SELECT * WHERE { VALUES ?l { 1 } ?s ?p ?l }",
            "SELECT * WHERE { VALUES ?z { 1 } { ?s ?p ?l } UNION { ?l ?p ?s } }",
            "SELECT * WHERE { ?x ?y ?z { ?s ?p ?l } UNION { ?l ?p ?s } }",
        ] {
            assert!(rewrite(&body(q), &no_calls).is_none(), "{q}");
        }
    }

    #[test]
    fn other_operands_keep_their_place() {
        let gp = body(
            "SELECT * WHERE { ?a ?b ?l VALUES ?l { 1 2 } { ?s ?p ?l } UNION { ?l ?p ?s } ?l ?c ?d }",
        );
        let got = rewrite(&gp, &no_calls).unwrap().to_string();
        let want = body(
            "SELECT * WHERE { ?a ?b ?l { VALUES ?l { 1 2 } ?s ?p ?l } UNION { VALUES ?l { 1 2 } ?l ?p ?s } ?l ?c ?d }",
        );
        assert_eq!(got, want.to_string());
    }

    /// The rows of `q` with the rewrite on and off.
    fn both(s: &crate::store::Store, q: &str) -> (Vec<String>, Vec<String>) {
        let on = rows(s, q, None).unwrap();
        DISABLED.set(true);
        let off = rows(s, q, None);
        DISABLED.set(false);
        (on, off.unwrap())
    }

    fn differential_store() -> crate::store::Store {
        let s = crate::store::Store::in_memory(Default::default());
        let mut trig = String::new();
        for i in 0..40 {
            trig.push_str(&format!(
                "<urn:s{i}> <urn:p> {} . <urn:s{i}> <urn:q> \"v{}\" .\n",
                i % 7,
                i % 5
            ));
            if i % 3 == 0 {
                trig.push_str(&format!("<urn:s{i}> <urn:r> <urn:s{}> .\n", (i + 1) % 40));
            }
        }
        for g in ["urn:g1", "urn:g2"] {
            trig.push_str(&format!("<{g}> {{\n"));
            for i in 0..20 {
                trig.push_str(&format!(
                    "<urn:t{i}> <urn:p> {} . <urn:t{i}> <urn:q> \"v{}\" .\n",
                    i % 4,
                    i % 3
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
        s
    }

    #[test]
    fn answers_are_the_same_with_and_without_the_rewrite() {
        let s = differential_store();
        let queries = [
            // UNDEF and duplicate rows
            "SELECT * WHERE { VALUES (?o ?l) { (1 \"v1\") (1 \"v1\") (UNDEF \"v2\") (3 UNDEF) } { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } }",
            // a variable bound in one branch only
            "SELECT * WHERE { VALUES ?o { 1 2 } { ?s <urn:p> ?o } UNION { ?s <urn:r> ?x } }",
            // OPTIONAL and MINUS in a branch
            "SELECT * WHERE { VALUES ?o { 1 2 UNDEF } { ?s <urn:p> ?o OPTIONAL { ?s <urn:r> ?x } } UNION { ?s <urn:q> ?l MINUS { ?s <urn:p> ?o } } }",
            "SELECT * WHERE { VALUES ?x { <urn:s1> <urn:s4> } { ?s <urn:p> ?o OPTIONAL { ?s <urn:r> ?x } } UNION { ?s <urn:r> ?x } }",
            // FILTER and BIND, reading the table or not
            "SELECT * WHERE { VALUES ?o { 1 2 3 } { ?s <urn:p> ?o FILTER(?o > 1) } UNION { ?s <urn:p> ?o FILTER(STRSTARTS(STR(?s), \"urn:s1\")) BIND(STR(?s) AS ?n) } }",
            "SELECT * WHERE { VALUES ?n { \"urn:s1\" } { ?s <urn:p> ?o BIND(STR(?s) AS ?n) } UNION { ?s <urn:q> ?l BIND(\"urn:s1\" AS ?n) } }",
            "SELECT * WHERE { VALUES ?o { 1 } { ?s <urn:p> ?o FILTER NOT EXISTS { ?s <urn:r> ?x } } UNION { ?s <urn:p> ?o FILTER(BOUND(?o)) } }",
            // nested unions and graphs
            "SELECT * WHERE { VALUES ?o { 0 2 } { { ?s <urn:p> ?o } UNION { GRAPH ?g { ?s <urn:p> ?o } } } UNION { GRAPH <urn:g1> { ?s <urn:p> ?o } } }",
            "SELECT * WHERE { VALUES ?l { \"v0\" \"v2\" } { ?s <urn:q> ?l } UNION { GRAPH ?g { ?s <urn:q> ?l } FILTER(?g != <urn:g2>) } }",
            // two tables, one of them on a variable the union does not bind
            "SELECT * WHERE { VALUES ?o { 1 2 } VALUES ?l { \"v1\" UNDEF } { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } }",
            "SELECT * WHERE { VALUES ?o { 1 2 } VALUES ?z { 7 8 } { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } }",
            // a subquery that projects the table's variable away
            "SELECT * WHERE { VALUES ?o { 1 } { SELECT ?s WHERE { ?s <urn:p> ?o } } UNION { ?s <urn:p> ?o } }",
            // the table inside OPTIONAL, and other operands of the join
            "SELECT * WHERE { ?s <urn:q> ?l OPTIONAL { VALUES ?o { 1 2 } { ?s <urn:p> ?o } UNION { ?s <urn:r> ?x } } }",
            "SELECT * WHERE { ?s <urn:q> ?l VALUES ?o { 1 2 } { ?s <urn:p> ?o } UNION { GRAPH ?g { ?s <urn:p> ?o } } }",
            // aggregates over the result count the multiplicities
            "SELECT ?o (COUNT(*) AS ?n) WHERE { VALUES ?o { 1 1 2 } { ?s <urn:p> ?o } UNION { GRAPH ?g { ?s <urn:p> ?o } } } GROUP BY ?o",
        ];
        for q in queries {
            let (on, off) = both(&s, q);
            assert_eq!(on, off, "{q}");
        }
        // the rewrite fires on these shapes
        assert!(rewrite(&body(queries[0]), &no_calls).is_some());
        assert!(rewrite(&body(queries[9]), &no_calls).is_some());
    }

    #[test]
    fn a_non_deterministic_bind_keeps_the_table_outside() {
        for f in ["UUID()", "STRUUID()", "RAND()", "BNODE()"] {
            let q = format!(
                "SELECT * WHERE {{ VALUES ?l {{ 1 1 }} {{ ?s ?p ?l BIND({f} AS ?u) }} UNION {{ ?s ?q ?l FILTER(RAND() < 2) }} }}"
            );
            let got = rewrite(&body(&q), &no_calls).unwrap().to_string();
            let want = body(&format!(
                "SELECT * WHERE {{ {{ VALUES ?l {{ 1 1 }} {{ ?s ?p ?l BIND({f} AS ?u) }} }} UNION {{ VALUES ?l {{ 1 1 }} {{ ?s ?q ?l FILTER(RAND() < 2) }} }} }}"
            ));
            assert_eq!(got, want.to_string(), "{f}");
        }
        // the duplicate rows share one value, as without the rewrite
        let s = differential_store();
        let q = "SELECT ?s ?u WHERE { VALUES ?o { 1 1 } { ?s <urn:p> ?o BIND(STRUUID() AS ?u) } UNION { ?s <urn:r> ?o } }";
        let r = rows(&s, q, None).unwrap();
        assert_eq!(r.len(), 2 * 6, "{r:?}");
        for pair in r.chunks(2) {
            assert_eq!(pair[0], pair[1]);
        }
        // NOW() is the same in every row of a query: the table moves
        let q =
            "SELECT * WHERE { VALUES ?l { 1 } { ?s ?p ?l BIND(NOW() AS ?t) } UNION { ?s ?q ?l } }";
        let want = "SELECT * WHERE { { { VALUES ?l { 1 } ?s ?p ?l } BIND(NOW() AS ?t) } UNION { VALUES ?l { 1 } ?s ?q ?l } }";
        assert_eq!(
            rewrite(&body(q), &no_calls).unwrap().to_string(),
            body(want).to_string()
        );
    }

    #[test]
    fn the_rows_moved_are_bounded_over_every_branch() {
        let table = |var: &str, n: usize| {
            let rows: Vec<String> = (0..n).map(|i| i.to_string()).collect();
            format!("VALUES ?{var} {{ {} }}", rows.join(" "))
        };
        // one table at the limit goes into two branches
        let q = format!(
            "SELECT * WHERE {{ {} {{ ?s ?p ?o }} UNION {{ ?o ?p ?s }} }}",
            table("o", MAX_ROWS / 2)
        );
        assert!(rewrite(&body(&q), &no_calls).is_some());
        // five tables that each fit, but not together, stay outside
        let q = format!(
            "SELECT * WHERE {{ {} {} {} {} {} {{ ?a ?b ?c . ?c ?d ?e }} UNION {{ ?e ?d ?c }} }}",
            table("a", MAX_ROWS - 1),
            table("b", MAX_ROWS - 1),
            table("c", MAX_ROWS - 1),
            table("d", MAX_ROWS - 1),
            table("e", MAX_ROWS - 1),
        );
        assert!(rewrite(&body(&q), &no_calls).is_none());
        // a small table into many branches
        let branches: Vec<String> = (0..64).map(|i| format!("{{ ?s <urn:p{i}> ?o }}")).collect();
        let q = format!(
            "SELECT * WHERE {{ {} {} }}",
            table("o", 1_000),
            branches.join(" UNION ")
        );
        assert!(rewrite(&body(&q), &no_calls).is_none());
        let q = format!(
            "SELECT * WHERE {{ {} {} }}",
            table("o", 10),
            branches.join(" UNION ")
        );
        assert!(rewrite(&body(&q), &no_calls).is_some());
    }

    #[test]
    fn calls_service_and_lateral_keep_the_table_outside() {
        let calls =
            |ps: &[TriplePattern]| ps.iter().any(|t| t.predicate.to_string() == "<urn:call>");
        for q in [
            "SELECT * WHERE { VALUES ?l { 1 } { ?s <urn:call> ?l } UNION { ?s ?p ?l } }",
            "SELECT * WHERE { VALUES ?l { 1 } { SERVICE <urn:x> { ?s ?p ?l } } UNION { ?s ?p ?l } }",
            "SELECT * WHERE { VALUES ?l { 1 } { ?s ?p ?l LATERAL { SELECT ?q WHERE { ?s ?q ?l } LIMIT 1 } } UNION { ?s ?p ?l } }",
        ] {
            assert!(rewrite(&body(q), &calls).is_none(), "{q}");
        }
        let q = "SELECT * WHERE { VALUES ?l { 1 } { ?s <urn:other> ?l } UNION { ?s ?p ?l } }";
        assert!(rewrite(&body(q), &calls).is_some());
    }
}
