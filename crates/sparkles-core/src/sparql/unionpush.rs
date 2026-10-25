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

/// A bounded UNION whose branch order and excess demand have no external effects.
fn pure_union(ctx: &super::ctx::Ctx, node: &Node) -> bool {
    use super::expr::Expr;

    fn expression(expr: &Expr, remaining: &mut usize) -> bool {
        if *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        match expr {
            Expr::Const(_) | Expr::Lit(..) | Expr::Var(_) | Expr::Bound(_) => true,
            Expr::Eq(a, b) | Expr::SameTerm(a, b) | Expr::And(a, b) | Expr::Or(a, b) => {
                expression(a, remaining) && expression(b, remaining)
            }
            Expr::Not(a) => expression(a, remaining),
            _ => false,
        }
    }

    fn plain(node: &Node, remaining: &mut usize, expressions: &mut usize) -> bool {
        if *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        let shape = match &node.kind {
            Kind::Scan(_) | Kind::Values(_) | Kind::Empty => node.children.is_empty(),
            Kind::Join { .. } => node.children.len() == 2,
            Kind::Union => (2..=MAX_BRANCHES).contains(&node.children.len()),
            Kind::Project(_) | Kind::Sort(_) => node.children.len() == 1,
            Kind::Extend(_, expr) => node.children.len() == 1 && expr.const_id().is_some(),
            Kind::Filter(exprs) => {
                node.children.len() == 1 && exprs.iter().all(|e| expression(e, expressions))
            }
            Kind::IndexJoin(spec) => {
                node.children.len() == 1
                    && spec
                        .probes
                        .iter()
                        .all(|p| p.filter.iter().all(|e| expression(e, expressions)))
            }
            _ => false,
        };
        shape
            && node
                .children
                .iter()
                .all(|c| plain(c, remaining, expressions))
    }

    !ctx.calls_extensions
        && matches!(node.kind, Kind::Union)
        && (2..=MAX_BRANCHES).contains(&node.children.len())
        && node
            .children
            .iter()
            .all(|c| c.cost.is_finite() && c.cost >= 0.0)
        && plain(node, &mut 128, &mut 256)
}

/// Only a wholly admitted pure ASK may reduce projection demand. Subtree purity
/// alone cannot rule out an effectful consumer outside that subtree.
pub(super) fn limited_projection(ctx: &super::ctx::Ctx, node: &Node) -> bool {
    !ctx.is_cursor()
        && matches!(node.kind, Kind::Project(_))
        && ctx
            .eager_union_demand
            .load(std::sync::atomic::Ordering::Relaxed)
}

/// A prefix's live rows cannot exceed its covering base blocks and inserted keys.
/// Unlike the planner's graph discount and midpoint estimate, this is an upper bound.
fn scan_upper(ctx: &super::ctx::Ctx, scan: &ScanSpec) -> u64 {
    if let Some(count) = ctx.snap.predicate_count(scan.perm, &scan.prefix) {
        return count;
    }
    let index = ctx.snap.perm(scan.perm);
    let (lo, hi) = index.block_range(&scan.prefix);
    let base = if lo < hi {
        index.blocks[hi - 1].row_start + u64::from(index.blocks[hi - 1].rows)
            - index.blocks[lo].row_start
    } else {
        0
    };
    base.saturating_add(crate::store::Delta::count_prefix(
        &ctx.snap.delta.ins[scan.perm.index()],
        &scan.prefix,
    ))
}

fn full_work(ctx: &super::ctx::Ctx, node: &Node, left: &mut usize) -> Option<f64> {
    if *left == 0 || !node.cost.is_finite() || node.cost < 0.0 {
        return None;
    }
    *left -= 1;
    let own = match &node.kind {
        Kind::Scan(scan) => node.cost.max(scan_upper(ctx, scan) as f64),
        Kind::Values(table) => node.cost.max(table.len() as f64),
        _ => node.cost,
    };
    let mut children = 0.0;
    for child in &node.children {
        children += full_work(ctx, child, left)?;
    }
    let work = own.max(children);
    work.is_finite().then_some(work)
}

