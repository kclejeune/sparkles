//! VALUES pushed into the branches of a UNION it is joined with.
//!
//! `{ VALUES ?l { "Berlin"@en } { ?s ?p ?l } UNION { GRAPH ?g { ?s ?p ?l } } }` joins a
//! constant table with a UNION. Planned as written, each branch is planned on its own and
//! reads every quad of `?s ?p ?l`, and the join with the table comes after. On a billion
//! quads that passes any memory budget, although each branch would be a few index lookups
//! with the table's values bound. The join distributes over the union, as a multiset join
//! does over a multiset union, so the table can move into each branch:
//! `{ VALUES … { ?s ?p ?l } } UNION { VALUES … GRAPH ?g { ?s ?p ?l } }`.
//!
//! On its way to the union and in a branch, a table also moves inside a `BIND`, a
//! `FILTER` and a `GRAPH`, where the group under them joins it with the patterns by
//! index. A table passes a `BIND` or `FILTER` whose expression reads none of its
//! variables, or reads only variables that every solution of the pattern under it binds:
//! a solution joined with the table then has the same values of those variables as
//! before the join, so the expression has the same value or error. A variable that the
//! pattern may leave unbound, under an OPTIONAL, a MINUS, a UNION branch or a BIND, keeps
//! the table outside, where an error that removes the solution would otherwise become a
//! value that keeps it. A table with an UNDEF cell in a variable that a FILTER reads stays
//! outside it too, because the planner may test the FILTER on the table's rows before the
//! join fills the cell. A table passes `GRAPH` when a UNION is under it and the table
//! does not name the graph variable, which the inner pattern does not bind. Each table
//! moves on its own, so one that cannot pass a node stays joined above it while the
//! others go in.
//!
//! The rewrite also looks into the `GRAPH` operands of a join, whose inner join the
//! planner flattens into the group without planning it on its own.

use std::cell::OnceCell;

use spargebra::algebra::{Expression, Function, GraphPattern};
use spargebra::term::{GroundTerm, NamedNodePattern, TriplePattern, Variable};

/// The most rows of the tables moved into the branches, over every table of the join.
const MAX_ROWS: usize = 10_000;

/// The most rows the branches receive in all: the rows moved times the copies made of
/// them, one per branch and one per named graph of a `GRAPH ?g` they enter.
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
/// see the table's variables bound where it saw them unbound. `graphs` counts the named
/// graphs of the dataset, which a `GRAPH ?g` plans its inner pattern for one by one.
pub(super) fn rewrite(
    gp: &GraphPattern,
    calls: &dyn Fn(&[TriplePattern]) -> bool,
    graphs: &dyn Fn() -> usize,
) -> Option<GraphPattern> {
    #[cfg(test)]
    if DISABLED.get() {
        return None;
    }
    Cx {
        calls,
        graphs,
        count: OnceCell::new(),
    }
    .rewrite(gp)
}

struct Cx<'a> {
    calls: &'a dyn Fn(&[TriplePattern]) -> bool,
    graphs: &'a dyn Fn() -> usize,
    count: OnceCell<usize>,
}

