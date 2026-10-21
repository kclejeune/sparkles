//! Give pure UNION branches the selective graph pattern joined outside them.
//!
//! The usual join-order search cannot reorder an opaque UNION's interior. Distribute
//! a small BGP into its branches so that search can constrain their joins. Filters
//! cross the join only when the new bindings cannot change their original scope.
use super::plan::{Kind, Node, ScanSpec};
use spargebra::algebra::{Expression, Function, GraphPattern};
use spargebra::term::{TermPattern, TriplePattern, Variable};

const MAX_TRIPLES: usize = 8;
const MAX_BRANCHES: usize = 8;
const MAX_SCREEN_NODES: usize = MAX_TRIPLES * MAX_BRANCHES * 4;
/// Do not duplicate broad anchor reads merely because they have a constant object.
pub(super) const MAX_ANCHOR_ROWS: f64 = 32.0;

#[cfg(test)]
thread_local! {
    static ALTERNATIVES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ELIGIBILITY: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn alternate_planned() {
    ALTERNATIVES.with(|n| n.set(n.get() + 1));
}

pub(super) struct Candidate<'a> {
    pub anchor: &'a GraphPattern,
    union: &'a GraphPattern,
    patterns: &'a [TriplePattern],
    branches: usize,
}

impl Candidate<'_> {
    pub fn expand(&self) -> GraphPattern {
        expand(self.union, self.patterns)
    }

    /// Invest in another join-order search only for repeated unconstrained reads.
    /// The work floor uses the existing anchor row limit per branch, in planner
    /// cost units. It is independent of host timings and correlated fanout estimates.
    pub fn worth_planning(&self, original: &Node) -> bool {
        if !original.cost.is_finite() || original.cost <= self.branches as f64 * MAX_ANCHOR_ROWS {
            return false;
        }
        let mut todo = vec![(original, false)];
        let mut visited = 0;
        while let Some((node, in_union)) = todo.pop() {
            visited += 1;
            if visited > MAX_SCREEN_NODES {
                return false;
            }
            if in_union && matches!(node.kind, Kind::Join { .. }) && node.children.len() == 2 {
                let left = &node.children[0];
                let right = &node.children[1];
                if let (Some(a), Some(b)) = (unrestricted_scan(left), unrestricted_scan(right))
                    && same_relation(a, b)
                    && (left.vars.iter().any(|v| !right.vars.contains(v))
                        || right.vars.iter().any(|v| !left.vars.contains(v)))
                {
                    return true;
                }
            }
            todo.extend(
                node.children
                    .iter()
                    .map(|c| (c, in_union || matches!(node.kind, Kind::Union))),
            );
        }
        false
    }
}

fn unrestricted_scan(mut node: &Node) -> Option<&ScanSpec> {
    for _ in 0..MAX_SCREEN_NODES {
        match &node.kind {
            Kind::Scan(scan) if scan.eqs.is_empty() => return Some(scan),
            Kind::Sort(_) if node.children.len() == 1 => node = &node.children[0],
            _ => return None,
        }
    }
    None
}

fn same_relation(a: &ScanSpec, b: &ScanSpec) -> bool {
    let bindings = |scan: &ScanSpec| {
        let mut values = [None; 4];
        for (&position, &value) in scan.perm.order().iter().zip(&scan.prefix) {
            values[position] = Some(value);
        }
        values
    };
    a.graph == b.graph && a.dedup == b.dedup && bindings(a) == bindings(b)
}

pub(super) fn candidate<'a>(
    gp: &'a GraphPattern,
    calls: &dyn Fn(&[TriplePattern]) -> bool,
) -> Option<Candidate<'a>> {
    #[cfg(test)]
    ELIGIBILITY.with(|n| n.set(n.get() + 1));
    let mut candidate = shape(gp)?;
    if calls(candidate.patterns) {
        return None;
    }
    let moved = variables(candidate.anchor);
    let mut todo = vec![candidate.union];
    let mut branches = 0;
    let mut visited = 0;
    while let Some(node) = todo.pop() {
        visited += 1;
        if visited > MAX_SCREEN_NODES {
            return None;
        }
        match node {
            GraphPattern::Union { left, right } => {
                todo.push(right);
                todo.push(left);
            }
            GraphPattern::Filter { expr, inner } => {
                if !safe_expression(expr, &moved, &bound(inner)?) {
                    return None;
                }
                todo.push(inner);
            }
            GraphPattern::Bgp { patterns } => {
                branches += 1;
                let vars = variables(node);
                if branches > MAX_BRANCHES
                    || calls(patterns)
                    || moved.is_empty()
                    // Keep anchor-only multiplicity columns on the ordinary path.
                    || !moved.iter().all(|v| vars.contains(v))
                {
                    return None;
                }
            }
            _ => return None,
        }
    }
    candidate.branches = branches;
    Some(candidate)
}