/// Work before the first limited result. A filtered or blocking input may have
/// to run to completion, so keep its full cost rather than amortizing it over
/// output rows. Eager joins also build their estimated smaller input in full.
fn first_work(
    ctx: &super::ctx::Ctx,
    node: &Node,
    want: usize,
    remaining: &mut usize,
) -> Option<f64> {
    use super::plan::GraphFilter;
    if *remaining == 0
        || !node.cost.is_finite()
        || node.cost < 0.0
        || !node.est.is_finite()
        || node.est < 0.0
    {
        return None;
    }
    *remaining -= 1;
    let prefix = |n: f64| (want as f64 / n.max(1.0)).min(1.0);
    let work = match &node.kind {
        Kind::Scan(spec) => {
            let all = spec.eqs.is_empty()
                && !spec.dedup
                && (matches!(spec.graph, GraphFilter::All)
                    || spec
                        .prefix
                        .get(spec.graph_col)
                        .is_some_and(|g| spec.graph.accepts(*g))
                    || (ctx.snap.delta.is_empty()
                        && ctx.snap.generation.stats.graphs.len() == 1
                        && spec.graph.accepts(ctx.snap.generation.stats.graphs[0].0)));
            if all {
                node.cost * prefix(node.est)
            } else {
                node.cost.max(scan_upper(ctx, spec) as f64)
            }
        }
        Kind::Values(table) => node.cost.max(table.len() as f64),
        Kind::Empty => 0.0,
        Kind::Project(_) => first_work(ctx, &node.children[0], want, remaining)?,
        Kind::Extend(..) => {
            first_work(ctx, &node.children[0], want.max(64), remaining)?
                + (node.cost - node.children[0].cost).max(0.0)
                    * (want.max(64) as f64 / node.est.max(1.0)).min(1.0)
        }
        Kind::Join { .. } => {
            if node
                .children
                .iter()
                .any(|c| !c.est.is_finite() || c.est < 0.0)
            {
                return None;
            }
            let lim = usize::from(node.children[0].est < node.children[1].est);
            let part = &node.children[lim];
            let other = &node.children[1 - lim];
            if !other.cost.is_finite()
                || other.cost < 0.0
                || !part.est.is_finite()
                || part.est < 0.0
            {
                return None;
            }
            full_work(ctx, other, remaining)?
                + first_work(ctx, part, want.max(64), remaining)?
                + (node.cost - part.cost - other.cost).max(0.0)
                    * (want.max(64) as f64 / part.est.max(1.0)).min(1.0)
        }
        Kind::Union => {
            let mut cost = 0.0;
            let mut need = want as f64;
            for child in &node.children {
                cost += first_work(ctx, child, need.ceil() as usize, remaining)?;
                need = (need - child.est).max(0.0);
                if need == 0.0 {
                    break;
                }
            }
            cost
        }
        Kind::Filter(_) | Kind::IndexJoin(_) | Kind::Sort(_) => {
            // This node was counted above; the full walk counts it once itself.
            *remaining += 1;
            full_work(ctx, node, remaining)?
        }
        _ => return None,
    };
    (work.is_finite() && work >= 0.0).then_some(work)
}

/// A new order must fit conservative output bounds, including compatible-key
/// products. Cardinality estimates do not bound a join whose inputs contain UNDEF.
fn ordering_fits(ctx: &super::ctx::Ctx, root: &Node) -> bool {
    // Geometric prefix retries can produce more than one full scan. Keep the
    // original order under a finite total-work cap instead of certifying a sum.
    if ctx.max_rows_produced != u64::MAX || !ctx.snap.delta.is_empty() {
        return false;
    }
    fn visit(ctx: &super::ctx::Ctx, node: &Node, left: &mut usize, bytes: &mut u64) -> Option<u64> {
        if *left == 0 || node.children.len() > MAX_BRANCHES {
            return None;
        }
        *left -= 1;
        let mut children = [0u64; MAX_BRANCHES];
        for (i, child) in node.children.iter().enumerate() {
            children[i] = visit(ctx, child, left, bytes)?;
        }
        let count = match &node.kind {
            Kind::Values(table) => table.len() as u64,
            Kind::Scan(scan) => scan_upper(ctx, scan),
            Kind::Empty => 0,
            Kind::Join { .. } => children[0].checked_mul(children[1])?,
            Kind::Union => children[..node.children.len()]
                .iter()
                .try_fold(0u64, |sum, n| sum.checked_add(*n))?,
            Kind::Project(_) | Kind::Filter(_) | Kind::Extend(..) | Kind::Sort(_) => children[0],
            Kind::Slice { offset, limit } => children[0]
                .saturating_sub(*offset as u64)
                .min(limit.map_or(u64::MAX, |n| n as u64)),
            // Probe fanout and duplicate input keys have no cheap output upper
            // bound. Keep their original order rather than certify an estimate.
            Kind::IndexJoin(_) => return None,
            _ => return None,
        };
        if count > ctx.max_rows as u64 {
            return None;
        }
        // Include room for pair indexes and capacity growth as well as columns.
        let width = (node.vars.len() as u64).checked_mul(16)?.checked_add(64)?;
        *bytes = bytes.checked_add(count.checked_mul(width)?.checked_add(128)?)?;
        (*bytes <= ctx.memory_room()).then_some(count)
    }
    let mut bytes = 0;
    let mut left = MAX_SCREEN_NODES;
    visit(ctx, root, &mut left, &mut bytes).is_some()
}