impl Cx<'_> {
    fn rewrite(&self, gp: &GraphPattern) -> Option<GraphPattern> {
        match gp {
            GraphPattern::Graph { name, inner } => {
                self.rewrite(inner).map(|i| GraphPattern::Graph {
                    name: name.clone(),
                    inner: Box::new(i),
                })
            }
            GraphPattern::Join { .. } => self.join(gp),
            _ => None,
        }
    }

    fn join(&self, gp: &GraphPattern) -> Option<GraphPattern> {
        let mut operands = Vec::new();
        flatten(gp, &mut operands);
        for target in 0..operands.len() {
            if reaches(operands[target])
                && let Some(out) = self.move_into(&operands, target)
            {
                return Some(out);
            }
        }
        // a GRAPH operand's inner join is flattened into this group without being planned
        let mut changed = false;
        let next: Vec<GraphPattern> = operands
            .iter()
            .map(|o| match o {
                GraphPattern::Graph { .. } => match self.rewrite(o) {
                    Some(r) => {
                        changed = true;
                        r
                    }
                    None => (*o).clone(),
                },
                _ => (*o).clone(),
            })
            .collect();
        changed.then(|| joined(next))
    }

    /// The join with its tables that share variables with `operands[target]` moved into
    /// it, or `None` when none can pass the target's top node.
    fn move_into(&self, operands: &[&GraphPattern], target: usize) -> Option<GraphPattern> {
        let mut scope: Vec<Variable> = Vec::new();
        operands[target].on_in_scope_variable(|v| scope.push(v.clone()));
        // the candidates are borrowed until every limit and guard has passed, so a table
        // that stays outside is never copied
        let mut tables: Vec<&GraphPattern> = Vec::new();
        let mut rows = 0usize;
        for o in operands {
            if let GraphPattern::Values {
                variables,
                bindings,
            } = o
                && variables.iter().any(|v| scope.contains(v))
            {
                rows += bindings.len();
                tables.push(o);
            }
        }
        // the limits count every candidate, also those that stay above a node they
        // cannot pass, so a later pass over the result cannot move more rows
        if tables.is_empty()
            || rows > MAX_ROWS
            || rows.saturating_mul(self.copies(operands[target])) > MAX_COPIED_ROWS
            || opaque(operands[target], self.calls)
        {
            return None;
        }
        let (inside, stay) = self.push_into(&tables, operands[target])?;
        let moved = |o: &GraphPattern| {
            tables.iter().any(|t| std::ptr::eq(*t, o)) && !stay.iter().any(|t| std::ptr::eq(*t, o))
        };
        // the other operands keep their order, with the target where it was
        let mut inside = Some(inside);
        let mut out = Vec::new();
        for (i, o) in operands.iter().enumerate() {
            if i == target {
                out.push(inside.take()?);
            } else if !moved(o) {
                out.push((*o).clone());
            }
        }
        Some(joined(out))
    }

    /// The named graphs, counted once.
    fn graph_count(&self) -> usize {
        *self.count.get_or_init(|| (self.graphs)())
    }

    /// How many copies of a table moving into `gp` can end up in it, here and in later
    /// passes over its branches: each UNION in it splits one copy in two, and a
    /// `GRAPH ?g` the table enters makes one per named graph.
    fn copies(&self, gp: &GraphPattern) -> usize {
        match gp {
            GraphPattern::Union { left, right } => {
                self.copies(left).saturating_add(self.copies(right))
            }
            GraphPattern::Graph {
                name: NamedNodePattern::Variable(_),
                inner,
            } if reaches(gp) => self.graph_count().saturating_mul(self.copies(inner)),
            _ => {
                let mut n = 1usize;
                for c in children(gp) {
                    n = n.saturating_add(self.copies(c) - 1);
                }
                n
            }
        }
    }

    /// `gp` with those of `tables` that can pass its top node moved below it, and the
    /// tables that stay above, or `None` when none can pass.
    fn push_into<'t>(
        &self,
        tables: &[&'t GraphPattern],
        gp: &GraphPattern,
    ) -> Option<(GraphPattern, Vec<&'t GraphPattern>)> {
        match gp {
            GraphPattern::Union { left, right } => Some((
                GraphPattern::Union {
                    left: Box::new(self.push(tables, left)),
                    right: Box::new(self.push(tables, right)),
                },
                Vec::new(),
            )),
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } => {
                let open = Open::new(inner);
                let (pass, stay) = split(tables, |vars, _| {
                    !vars.contains(variable) && !reads(expression, &open.of(vars))
                });
                (!pass.is_empty()).then(|| {
                    (
                        GraphPattern::Extend {
                            inner: Box::new(self.push(&pass, inner)),
                            variable: variable.clone(),
                            expression: expression.clone(),
                        },
                        stay,
                    )
                })
            }
            GraphPattern::Filter { expr, inner } => {
                let open = Open::new(inner);
                // the planner may test the FILTER on the table before the join, which
                // fails on a row whose cell is UNDEF although the joined row binds it
                let (pass, stay) = split(tables, |vars, rows| {
                    let mut open = open.of(vars);
                    open.extend(undefined(vars, rows));
                    !reads(expr, &open)
                });
                (!pass.is_empty()).then(|| {
                    (
                        GraphPattern::Filter {
                            expr: expr.clone(),
                            inner: Box::new(self.push(&pass, inner)),
                        },
                        stay,
                    )
                })
            }
            // a GRAPH is entered only for a UNION under it: with plain patterns inside, the
            // planner joins the table with them in the group outside
            GraphPattern::Graph { name, inner } if reaches(gp) => {
                // the inner pattern does not bind the graph variable, the GRAPH does
                let (pass, mut stay) = split(tables, |vars, _| match name {
                    NamedNodePattern::Variable(g) => !vars.contains(g),
                    NamedNodePattern::NamedNode(_) => true,
                });
                if pass.is_empty() {
                    return None;
                }
                // the tables go in only to reach further, as `GRAPH ?g` copies them into
                // the plan of each graph
                let inner = match &**inner {
                    GraphPattern::Join { .. } => {
                        self.join(&joined_with(&pass, (**inner).clone()))?
                    }
                    other => {
                        let (inside, more) = self.push_into(&pass, other)?;
                        stay.extend(more);
                        inside
                    }
                };
                Some((
                    GraphPattern::Graph {
                        name: name.clone(),
                        inner: Box::new(inner),
                    },
                    stay,
                ))
            }
            _ => None,
        }
    }

    /// `tables` joined with `gp`, moved as far in as BINDs, FILTERs, GRAPHs and UNIONs
    /// allow.
    fn push(&self, tables: &[&GraphPattern], gp: &GraphPattern) -> GraphPattern {
        match self.push_into(tables, gp) {
            Some((inside, stay)) => joined_with(&stay, inside),
            None => joined_with(tables, gp.clone()),
        }
    }
}