/// Screen ordinary work before property catalogues and variable sets are allocated.
/// Every direct UNION has at least two branches. The complete branch-count floor
/// and all semantic guards still apply after this conservative lower-bound check.
pub(super) fn screen(gp: &GraphPattern, original: &Node) -> bool {
    shape(gp).is_some_and(|candidate| candidate.worth_planning(original))
}

fn shape(gp: &GraphPattern) -> Option<Candidate<'_>> {
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
        // A constant predicate alone need not constrain the shared variables.
        || !patterns.iter().any(|t| {
            matches!(t.object, TermPattern::NamedNode(_) | TermPattern::Literal(_))
        })
    {
        return None;
    }
    Some(Candidate {
        anchor,
        union,
        patterns,
        branches: 2,
    })
}

#[cfg(test)]
fn rewrite<'a>(
    gp: &'a GraphPattern,
    calls: &dyn Fn(&[TriplePattern]) -> bool,
) -> Option<(&'a GraphPattern, GraphPattern)> {
    candidate(gp, calls).map(|c| (c.anchor, c.expand()))
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
fn bound(mut gp: &GraphPattern) -> Option<Vec<Variable>> {
    for _ in 0..MAX_SCREEN_NODES {
        match gp {
            GraphPattern::Bgp { .. } => return Some(variables(gp)),
            GraphPattern::Filter { inner, .. } => gp = inner,
            _ => return None,
        }
    }
    None
}

fn expand(gp: &GraphPattern, anchor: &[TriplePattern]) -> GraphPattern {
    match gp {
        GraphPattern::Union { left, right } => GraphPattern::Union {
            left: Box::new(expand(left, anchor)),
            right: Box::new(expand(right, anchor)),
        },
        GraphPattern::Filter { expr, inner } => GraphPattern::Filter {
            expr: expr.clone(),
            inner: Box::new(expand(inner, anchor)),
        },
        GraphPattern::Bgp { patterns } => {
            // Conjunction is a bag join. Keep every pattern and every matching
            // assignment, including anchor variables hidden by an outer projection.
            let mut combined = Vec::with_capacity(anchor.len() + patterns.len());
            combined.extend_from_slice(anchor);
            combined.extend_from_slice(patterns);
            GraphPattern::Bgp { patterns: combined }
        }
        // No scope or effect assumptions about OPTIONAL, subqueries, BIND, SERVICE,
        // LATERAL, paths or registered property functions are needed for this rule.
        _ => unreachable!("candidate contains only validated pure branches"),
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

    #[test]
    fn small_or_unshared_anchors_do_not_plan_an_alternative() {
        use crate::io::Source;
        use crate::sparql::{QueryOptions, query};
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        store.load(&[Source::from_bytes(
            b"<urn:s> <urn:name> \"Anchor\"; <urn:tag> \"one\", \"two\"; <urn:p> <urn:a>, <urn:b> .".to_vec(),
            oxrdfio::RdfFormat::Turtle, None,
        )]).unwrap();
        for (anchor, branch, rows) in [
            (
                "?s <urn:name> \"Anchor\"; <urn:tag> ?hidden",
                "?s <urn:p> ?v",
                8,
            ),
            (
                "?s <urn:name> \"Anchor\"",
                "?s <urn:p> ?v; <urn:name> ?name",
                4,
            ),
            ("?s <urn:name> \"Anchor\"", "?s <urn:p> ?x; <urn:p> ?y", 8),
        ] {
            for body in [
                format!("{anchor} {{ {{ {branch} }} UNION {{ {branch} }} }}"),
                format!("{{ {{ {branch} }} UNION {{ {branch} }} }} {anchor}"),
            ] {
                ALTERNATIVES.with(|n| n.set(0));
                ELIGIBILITY.with(|n| n.set(0));
                let result = query(
                    store.snapshot(),
                    &format!("SELECT * WHERE {{ {body} }}"),
                    &QueryOptions::default(),
                )
                .unwrap();
                assert_eq!(result.len(), rows, "{body}");
                assert_eq!(ALTERNATIVES.with(|n| n.get()), 0, "{body}");
                assert_eq!(ELIGIBILITY.with(|n| n.get()), 0, "{body}");
            }
        }
    }

    #[test]
    fn a_rejected_high_read_anchor_does_not_plan_an_alternative() {
        use crate::io::Source;
        use crate::sparql::{QueryOptions, query};
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::from("<urn:shared> a <urn:C>; <urn:q> <urn:x> .\n");
        for branch in 0..8 {
            data.push_str(&format!("<urn:shared> <urn:r{branch}> \"v\" .\n"));
        }
        for i in 0..64 {
            data.push_str(&format!(
                "<urn:s:{}> a <urn:C> .\n<urn:s:{}> <urn:q> <urn:x> .\n",
                2 * i,
                2 * i + 1
            ));
            for branch in 0..8 {
                data.push_str(&format!("<urn:s:{}> <urn:r{branch}> \"v\" .\n", 2 * i + 1));
            }
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let branches = (0..8)
            .map(|i| format!("{{ ?s <urn:r{i}> ?v }}"))
            .collect::<Vec<_>>()
            .join(" UNION ");
        for object in ["?hidden", "<urn:x>"] {
            ALTERNATIVES.with(|n| n.set(0));
            ELIGIBILITY.with(|n| n.set(0));
            let text = format!(
                "SELECT ?s ?v WHERE {{ ?s a <urn:C>; <urn:q> {object} . {{ {branches} }} }}"
            );
            let result = query(store.snapshot(), &text, &QueryOptions::default()).unwrap();
            assert_eq!(result.len(), 8);
            assert_eq!(ALTERNATIVES.with(|n| n.get()), 0, "{text}");
            assert_eq!(ELIGIBILITY.with(|n| n.get()), 0, "{text}");
        }
    }

    #[test]
    fn relation_screen_checks_bindings_work_and_bounded_traversal() {
        use super::super::plan::{GraphFilter, JoinAlgo};
        use crate::index::Perm;
        fn scan(perm: Perm, predicate: u64, vars: Vec<u32>) -> Node {
            let spec = ScanSpec {
                perm,
                prefix: vec![predicate],
                cols: vec![],
                eqs: vec![],
                graph: GraphFilter::Default,
                graph_col: perm.col_of(crate::index::G),
                dedup: false,
            };
            Node::leaf(Kind::Scan(spec), vars, 100.0, String::new())
        }
        fn unary(kind: Kind, child: Node) -> Node {
            let mut node = Node::leaf(kind, child.vars.clone(), child.est, String::new());
            node.cost = child.cost;
            node.children.push(child);
            node
        }
        let gp = body(
            "SELECT * WHERE { ?root <urn:anchor> <urn:target> { ?root <urn:p> ?v } UNION { ?root <urn:q> ?v } }",
        );
        let candidate = candidate(&gp, &|_| false).unwrap();
        let left = scan(Perm::Pso, 42, vec![0, 1]);
        let right = scan(Perm::Pos, 42, vec![0, 2]);
        let mut joined = Node::leaf(
            Kind::Join {
                algo: JoinAlgo::Merge,
                keys: vec![0],
            },
            vec![0, 1, 2],
            50.0,
            String::new(),
        );
        joined.children = vec![left, right];
        let mut original = unary(Kind::Union, joined);
        original.cost = 1000.0;
        // Uniform estimates can understate a correlated self-join's fanout.
        assert!(candidate.worth_planning(&original));
        original.cost = 2.0 * MAX_ANCHOR_ROWS;
        assert!(!candidate.worth_planning(&original));
        original.cost = 1000.0;
        let mut other = original.clone();
        let Kind::Scan(scan) = &mut other.children[0].children[1].kind else {
            unreachable!()
        };
        scan.prefix[0] = 43;
        assert!(!candidate.worth_planning(&other));
        let mut other = original.clone();
        let Kind::Scan(scan) = &mut other.children[0].children[1].kind else {
            unreachable!()
        };
        scan.graph = GraphFilter::One(9);
        assert!(!candidate.worth_planning(&other));
        let mut other = original.clone();
        other.children[0].children[1].vars = vec![0, 1];
        assert!(!candidate.worth_planning(&other));
        for _ in 0..MAX_SCREEN_NODES {
            original = unary(Kind::Sort(vec![]), original);
        }
        assert!(!candidate.worth_planning(&original));
    }

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
            let anchor = if branch.contains("?outer") {
                "?root <urn:anchor> <urn:target>; <urn:bind> ?outer"
            } else {
                "?root <urn:anchor> <urn:target>"
            };
            let query = format!(
                "SELECT * WHERE {{ {anchor} . {{ {branch} }} UNION {{ ?root <urn:q> ?v }} }}"
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