/// An eager ASK needs one solution, so visit a cheaper pure UNION operand first.
/// Cursor execution and SELECT retain their existing order.
pub(super) fn order_ask(ctx: &super::ctx::Ctx, node: &mut Node) {
    if !matches!(
        node.kind,
        Kind::Slice {
            offset: 0,
            limit: Some(1)
        }
    ) || node.children.len() != 1
    {
        return;
    }
    let mut union = &mut node.children[0];
    for _ in 0..8 {
        if !matches!(union.kind, Kind::Project(_)) || union.children.len() != 1 {
            break;
        }
        union = &mut union.children[0];
    }
    if !pure_union(ctx, union) {
        return;
    }
    ctx.eager_union_demand
        .store(true, std::sync::atomic::Ordering::Relaxed);
    if !ordering_fits(ctx, node) {
        return;
    }
    let mut union = &mut node.children[0];
    for _ in 0..8 {
        if !matches!(union.kind, Kind::Project(_)) || union.children.len() != 1 {
            break;
        }
        union = &mut union.children[0];
    }
    let Some(mut work) = first_work(ctx, &union.children[0], 1, &mut 128) else {
        return;
    };
    let mut cheapest = 0;
    for i in 1..union.children.len() {
        let Some(candidate) = first_work(ctx, &union.children[i], 1, &mut 128) else {
            return;
        };
        if candidate < work {
            cheapest = i;
            work = candidate;
        }
    }
    union.children.swap(0, cheapest);
}
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
    fn compatible_undef_join_fanout_does_not_displace_a_bounded_answer() {
        use crate::io::Source;
        use crate::sparql::{QueryOptions, query};
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::new();
        for i in 0..1000 {
            data.push_str(&format!("<urn:s{i}> <urn:p> {i} .\n"));
        }
        data.push_str("<urn:g> {\n");
        for i in 0..99_000 {
            data.push_str(&format!("<urn:n{i}> <urn:q> {i} .\n"));
        }
        data.push_str("}\n");
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::TriG,
                None,
            )])
            .unwrap();
        let values = vec!["UNDEF"; 32].join(" ");
        let text = format!(
            "ASK {{ {{ ?s <urn:p> ?v FILTER(?v != ?s) }} UNION {{ {{ VALUES ?x {{ {values} }} }} {{ VALUES ?x {{ {values} }} }} }} }}"
        );
        let result = query(
            store.snapshot(),
            &text,
            &QueryOptions {
                no_cache: true,
                max_rows: Some(1000),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(result.boolean);
    }

    #[test]
    fn graph_discounted_scan_estimates_do_not_admit_an_oversized_prefix() {
        use crate::io::Source;
        use crate::sparql::{QueryOptions, query};
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::new();
        for i in 0..100 {
            data.push_str(&format!("<urn:s{i}> <urn:p> {i} .\n"));
        }
        data.push_str("<urn:g> {\n");
        for i in 0..900 {
            data.push_str(&format!("<urn:n{i}> <urn:q> {i} .\n"));
        }
        data.push_str("}\n");
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::TriG,
                None,
            )])
            .unwrap();
        let values = (0..24)
            .map(|i| format!("<urn:v{i}>"))
            .collect::<Vec<_>>()
            .join(" ");
        let text = format!(
            "ASK {{ {{ VALUES ?s {{ {values} }} }} UNION {{ ?s <urn:p> ?v FILTER(?v != ?s) }} }}"
        );
        let result = query(
            store.snapshot(),
            &text,
            &QueryOptions {
                no_cache: true,
                max_rows: Some(40),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(result.boolean);
    }

    #[test]
    fn finite_produced_row_caps_keep_prefix_retries_out_of_the_first_operand() {
        use crate::io::Source;
        use crate::sparql::{QueryOptions, query};
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        let data = (0..400)
            .map(|i| format!("<urn:s{i}> <urn:p> {i} .\n"))
            .collect::<String>();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let values = (0..1000)
            .map(|i| format!("<urn:v{i}>"))
            .collect::<Vec<_>>()
            .join(" ");
        let text = format!(
            "ASK {{ {{ VALUES ?s {{ {values} }} }} UNION {{ ?s <urn:p> ?v FILTER(?v = ?s) }} }}"
        );
        let result = query(
            store.snapshot(),
            &text,
            &QueryOptions {
                no_cache: true,
                max_rows_produced: Some(3420),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(result.boolean);
        assert_eq!(result.rows_produced, 3001);
    }

    #[test]
    fn an_eager_values_operand_is_not_priced_as_a_single_row() {
        use crate::io::Source;
        use crate::sparql::{QueryOptions, query};
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        let data = (0..64)
            .map(|i| format!("<urn:s{i}> <urn:first> {i} .\n"))
            .collect::<String>();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let mut failures = Vec::new();
        for (size, rows, produced) in [
            (65, Some(64), None),
            (100, Some(64), None),
            (10_000, Some(64), None),
            (100, None, Some(300)),
            (10_000, None, None),
        ] {
            let values = (0..size)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            let text = format!(
                "ASK {{ {{ ?s <urn:first> ?v FILTER(?v != ?s) }} UNION {{ VALUES ?x {{ {values} }} }} }}"
            );
            let result = query(
                store.snapshot(),
                &text,
                &QueryOptions {
                    no_cache: true,
                    max_rows: rows,
                    max_rows_produced: produced,
                    ..Default::default()
                },
            );
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    failures.push((size, rows, produced, error));
                    continue;
                }
            };
            assert!(result.boolean);
            let mut todo = vec![&result.plan];
            while let Some(p) = todo.pop() {
                if p.operator == "Values" {
                    assert_eq!(
                        p.actual_rows, -1,
                        "the eager VALUES that cannot fit the budget must stay skipped"
                    );
                }
                todo.extend(p.children.iter());
            }
        }
        assert!(
            failures.is_empty(),
            "previously answering budget cases failed: {failures:?}"
        );
    }

    #[test]
    fn a_nested_union_prices_the_remaining_constant_bind_demand() {
        use crate::io::Source;
        use crate::sparql::{QueryOptions, query};
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::new();
        for i in 0..64 {
            data.push_str(&format!("<urn:s{i}> <urn:first> {i} .\n"));
        }
        for i in 0..10_000 {
            data.push_str(&format!("<urn:f{i}> <urn:false> {i} .\n"));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let text = "ASK { { ?s <urn:first> ?v FILTER(?v != ?s) } UNION { { SELECT ?s WHERE { { VALUES ?s { <urn:one> } } UNION { ?s <urn:false> ?v FILTER(?v = ?s) } } } BIND(1 AS ?tag) } }";
        for rows in [None, Some(64)] {
            let result = query(
                store.snapshot(),
                text,
                &QueryOptions {
                    no_cache: true,
                    max_rows: rows,
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(result.boolean);
            let mut todo = vec![&result.plan];
            while let Some(p) = todo.pop() {
                if p.operator == "IndexScan" && p.description.contains("urn:false") {
                    assert_eq!(p.actual_rows, -1, "the nested false arm must stay skipped");
                }
                todo.extend(p.children.iter());
            }
        }
    }

    #[test]
    fn a_false_filtered_operand_does_not_precede_an_immediate_wide_answer() {
        use crate::io::Source;
        use crate::sparql::{QueryOptions, query};
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::new();
        for i in 0..100_000 {
            data.push_str(&format!("<urn:w{i}> <urn:wide> {i} .\n"));
        }
        for i in 0..10_000 {
            data.push_str(&format!("<urn:f{i}> <urn:small> {i} .\n"));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        for rows in [None, Some(64)] {
            let result = query(
                store.snapshot(),
                "ASK { { ?s <urn:wide> ?v } UNION { ?s <urn:small> ?v FILTER(?v = ?s) } }",
                &QueryOptions {
                    no_cache: true,
                    max_rows: rows,
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(result.boolean);
            let mut todo = vec![&result.plan];
            while let Some(p) = todo.pop() {
                if p.operator == "IndexScan" && p.description.contains("urn:small") {
                    assert_eq!(p.actual_rows, -1, "the false operand must remain skipped");
                }
                todo.extend(p.children.iter());
            }
        }
    }

    #[test]
    fn a_nested_pure_union_keeps_an_effectful_consumers_original_demand() {
        use crate::io::Source;
        use crate::sparql::{LocalEndpoints, LocalTarget, QueryOptions, query};
        use crate::store::{Snapshot, Store, StoreOptions};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Local {
            snap: Arc<Snapshot>,
            calls: Arc<AtomicUsize>,
        }
        impl std::fmt::Debug for Local {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("Local")
            }
        }
        impl LocalEndpoints for Local {
            fn resolve(&self, iri: &str) -> Option<crate::Result<LocalTarget>> {
                (iri == "http://local/sparql").then(|| {
                    self.calls.fetch_add(1, Ordering::Relaxed);
                    Ok(LocalTarget {
                        snap: self.snap.clone(),
                        options: QueryOptions {
                            no_cache: true,
                            ..Default::default()
                        },
                        hold: None,
                    })
                })
            }
        }
        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::from("<urn:other> <urn:other> 1 .\n");
        for i in 0..128 {
            data.push_str(&format!("<urn:w{i}> <urn:wide> {i} .\n"));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let remote = Store::in_memory(StoreOptions::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let values = (0..63)
            .map(|i| format!("<urn:v{i}>"))
            .collect::<Vec<_>>()
            .join(" ");
        let text = format!(
            "ASK {{ {{ VALUES ?s {{ {values} }} }} UNION {{ SELECT ?s WHERE {{ {{ ?s <urn:wide> ?v }} UNION {{ ?s <urn:other> ?v }} }} }} BIND(EXISTS {{ SERVICE <http://local/sparql> {{ ?s <urn:r> ?x }} }} AS ?e) }}"
        );
        let result = query(
            store.snapshot(),
            &text,
            &QueryOptions {
                no_cache: true,
                allow_service: true,
                local_endpoints: Some(Arc::new(Local {
                    snap: remote.snapshot(),
                    calls: calls.clone(),
                })),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(result.boolean);
        assert!(
            calls.load(Ordering::Relaxed) > 0,
            "the in-process SERVICE must actually execute"
        );
        let mut todo = vec![&result.plan];
        let mut rows = None;
        while let Some(p) = todo.pop() {
            if p.operator == "Bind" && p.columns.iter().any(|v| v == "e") {
                rows = Some(p.actual_rows);
            }
            todo.extend(p.children.iter());
        }
        assert_eq!(
            rows,
            Some(127),
            "the pure subtree must not reduce its effectful consumer's127correlated evaluations"
        );
    }

    #[test]
    fn ask_visits_a_cheap_pure_union_branch_before_reading_the_wide_one() {
        use crate::io::Source;
        use crate::sparql::{PlanInfo, QueryOptions, query};
        use crate::store::{Store, StoreOptions};

        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::from("<urn:yes> <urn:cheap> 1 .\n");
        for i in 0..128 {
            data.push_str(&format!(
                "<urn:s{i}> <urn:wide> {i}; <urn:extra> {} .\n",
                i + 1
            ));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let r = query(
            store.snapshot(),
            "ASK { { ?s <urn:wide> ?v; <urn:extra> ?n } UNION { <urn:yes> <urn:cheap> ?v } }",
            &QueryOptions::default(),
        )
        .unwrap();
        assert!(r.boolean);
        fn wide(p: &PlanInfo) -> Option<&PlanInfo> {
            if p.description.contains("urn:wide") {
                return Some(p);
            }
            p.children.iter().find_map(wide)
        }
        let wide = wide(&r.plan).expect("wide scan is retained in the physical plan");
        assert_eq!(
            wide.actual_rows, -1,
            "the cheaper branch must answer ASK before any wide scan"
        );
        assert!(wide.fidelity().skipped.is_some());
        let selected = query(
            store.snapshot(),
            "SELECT * WHERE { { ?s <urn:wide> ?v; <urn:extra> ?n } UNION { <urn:yes> <urn:cheap> ?v } } LIMIT 1",
            &QueryOptions::default(),
        )
        .unwrap();
        assert!(
            selected.rows()[0]
                .iter()
                .any(|v| v.as_ref().is_some_and(|v| v.to_string().contains("urn:s")))
        );
        let explicit = crate::sparql::ask_streaming(
            store.snapshot(),
            "ASK { { ?s <urn:wide> ?v; <urn:extra> ?n } UNION { <urn:yes> <urn:cheap> ?v } }",
            &QueryOptions::default(),
            &crate::sparql::CursorOptions {
                batch_rows: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let mut plans = vec![&explicit.plan];
        let mut read_wide = false;
        while let Some(p) = plans.pop() {
            if p.description.contains("urn:wide") && p.actual_rows > 0 {
                read_wide = true;
            }
            plans.extend(p.children.iter());
        }
        assert!(
            read_wide,
            "forced streaming retains its original operand order"
        );
    }

    #[test]
    fn ask_union_answers_and_explicit_streaming_keep_their_execution_contract() {
        use crate::io::Source;
        use crate::sparql::{
            CursorOptions, ExecutionMode, QueryExecution, QueryOptions, query_execution,
        };
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<urn:s> <urn:p> 1, 2 . <urn:t> <urn:q> 3 . <urn:g> { <urn:s> <urn:p> 4 }"
                    .to_vec(),
                oxrdfio::RdfFormat::TriG,
                None,
            )])
            .unwrap();
        let cases = [
            ("{ ?s <urn:p> ?v } UNION { <urn:t> <urn:q> ?v }", true),
            (
                "{ ?s <urn:p> ?v FILTER(?v = 99) } UNION { <urn:t> <urn:q> ?v }",
                true,
            ),
            (
                "{ ?s <urn:p> ?v FILTER(?v = 99) } UNION { <urn:t> <urn:q> ?v FILTER(?v = 99) }",
                false,
            ),
            (
                "{ VALUES ?v { UNDEF 1 1 } ?s <urn:p> ?v } UNION { <urn:t> <urn:q> ?v }",
                true,
            ),
            (
                "{ GRAPH ?g { ?s <urn:p> ?v } } UNION { GRAPH <urn:g> { ?s <urn:p> ?v } }",
                true,
            ),
            (
                "{ BIND(<urn:t> AS ?s) ?s <urn:q> ?v } UNION { ?s <urn:missing> ?v }",
                true,
            ),
            (
                "{ ?s <urn:missing> ?v } UNION { ?s <urn:absent> ?v }",
                false,
            ),
        ];
        for (body, expected) in cases {
            let text = format!("ASK {{ {body} }}");
            for mode in [
                ExecutionMode::Eager,
                ExecutionMode::Auto,
                ExecutionMode::Streaming,
            ] {
                let result = query_execution(
                    store.snapshot(),
                    &text,
                    &QueryOptions::default(),
                    &CursorOptions {
                        batch_rows: 1,
                        ..Default::default()
                    },
                    mode,
                )
                .unwrap();
                let result = match result {
                    QueryExecution::Eager(result) => {
                        assert_ne!(mode, ExecutionMode::Streaming);
                        result
                    }
                    QueryExecution::Ask(result) => {
                        assert_eq!(mode, ExecutionMode::Streaming);
                        result
                    }
                    _ => panic!("ASK must produce a completed boolean"),
                };
                assert_eq!(result.result().boolean, expected, "{text}, {mode:?}");
            }
        }
        let parsed = crate::sparql::parse_query(
            "ASK { { ?s <urn:p> ?v } UNION { <urn:t> <urn:q> ?v } }",
            None,
            &[],
        )
        .unwrap();
        let mut prepared =
            super::super::prepare_query(store.snapshot(), &parsed, &Default::default(), None, None)
                .unwrap();
        assert!(!limited_projection(
            &prepared.ctx,
            &prepared.node.children[0]
        ));
        order_ask(&prepared.ctx, &mut prepared.node);
        assert!(limited_projection(
            &prepared.ctx,
            &prepared.node.children[0]
        ));
        std::sync::Arc::get_mut(&mut prepared.ctx)
            .unwrap()
            .enable_cursor_accounting()
            .unwrap();
        assert!(
            !limited_projection(&prepared.ctx, &prepared.node.children[0]),
            "a cursor fallback must retain its original eager prefix policy too"
        );
    }

    #[test]
    fn effectful_ask_branches_still_run_in_the_original_order() {
        use crate::io::Source;
        use crate::sparql::extensions::{
            ExtensionRegistry, ScalarContext, ScalarDescriptor, ScalarResult, Volatility,
        };
        use crate::sparql::{QueryOptions, query};
        use crate::store::{Store, StoreOptions};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::from("<urn:t> <urn:cheap> 1 .\n");
        for i in 0..128 {
            data.push_str(&format!("<urn:s{i}> <urn:wide> {i} .\n"));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let mut descriptor = ScalarDescriptor::new(
            "urn:tick",
            0..=0,
            move |_: &ScalarContext<'_>, _: &[oxrdf::Term]| -> ScalarResult {
                seen.fetch_add(1, Ordering::Relaxed);
                Ok(oxrdf::Literal::from(true).into())
            },
        )
        .unwrap();
        descriptor.volatility = Volatility::Volatile;
        let mut registry = ExtensionRegistry::builder();
        registry.register_scalar(descriptor).unwrap();
        let opts = QueryOptions {
            extensions: Some(registry.build()),
            ..Default::default()
        };
        let result = query(
            store.snapshot(),
            "ASK { { ?s <urn:wide> ?v FILTER(<urn:tick>()) } UNION { <urn:t> <urn:cheap> ?v } }",
            &opts,
        )
        .unwrap();
        assert!(result.boolean);
        assert_eq!(
            count.load(Ordering::Relaxed),
            64,
            "the effectful path retains its original demand and callback count"
        );
    }

    #[test]
    fn reordered_and_retained_ask_work_is_still_bounded() {
        use crate::io::Source;
        use crate::sparql::{QueryOptions, query};
        use crate::store::{Store, StoreOptions};
        use std::sync::{Arc, atomic::AtomicBool};
        use std::time::{Duration, Instant};
        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::from("<urn:t> <urn:cheap> 1 .\n");
        for i in 0..128 {
            data.push_str(&format!("<urn:s{i}> <urn:wide> {i} .\n"));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let q = "ASK { { ?s <urn:wide> ?v } UNION { <urn:t> <urn:cheap> ?v } }";
        assert!(
            query(
                store.snapshot(),
                q,
                &QueryOptions {
                    max_rows: Some(1),
                    ..Default::default()
                }
            )
            .unwrap()
            .boolean
        );
        for options in [
            QueryOptions {
                max_rows: Some(0),
                ..Default::default()
            },
            QueryOptions {
                max_rows_produced: Some(0),
                ..Default::default()
            },
            QueryOptions {
                max_memory_bytes: Some(0),
                ..Default::default()
            },
            QueryOptions {
                cancel: Some(Arc::new(AtomicBool::new(true))),
                ..Default::default()
            },
            QueryOptions {
                deadline: Some(Instant::now() - Duration::from_secs(1)),
                ..Default::default()
            },
        ] {
            assert!(query(store.snapshot(), q, &options).is_err());
        }
        let false_then_wide =
            "ASK { { ?s <urn:wide> ?v } UNION { <urn:t> <urn:cheap> ?v FILTER(?v = 99) } }";
        let one_row = query(
            store.snapshot(),
            false_then_wide,
            &QueryOptions {
                max_rows: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(one_row.boolean);
        let mut plans = vec![&one_row.plan];
        let mut wide_rows = None;
        while let Some(p) = plans.pop() {
            if p.operator == "IndexScan" && p.description.contains("urn:wide") {
                wide_rows = Some(p.actual_rows);
            }
            plans.extend(p.children.iter());
        }
        assert_eq!(
            wide_rows,
            Some(1),
            "the remaining plain branch reads only its actual demand"
        );
        let filtered_wide = "ASK { { ?s <urn:wide> ?v FILTER(?v != ?s) } UNION { <urn:t> <urn:cheap> ?v FILTER(?v = 99) } }";
        assert!(
            query(
                store.snapshot(),
                filtered_wide,
                &QueryOptions {
                    max_rows: Some(1),
                    ..Default::default()
                }
            )
            .is_err(),
            "the retained filtered path still refuses its physical64-row input work"
        );
    }

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
                src: None,
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