/// The variables of a table that the pattern under a BIND or FILTER may leave unbound.
struct Open {
    certain: Vec<String>,
}

impl Open {
    fn new(inner: &GraphPattern) -> Self {
        let mut certain = Vec::new();
        super::plan::certain_names(inner, &mut certain);
        Open { certain }
    }

    fn of(&self, vars: &[Variable]) -> Vec<Variable> {
        vars.iter()
            .filter(|v| !self.certain.iter().any(|c| c == v.as_str()))
            .cloned()
            .collect()
    }
}

/// `tables` split into those whose variables and rows pass `ok` and the others.
fn split<'t>(
    tables: &[&'t GraphPattern],
    ok: impl Fn(&[Variable], &[Vec<Option<GroundTerm>>]) -> bool,
) -> (Vec<&'t GraphPattern>, Vec<&'t GraphPattern>) {
    tables.iter().partition(|t| match t {
        GraphPattern::Values {
            variables,
            bindings,
        } => ok(variables, bindings),
        _ => false,
    })
}

/// The variables of a table that are UNDEF in one of its rows.
fn undefined(vars: &[Variable], rows: &[Vec<Option<GroundTerm>>]) -> Vec<Variable> {
    vars.iter()
        .enumerate()
        .filter(|(c, _)| rows.iter().any(|r| r.get(*c).is_none_or(Option::is_none)))
        .map(|(_, v)| v.clone())
        .collect()
}

/// The operands joined left to right.
fn joined(operands: Vec<GraphPattern>) -> GraphPattern {
    operands
        .into_iter()
        .reduce(|l, r| GraphPattern::Join {
            left: Box::new(l),
            right: Box::new(r),
        })
        .unwrap_or_else(|| GraphPattern::Bgp {
            patterns: Vec::new(),
        })
}

/// `tables`, in order, joined with `gp` on their right.
fn joined_with(tables: &[&GraphPattern], gp: GraphPattern) -> GraphPattern {
    let mut out = gp;
    for t in tables.iter().rev() {
        out = GraphPattern::Join {
            left: Box::new((*t).clone()),
            right: Box::new(out),
        };
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

/// Whether a UNION is under `gp` past the nodes a table can move through: BINDs, FILTERs,
/// GRAPHs and the joins under a GRAPH.
fn reaches(gp: &GraphPattern) -> bool {
    match gp {
        GraphPattern::Union { .. } => true,
        GraphPattern::Filter { inner, .. }
        | GraphPattern::Extend { inner, .. }
        | GraphPattern::Graph { inner, .. } => reaches(inner),
        GraphPattern::Join { .. } => {
            let mut operands = Vec::new();
            flatten(gp, &mut operands);
            operands.len() > 1 && operands.iter().any(|o| reaches(o))
        }
        _ => false,
    }
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

/// The patterns directly under `gp`, outside expressions.
fn children(gp: &GraphPattern) -> Vec<&GraphPattern> {
    use GraphPattern as GP;
    match gp {
        GP::Bgp { .. } | GP::Path { .. } | GP::Values { .. } => Vec::new(),
        GP::Join { left, right }
        | GP::LeftJoin { left, right, .. }
        | GP::Union { left, right }
        | GP::Minus { left, right }
        | GP::SemiJoin { left, right }
        | GP::AntiJoin { left, right }
        | GP::Lateral { left, right } => vec![left, right],
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
        | GP::Service { inner, .. } => vec![inner],
    }
}

/// Calls `f` on `gp` and the patterns under it, outside expressions, until it returns
/// true; returns whether it did.
fn walk(gp: &GraphPattern, f: &mut dyn FnMut(&GraphPattern) -> bool) -> bool {
    f(gp) || children(gp).into_iter().any(|c| walk(c, f))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn no_calls(_: &[TriplePattern]) -> bool {
        false
    }

    /// The named graphs of the test stores.
    fn two() -> usize {
        2
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
        let got = rewrite(&gp, &no_calls, &two).unwrap();
        assert_eq!(got.to_string(), want.to_string());
    }

    #[test]
    fn values_stay_outside_what_reads_them() {
        // the BIND reads ?l, which the OPTIONAL may leave unbound, and the FILTER holds
        // EXISTS: the table joins outside them
        let gp = body(
            "SELECT * WHERE { VALUES ?l { 1 } { { ?s ?p ?o OPTIONAL { ?s ?q ?l } } BIND(?l + 1 AS ?m) } UNION { ?s ?p ?l FILTER EXISTS { ?s ?q ?l } } }",
        );
        let got = rewrite(&gp, &no_calls, &two).unwrap().to_string();
        let want = body(
            "SELECT * WHERE { { VALUES ?l { 1 } { { ?s ?p ?o OPTIONAL { ?s ?q ?l } } BIND(?l + 1 AS ?m) } } UNION { VALUES ?l { 1 } { ?s ?p ?l FILTER EXISTS { ?s ?q ?l } } } }",
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
            assert!(rewrite(&body(q), &no_calls, &two).is_none(), "{q}");
        }
    }

    #[test]
    fn other_operands_keep_their_place() {
        let gp = body(
            "SELECT * WHERE { ?a ?b ?l VALUES ?l { 1 2 } { ?s ?p ?l } UNION { ?l ?p ?s } ?l ?c ?d }",
        );
        let got = rewrite(&gp, &no_calls, &two).unwrap().to_string();
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
        assert!(rewrite(&body(queries[0]), &no_calls, &two).is_some());
        assert!(rewrite(&body(queries[9]), &no_calls, &two).is_some());
    }

    #[test]
    fn a_non_deterministic_bind_keeps_the_table_outside() {
        for f in ["UUID()", "STRUUID()", "RAND()", "BNODE()"] {
            let q = format!(
                "SELECT * WHERE {{ VALUES ?l {{ 1 1 }} {{ ?s ?p ?l BIND({f} AS ?u) }} UNION {{ ?s ?q ?l FILTER(RAND() < 2) }} }}"
            );
            let got = rewrite(&body(&q), &no_calls, &two).unwrap().to_string();
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
            rewrite(&body(q), &no_calls, &two).unwrap().to_string(),
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
        assert!(rewrite(&body(&q), &no_calls, &two).is_some());
        // five tables that each fit, but not together, stay outside
        let q = format!(
            "SELECT * WHERE {{ {} {} {} {} {} {{ ?a ?b ?c . ?c ?d ?e }} UNION {{ ?e ?d ?c }} }}",
            table("a", MAX_ROWS - 1),
            table("b", MAX_ROWS - 1),
            table("c", MAX_ROWS - 1),
            table("d", MAX_ROWS - 1),
            table("e", MAX_ROWS - 1),
        );
        assert!(rewrite(&body(&q), &no_calls, &two).is_none());
        // a small table into many branches
        let branches: Vec<String> = (0..64).map(|i| format!("{{ ?s <urn:p{i}> ?o }}")).collect();
        let q = format!(
            "SELECT * WHERE {{ {} {} }}",
            table("o", 1_000),
            branches.join(" UNION ")
        );
        assert!(rewrite(&body(&q), &no_calls, &two).is_none());
        let q = format!(
            "SELECT * WHERE {{ {} {} }}",
            table("o", 10),
            branches.join(" UNION ")
        );
        assert!(rewrite(&body(&q), &no_calls, &two).is_some());
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
            assert!(rewrite(&body(q), &calls, &two).is_none(), "{q}");
        }
        let q = "SELECT * WHERE { VALUES ?l { 1 } { ?s <urn:other> ?l } UNION { ?s ?p ?l } }";
        assert!(rewrite(&body(q), &calls, &two).is_some());
    }

    /// Asserts that the rewrite of `q` is the pattern of `want`.
    fn moves(q: &str, want: &str) {
        let got = rewrite(&body(q), &no_calls, &two).unwrap_or_else(|| panic!("{q}"));
        assert_eq!(got.to_string(), body(want).to_string(), "{q}");
    }

    /// Whether a FILTER in `gp` has a VALUES table under it.
    fn table_under_filter(gp: &GraphPattern) -> bool {
        walk(gp, &mut |p| match p {
            GraphPattern::Filter { inner, .. } => {
                walk(inner, &mut |q| matches!(q, GraphPattern::Values { .. }))
            }
            _ => false,
        })
    }

    #[test]
    fn a_filter_or_bind_on_a_variable_the_branch_binds_takes_the_table() {
        moves(
            "SELECT * WHERE { VALUES ?o { 1 2 } { ?s <urn:p> ?o FILTER(?o > 1) } UNION { ?s <urn:q> ?o BIND(STR(?o) AS ?n) } }",
            "SELECT * WHERE { { { VALUES ?o { 1 2 } ?s <urn:p> ?o } FILTER(?o > 1) } UNION { { VALUES ?o { 1 2 } ?s <urn:q> ?o } BIND(STR(?o) AS ?n) } }",
        );
        // the reader's union of the default graph and the named graphs with the graphs in
        // a table: the FILTER on ?g lets both tables in, the BIND of ?g only the other one
        moves(
            "SELECT * WHERE { VALUES ?g { <urn:g1> <urn:g2> } VALUES ?o { 1 2 } { { ?s <urn:p> ?o } BIND(<urn:d> AS ?g) } UNION { GRAPH ?g { ?s <urn:p> ?o } FILTER(?g != <urn:i>) } }",
            "SELECT * WHERE { { VALUES ?g { <urn:g1> <urn:g2> } { { VALUES ?o { 1 2 } ?s <urn:p> ?o } BIND(<urn:d> AS ?g) } } UNION { { VALUES ?g { <urn:g1> <urn:g2> } { VALUES ?o { 1 2 } GRAPH ?g { ?s <urn:p> ?o } } } FILTER(?g != <urn:i>) } }",
        );
        // an UNDEF cell in a variable the FILTER does not read
        moves(
            "SELECT * WHERE { VALUES (?o ?l) { (1 UNDEF) (2 \"v1\") } { ?s <urn:p> ?o FILTER(?o > 1) } UNION { ?s <urn:q> ?l } }",
            "SELECT * WHERE { { { VALUES (?o ?l) { (1 UNDEF) (2 \"v1\") } ?s <urn:p> ?o } FILTER(?o > 1) } UNION { VALUES (?o ?l) { (1 UNDEF) (2 \"v1\") } ?s <urn:q> ?l } }",
        );
        // a FILTER over the whole union, on a variable both branches bind
        moves(
            "SELECT * WHERE { VALUES ?o { 1 2 } { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?o } FILTER(?o != 2) } }",
            "SELECT * WHERE { { VALUES ?o { 1 2 } ?s <urn:p> ?o } UNION { VALUES ?o { 1 2 } ?s <urn:q> ?o } FILTER(?o != 2) }",
        );
    }

    #[test]
    fn a_variable_the_branch_may_leave_unbound_keeps_the_table_outside_the_filter() {
        for q in [
            // OPTIONAL
            "SELECT * WHERE { VALUES ?x { <urn:s1> <urn:s4> } { ?s <urn:p> ?o OPTIONAL { ?s <urn:r> ?x } FILTER(!BOUND(?x) || ?x != <urn:s2>) } UNION { ?s <urn:r> ?x } }",
            // MINUS, whose right side binds nothing in the result
            "SELECT * WHERE { VALUES ?o { 1 2 } { ?s <urn:q> ?l MINUS { ?s <urn:p> ?o } FILTER(COALESCE(?o, 1) = 1) } UNION { ?s <urn:p> ?o } }",
            // a BIND, whose error leaves the variable unbound
            "SELECT * WHERE { VALUES ?o { 1 2 } { ?s <urn:q> ?y BIND(<http://www.w3.org/2001/XMLSchema#integer>(?y) AS ?o) FILTER(COALESCE(?o, 1) = 1) } UNION { ?s <urn:p> ?o } }",
            // a union whose other branch does not bind it
            "SELECT * WHERE { VALUES ?o { 1 2 } { { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } } FILTER(!BOUND(?o)) } UNION { ?s <urn:p> ?o } }",
            // a union filtered as a whole
            "SELECT * WHERE { VALUES ?o { 1 2 } { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } FILTER(!BOUND(?o)) } }",
            // an UNDEF cell in the variable the FILTER reads
            "SELECT * WHERE { VALUES ?o { 1 2 UNDEF } { ?s <urn:p> ?o FILTER(?o > 1) } UNION { ?s <urn:q> ?l } }",
        ] {
            if let Some(got) = rewrite(&body(q), &no_calls, &two) {
                assert!(!table_under_filter(&got), "{q}\n{got}");
            }
        }
        // moved inside, the table would bind ?x where the OPTIONAL found nothing, and
        // the FILTER would see a value where it saw an unbound variable
        let s = differential_store();
        let outside = "SELECT * WHERE { VALUES ?x { <urn:s1> <urn:s4> } { ?s <urn:p> ?o OPTIONAL { ?s <urn:r> ?x } FILTER(!BOUND(?x)) } }";
        let inside = "SELECT * WHERE { { VALUES ?x { <urn:s1> <urn:s4> } ?s <urn:p> ?o OPTIONAL { ?s <urn:r> ?x } } FILTER(!BOUND(?x)) }";
        let (a, b) = (
            rows(&s, outside, None).unwrap(),
            rows(&s, inside, None).unwrap(),
        );
        assert!(!a.is_empty() && b.is_empty(), "{a:?} {b:?}");
        // an error in the FILTER removes the solution when the variable is unbound
        let outside = "SELECT * WHERE { VALUES ?o { 1 } { ?s <urn:q> ?l MINUS { ?s <urn:p> ?o } FILTER(?o = 1) } }";
        let inside = "SELECT * WHERE { { VALUES ?o { 1 } ?s <urn:q> ?l MINUS { ?s <urn:p> ?o } } FILTER(?o = 1) }";
        let (a, b) = (
            rows(&s, outside, None).unwrap(),
            rows(&s, inside, None).unwrap(),
        );
        assert!(a.is_empty() && !b.is_empty(), "{a:?} {b:?}");
    }

    #[test]
    fn values_move_into_a_graph_that_holds_a_union() {
        // the table outside a GRAPH, named or a variable
        moves(
            "SELECT * WHERE { VALUES ?o { 0 2 } GRAPH <urn:g1> { { ?s <urn:p> ?o } UNION { ?t <urn:p> ?o } } }",
            "SELECT * WHERE { GRAPH <urn:g1> { { VALUES ?o { 0 2 } ?s <urn:p> ?o } UNION { VALUES ?o { 0 2 } ?t <urn:p> ?o } } }",
        );
        moves(
            "SELECT * WHERE { ?a <urn:q> ?l VALUES ?o { 0 2 } GRAPH ?g { { ?s <urn:p> ?o } UNION { ?t <urn:p> ?o } } }",
            "SELECT * WHERE { ?a <urn:q> ?l GRAPH ?g { { VALUES ?o { 0 2 } ?s <urn:p> ?o } UNION { VALUES ?o { 0 2 } ?t <urn:p> ?o } } }",
        );
        // a join under the GRAPH, which the planner flattens into the outer group
        moves(
            "SELECT * WHERE { VALUES ?o { 0 2 } GRAPH <urn:g1> { ?s <urn:q> ?l { ?s <urn:p> ?o } UNION { ?t <urn:p> ?o } } }",
            "SELECT * WHERE { GRAPH <urn:g1> { ?s <urn:q> ?l { VALUES ?o { 0 2 } ?s <urn:p> ?o } UNION { VALUES ?o { 0 2 } ?t <urn:p> ?o } } }",
        );
        // the table and the union under a GRAPH that is an operand of a join
        moves(
            "SELECT * WHERE { ?a <urn:q> ?l GRAPH <urn:g1> { VALUES ?o { 0 2 } { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } } }",
            "SELECT * WHERE { ?a <urn:q> ?l GRAPH <urn:g1> { { VALUES ?o { 0 2 } ?s <urn:p> ?o } UNION { VALUES ?o { 0 2 } ?s <urn:q> ?l } } }",
        );
        moves(
            "SELECT * WHERE { GRAPH <urn:g1> { VALUES ?o { 0 2 } { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } } }",
            "SELECT * WHERE { GRAPH <urn:g1> { { VALUES ?o { 0 2 } ?s <urn:p> ?o } UNION { VALUES ?o { 0 2 } ?s <urn:q> ?l } } }",
        );
    }

    #[test]
    fn a_table_on_the_graph_variable_stays_outside_the_graph() {
        for q in [
            // the GRAPH binds ?g, not the pattern inside it
            "SELECT * WHERE { VALUES ?g { <urn:g1> <urn:g2> } GRAPH ?g { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } } }",
            "SELECT * WHERE { VALUES (?g ?o) { (<urn:g1> 0) (<urn:g2> UNDEF) } GRAPH ?g { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } } }",
            // plain patterns under the GRAPH: the planner joins the table with them
            "SELECT * WHERE { VALUES ?o { 0 2 } GRAPH ?g { ?s <urn:p> ?o OPTIONAL { ?s <urn:q> ?l } } }",
        ] {
            assert!(rewrite(&body(q), &no_calls, &two).is_none(), "{q}");
        }
        // a GRAPH ?g copies the table into the plan of each named graph
        let q = "SELECT * WHERE { VALUES ?o { 0 2 } GRAPH ?g { { ?s <urn:p> ?o } UNION { ?t <urn:p> ?o } } }";
        assert!(rewrite(&body(q), &no_calls, &|| MAX_COPIED_ROWS / 4 + 1).is_none());
        assert!(rewrite(&body(q), &no_calls, &|| MAX_COPIED_ROWS / 4).is_some());
    }

    /// The largest estimate of an index scan in the plan of `q`: a pattern looked up with
    /// the table's values has none, or a small one.
    fn widest_scan(s: &crate::store::Store, q: &str) -> f64 {
        use crate::sparql::PlanInfo;
        fn widest(p: &PlanInfo) -> f64 {
            let own = if p.operator == "IndexScan" {
                p.estimated_rows
            } else {
                0.0
            };
            p.children.iter().map(widest).fold(own, f64::max)
        }
        let (_, plan) = crate::sparql::explain(s.snapshot(), q, &Default::default()).unwrap();
        widest(&plan)
    }

    /// The queries of the shapes this rewrite added, over [`large_store`], with their
    /// answer's size.
    const NEW_SHAPES: [(&str, usize); 5] = [
        // the reader's union with its graphs in a table, under its FILTER on ?g
        (
            "SELECT ?s ?g WHERE { VALUES ?g { <urn:g1> <urn:x> } VALUES ?l { \"v7\" \"v70000\" } { { ?s <urn:p> ?l } BIND(<urn:x-arq:DefaultGraph> AS ?g) } UNION { GRAPH ?g { ?s <urn:p> ?l } FILTER(?g != <urn:i>) } }",
            1,
        ),
        // a FILTER on the table's variable over a join in a branch
        (
            "SELECT ?s WHERE { VALUES ?l { \"v7\" \"v70000\" } { ?s <urn:p> ?l FILTER(STRLEN(?l) > 1) } UNION { GRAPH ?g { ?s <urn:p> ?l . ?s <urn:q> ?n } FILTER(STRLEN(?l) > 1) } }",
            4,
        ),
        // the table and the union under a named GRAPH in a group
        (
            "SELECT ?s WHERE { <urn:s7> <urn:p> ?n GRAPH <urn:g1> { VALUES ?l { \"v7\" \"v8\" } { ?s <urn:p> ?l } UNION { ?l <urn:p> ?s } } }",
            2,
        ),
        // the table outside a named GRAPH that holds a union
        (
            "SELECT ?s WHERE { VALUES ?l { \"v7\" \"v8\" } GRAPH <urn:g2> { { ?s <urn:p> ?l } UNION { ?l <urn:p> ?s } } }",
            2,
        ),
        // the table outside a GRAPH ?g that holds a union
        (
            "SELECT ?s ?g WHERE { VALUES ?l { \"v7\" \"v8\" } GRAPH ?g { { ?s <urn:p> ?l } UNION { ?l <urn:p> ?s } } }",
            4,
        ),
    ];

    #[test]
    fn the_planner_looks_up_the_rows_of_the_tables_in_the_new_shapes() {
        let s = large_store();
        for (q, _) in NEW_SHAPES {
            let on = widest_scan(&s, q);
            DISABLED.set(true);
            let off = widest_scan(&s, q);
            DISABLED.set(false);
            assert!(on < 1_000.0 && off > 10_000.0, "{q}: {on} {off}");
        }
    }

    /// 100,000 quads in the default graph and in each of two named graphs.
    fn large_store() -> crate::store::Store {
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
        s
    }

    #[test]
    fn the_new_shapes_read_only_the_rows_of_their_tables() {
        let s = large_store();
        for (q, n) in NEW_SHAPES {
            let on = rows(&s, q, Some(1 << 20)).unwrap_or_else(|e| panic!("{q}: {e}"));
            assert_eq!(on.len(), n, "{q}: {on:?}");
            DISABLED.set(true);
            let unbounded = rows(&s, q, None);
            let off = rows(&s, q, Some(1 << 20));
            DISABLED.set(false);
            assert_eq!(unbounded.unwrap(), on, "{q}");
            assert!(off.is_err(), "{q}: {off:?}");
        }
    }

    #[test]
    fn answers_of_the_new_shapes_are_the_same_with_and_without_the_rewrite() {
        let s = differential_store();
        let queries = [
            // FILTER and BIND on a variable the branch binds, with UNDEF, duplicates and
            // values on which the expression fails
            "SELECT * WHERE { VALUES ?o { 1 2 UNDEF 2 \"x\" } { ?s <urn:p> ?o FILTER(?o > 1) } UNION { ?s <urn:q> ?o BIND(STR(?o) AS ?n) } }",
            "SELECT * WHERE { VALUES (?o ?l) { (1 \"v1\") (UNDEF \"v2\") (3 UNDEF) (\"a\" \"v1\") } { ?s <urn:p> ?o FILTER(?o + 1 > 2) } UNION { ?s <urn:q> ?l BIND(?l AS ?m) FILTER(STRSTARTS(?l, \"v\")) } }",
            "SELECT * WHERE { VALUES ?o { 1 2 } { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?o } FILTER(?o != 2) } }",
            // the graphs in a table, the default graph through a BIND
            "SELECT * WHERE { VALUES ?g { <urn:g1> <urn:x-arq:DefaultGraph> UNDEF } VALUES ?o { 1 2 } { { ?s <urn:p> ?o } BIND(<urn:x-arq:DefaultGraph> AS ?g) } UNION { GRAPH ?g { ?s <urn:p> ?o } FILTER(?g != <urn:g2>) } }",
            // the negative shapes
            "SELECT * WHERE { VALUES ?x { <urn:s1> <urn:s4> UNDEF } { ?s <urn:p> ?o OPTIONAL { ?s <urn:r> ?x } FILTER(!BOUND(?x) || ?x != <urn:s2>) } UNION { ?s <urn:r> ?x } }",
            "SELECT * WHERE { VALUES ?o { 1 2 } { ?s <urn:q> ?l MINUS { ?s <urn:p> ?o } FILTER(COALESCE(?o, 1) = 1) } UNION { ?s <urn:p> ?o } }",
            "SELECT * WHERE { VALUES ?o { 1 2 } { ?s <urn:q> ?y BIND(<http://www.w3.org/2001/XMLSchema#integer>(?y) AS ?o) FILTER(COALESCE(?o, 1) = 1) } UNION { ?s <urn:p> ?o } }",
            "SELECT * WHERE { VALUES ?o { 1 2 } { { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } } FILTER(!BOUND(?o)) } UNION { ?s <urn:p> ?o } }",
            "SELECT * WHERE { VALUES ?o { 1 2 } { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } FILTER(!BOUND(?o)) } }",
            // GRAPH shapes
            "SELECT * WHERE { VALUES ?o { 0 2 UNDEF } GRAPH <urn:g1> { { ?s <urn:p> ?o } UNION { ?t <urn:q> ?l } } }",
            "SELECT * WHERE { ?a <urn:r> ?b VALUES ?o { 0 2 } GRAPH ?g { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } } }",
            "SELECT * WHERE { VALUES ?o { 0 2 } GRAPH <urn:g1> { ?s <urn:q> ?l { ?s <urn:p> ?o } UNION { ?t <urn:p> ?o } } }",
            "SELECT * WHERE { ?a <urn:q> ?l GRAPH <urn:g1> { VALUES ?o { 0 2 } { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } } }",
            "SELECT * WHERE { GRAPH ?g { VALUES ?o { 0 2 } { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } } }",
            "SELECT * WHERE { VALUES ?g { <urn:g1> <urn:g9> } GRAPH ?g { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } } }",
            "SELECT * WHERE { VALUES (?g ?o) { (<urn:g1> 0) (<urn:g2> UNDEF) } GRAPH ?g { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } } }",
            "SELECT * WHERE { VALUES ?o { 0 2 } GRAPH ?g { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?l } FILTER(?o != 2) } }",
            "SELECT ?g (COUNT(*) AS ?n) WHERE { VALUES ?o { 0 0 2 } GRAPH ?g { { ?s <urn:p> ?o } UNION { ?s <urn:p> ?o } } } GROUP BY ?g",
        ];
        for q in queries {
            let (on, off) = both(&s, q);
            assert_eq!(on, off, "{q}");
            assert!(!on.is_empty(), "{q}");
        }
    }
}
