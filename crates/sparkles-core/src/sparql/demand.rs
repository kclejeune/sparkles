//! Planning choices for a consumer that takes only the first rows of a plan, and for a
//! plan that may spill (item 827).
//!
//! An ASK and a LIMIT without ORDER BY run their input as cursors that stop once they
//! have the rows they ask for, in every execution mode. The planner therefore makes the
//! choices that suit a few rows here, so that one query has one plan whatever its mode.
//! A UNION under such a limit reads its branches in order, so its cheapest branch to
//! the first rows goes first. An ASK, or a LIMIT of one row, over a keyed join reads
//! both inputs a few rows at a time, and over a filtered product it streams the larger
//! input against the smaller. A DISTINCT over an ORDER BY deduplicates below the sort
//! when the query may spill.

use super::ctx::Ctx;
use super::expr::{Expr, Func};
use super::plan::{JoinAlgo, Kind, Node};

/// The branches of a UNION that the planner orders at most, and the nodes of a branch
/// it inspects at most.
const MAX_BRANCHES: usize = 8;
const MAX_NODES: usize = 128;

/// Make the choices for a LIMIT of `limit` rows after `offset`, without ORDER BY, over
/// `child`. `None` takes every row and changes nothing.
pub(super) fn plan_slice(ctx: &Ctx, child: &mut Node, offset: usize, limit: Option<usize>) {
    let Some(limit) = limit else {
        return;
    };
    let want = offset.saturating_add(limit);
    if offset == 0 && limit == 1 {
        orient_first_row(ctx, child);
    }
    order_union(ctx, child, want);
}

/// Whether a LIMIT over `node` takes its rows in an order its query asked for, from
/// an ORDER BY below projections, which reads its whole input before its first row
/// whatever the demand. Such a LIMIT runs its input as it would without one.
pub(super) fn ordered_input(mut node: &Node) -> bool {
    for _ in 0..8 {
        match &node.kind {
            Kind::Project(_) if node.children.len() == 1 => node = &node.children[0],
            Kind::OrderBy { .. } | Kind::OrderedTopK(_) => return true,
            _ => return false,
        }
    }
    false
}

/// Whether `node` is a LIMIT, under projections, whose input the planner chose to run
/// whole.
pub(super) fn whole_slice(mut node: &Node) -> bool {
    for _ in 0..8 {
        match &node.kind {
            Kind::Project(_) if node.children.len() == 1 => node = &node.children[0],
            Kind::Slice { whole, .. } => return *whole,
            _ => return false,
        }
    }
    false
}

/// The node below projections, which keep every row.
fn below_projections(mut node: &mut Node) -> &mut Node {
    for _ in 0..8 {
        if !matches!(node.kind, Kind::Project(_)) || node.children.len() != 1 {
            break;
        }
        node = &mut node.children[0];
    }
    node
}

/// One row answers the query. A keyed join reads both inputs a few rows at a time, so
/// that the first matching pair ends it, and a hash join in either input that would
/// read one of its own inputs whole becomes an index join where it can. A filtered
/// product streams its larger input against the smaller one, which it holds.
fn orient_first_row(ctx: &Ctx, node: &mut Node) {
    let filtered = below_projections(node);
    if let Kind::KeyedJoin { ripple, .. } = &mut filtered.kind {
        *ripple = filtered.children.len() == 2;
        let batch_rows = super::cursor::CursorOptions::default().batch_rows;
        for input in &mut filtered.children {
            first_rows(input, ctx, batch_rows);
        }
        return;
    }
    if !matches!(filtered.kind, Kind::Filter(_)) {
        return;
    }
    let Some(product) = filtered.children.first_mut() else {
        return;
    };
    if matches!(
        product.kind,
        Kind::Join {
            algo: JoinAlgo::Cross,
            ..
        }
    ) && product.children.len() == 2
        && product.children[0].est < product.children[1].est
    {
        product.children.swap(0, 1);
    }
}

/// A ripple join reads its inputs a few rows at a time, so a hash join below it that
/// reads one input whole before its first row costs more than probing the index of a
/// pattern for each row it needs. Each hash join with a triple pattern as one input
/// becomes an index join from its other input into that pattern, when that input
/// always binds the pattern's key and the index join keeps the order the hash join
/// promised.
fn first_rows(node: &mut Node, ctx: &Ctx, batch_rows: usize) {
    for child in &mut node.children {
        first_rows(child, ctx, batch_rows);
    }
    if !matches!(
        node.kind,
        Kind::Join {
            algo: JoinAlgo::Hash,
            ..
        }
    ) || node.children.len() != 2
    {
        return;
    }
    for drive in [0, 1] {
        if let Some(joined) = super::indexjoin::index_join_for_first_rows(
            &node.children[drive],
            &node.children[1 - drive],
            ctx,
            batch_rows,
        ) && joined.sorted.starts_with(&node.sorted)
            && joined.vars.len() == node.vars.len()
        {
            *node = joined;
            return;
        }
    }
}

/// Put the branches of a UNION below `node` in the order of their cost to `want`
/// rows, from their cost summaries, when the branches call nothing that could observe
/// the order. A UNION reads its branches in order and stops when its consumer has
/// enough rows, so a cheap branch that has them all saves reading the others.
fn order_union(ctx: &Ctx, node: &mut Node, want: usize) {
    let union = below_projections(node);
    if ctx.calls_extensions
        || !matches!(union.kind, Kind::Union)
        || !(2..=MAX_BRANCHES).contains(&union.children.len())
        || !union
            .children
            .iter()
            .all(|b| plain(b, &mut MAX_NODES.clone()))
    {
        return;
    }
    let want = want as f64;
    let mut costs: Vec<(f64, Node)> = std::mem::take(&mut union.children)
        .into_iter()
        .map(|b| (first_cost(&b, want, &mut MAX_NODES.clone()), b))
        .collect();
    // a stable sort keeps the written order of branches that cost the same
    costs.sort_by(|a, b| a.0.total_cmp(&b.0));
    union.children = costs.into_iter().map(|(_, b)| b).collect();
}

/// The rows of a cursor's batch, which [`runs_whole`] compares each operator's rows
/// with.
const WHOLE_ROWS: u64 = 4096;

/// What building cursor operators and pulling rows through them costs besides the rows
/// they read, in the planner's cost units, which count rows read and formed. Pulling a
/// small input costs about eight microseconds more than running it whole, which is what
/// running a join of a few thousand rows whole costs.
const PULL_COST: f64 = 4096.0;

/// What evaluating an expression for one row costs in the planner's cost units, which
/// count a filter's or a BIND's row as one. A comparison of strings costs about as much
/// as reading a hundred rows, and one of small integers a few.
const EXPR_COST: f64 = 16.0;

/// Whether a LIMIT without ORDER BY, an ASK or the per-row test of an EXISTS pattern,
/// which take `want` rows, runs its input `node` whole, as a SELECT does, rather than
/// pulling its first rows through cursor operators. It does when the planner's cost
/// of the whole input exceeds its cost of the first `want` rows by less than building
/// and running cursor operators costs, so a small input runs whole and a selective
/// filter over a larger one stops early. The input must also be small by upper bounds
/// from exact index counts: no operator of it may give more rows than a cursor's batch,
/// and together they stay within the query's row budgets and half its memory budget,
/// so that running it whole fails no budget that pulling it would pass whatever the
/// estimates. A scan is bounded by the rows of its prefix, a join by the product of its
/// inputs or, where an input that always binds a key meets a pattern with a constant
/// predicate, by the rows of that input times the most rows the pattern has for one
/// key, which the predicate's statistics bound. The input must evaluate only
/// expressions that it may evaluate for more rows than its consumer takes. The choice
/// depends on the plan and the snapshot alone.
pub(super) fn runs_whole(ctx: &Ctx, node: &Node, want: usize) -> bool {
    if ctx.calls_extensions || !node.cost.is_finite() || !plain(node, &mut MAX_NODES.clone()) {
        return false;
    }
    // the rows past the first `want` that running whole evaluates expressions for
    let skipped = 1.0 - share(want as f64, node.est);
    let more = node.cost - first_cost(node, want as f64, &mut MAX_NODES.clone())
        + EXPR_COST * skipped * expression_rows(node, &mut MAX_NODES.clone());
    if !more.is_finite() || more > PULL_COST {
        return false;
    }
    let mut total = (0u64, 0u64);
    upper_rows(ctx, node, &mut MAX_NODES.clone(), &mut total).is_some()
        && total.0 <= ctx.max_rows_produced
        && total.1 <= ctx.mem_limit / 2
}

/// An upper bound of the rows `node` gives, when it and the bound of each operator
/// below it are at most [`WHOLE_ROWS`] and the query's output row limit. Adds the
/// rows and an estimate of their bytes to `total`.
fn upper_rows(ctx: &Ctx, node: &Node, left: &mut usize, total: &mut (u64, u64)) -> Option<u64> {
    if *left == 0 {
        return None;
    }
    *left -= 1;
    let only = || (node.children.len() == 1).then(|| &node.children[0]);
    let rows = match &node.kind {
        Kind::Scan(spec) | Kind::RangeScan(spec, _) => scan_rows(ctx, spec)?,
        Kind::Values(table) => table.len() as u64,
        Kind::Empty => 0,
        Kind::Project(_) | Kind::Filter(_) | Kind::Extend(..) | Kind::Distinct | Kind::Sort(_) => {
            upper_rows(ctx, only()?, left, total)?
        }
        Kind::Slice { offset, limit, .. } => upper_rows(ctx, only()?, left, total)?
            .saturating_sub(*offset as u64)
            .min(limit.map_or(u64::MAX, |l| l as u64)),
        Kind::Union => {
            let mut sum = 0u64;
            for branch in &node.children {
                sum = sum.saturating_add(upper_rows(ctx, branch, left, total)?);
            }
            sum
        }
        Kind::Join { algo, .. } if node.children.len() == 2 => {
            let [a, b] = [&node.children[0], &node.children[1]];
            let (ra, rb) = (
                upper_rows(ctx, a, left, total)?,
                upper_rows(ctx, b, left, total)?,
            );
            let mut rows = ra.saturating_mul(rb);
            if *algo != JoinAlgo::Cross {
                // a row that binds a shared variable meets only the other input's rows
                // with its value
                for &v in a.vars.iter().filter(|v| b.vars.contains(v)) {
                    if a.certain.contains(&v)
                        && let Some(each) = key_rows(ctx, b, v, &mut MAX_NODES.clone())
                    {
                        rows = rows.min(ra.saturating_mul(each));
                    }
                    if b.certain.contains(&v)
                        && let Some(each) = key_rows(ctx, a, v, &mut MAX_NODES.clone())
                    {
                        rows = rows.min(rb.saturating_mul(each));
                    }
                }
            }
            rows
        }
        Kind::IndexJoin(spec) => {
            // the input always binds the key, and each of its rows meets the rows of
            // every probe for its key
            let mut rows = upper_rows(ctx, only()?, left, total)?;
            for probe in &spec.probes {
                let each = scan_rows(ctx, &probe.scan)?.min(
                    super::indexjoin::key_fanout(ctx, &probe.scan, spec.key).unwrap_or(u64::MAX),
                );
                rows = rows.saturating_mul(each);
            }
            rows
        }
        _ => return None,
    };
    if rows > WHOLE_ROWS || rows > ctx.max_rows as u64 {
        return None;
    }
    total.0 = total.0.saturating_add(rows);
    total.1 = total
        .1
        .saturating_add(rows.saturating_mul(node.vars.len() as u64 * 8 + 16));
    Some(rows)
}

/// The estimated rows that the filters, BINDs and filtered index joins of `node`
/// evaluate expressions for when it runs whole.
fn expression_rows(node: &Node, left: &mut usize) -> f64 {
    if *left == 0 {
        return f64::INFINITY;
    }
    *left -= 1;
    let own = match &node.kind {
        Kind::Filter(_) | Kind::Extend(..) => node.children.first().map_or(0.0, |c| c.est),
        Kind::IndexJoin(spec) if spec.probes.iter().any(|p| !p.filter.is_empty()) => node.est,
        _ => 0.0,
    };
    own + node
        .children
        .iter()
        .map(|c| expression_rows(c, left))
        .sum::<f64>()
}

/// The rows of a scan's prefix, which bound the rows of the scan whatever its graph
/// filter and repeated variables drop.
fn scan_rows(ctx: &Ctx, spec: &super::plan::ScanSpec) -> Option<u64> {
    if let Some(n) = ctx.snap.predicate_count(spec.perm, &spec.prefix) {
        return Some(n);
    }
    ctx.snap.count(spec.perm, &spec.prefix).ok()
}

/// The most rows `node` gives for any one value of its variable `key`, for a scan
/// with a constant predicate and the operators above it that only drop rows.
fn key_rows(ctx: &Ctx, node: &Node, key: super::table::VarId, left: &mut usize) -> Option<u64> {
    if *left == 0 {
        return None;
    }
    *left -= 1;
    match &node.kind {
        Kind::Scan(spec) | Kind::RangeScan(spec, _) => super::indexjoin::key_fanout(ctx, spec, key),
        Kind::Project(_) | Kind::Filter(_) | Kind::Distinct | Kind::Sort(_)
            if node.children.len() == 1 =>
        {
            key_rows(ctx, &node.children[0], key, left)
        }
        Kind::Extend(v, _) if *v != key && node.children.len() == 1 => {
            key_rows(ctx, &node.children[0], key, left)
        }
        _ => None,
    }
}

/// Whether a branch holds only operators whose order of evaluation nothing observes:
/// scans, joins, projections and the like, whose expressions are [`effect_free`].
fn plain(node: &Node, left: &mut usize) -> bool {
    if *left == 0 {
        return false;
    }
    *left -= 1;
    let shape = match &node.kind {
        Kind::Scan(_)
        | Kind::RangeScan(..)
        | Kind::Values(_)
        | Kind::Empty
        | Kind::PatternScan(_)
        | Kind::Join { .. }
        | Kind::Union
        | Kind::Project(_)
        | Kind::Sort(_)
        | Kind::Distinct
        | Kind::Slice { .. } => true,
        Kind::Extend(_, expr) | Kind::KeyedJoin { expr, .. } => effect_free(expr, left),
        Kind::Filter(exprs) => exprs.iter().all(|e| effect_free(e, left)),
        Kind::IndexJoin(spec) => spec
            .probes
            .iter()
            .all(|p| p.filter.iter().all(|e| effect_free(e, left))),
        _ => false,
    };
    shape && node.children.iter().all(|c| plain(c, left))
}

/// Whether evaluating `expr` fewer times, or in another order, changes nothing but its
/// own values: built-in functions and operators that give the same value for the same
/// input. An EXISTS, which may call SERVICE, an extension or application function,
/// which may have effects, and RAND, UUID or BNODE are not.
fn effect_free(expr: &Expr, left: &mut usize) -> bool {
    if *left == 0 {
        return false;
    }
    *left -= 1;
    if crate::sparql::exprcache::impurity(expr).is_some() {
        return false;
    }
    match expr {
        Expr::Const(_) | Expr::Lit(..) | Expr::Var(_) | Expr::Bound(_) => true,
        Expr::Exists(_) => false,
        Expr::Call(Func::Ext(_) | Func::Registered(_), _) => false,
        Expr::Call(Func::Builtin(_) | Func::Cast(_), args) | Expr::Coalesce(args) => {
            args.iter().all(|a| effect_free(a, left))
        }
        Expr::Or(a, b)
        | Expr::And(a, b)
        | Expr::Eq(a, b)
        | Expr::SameTerm(a, b)
        | Expr::Cmp(a, b, _)
        | Expr::Arith(a, b, _) => effect_free(a, left) && effect_free(b, left),
        Expr::Not(a) | Expr::Neg(a) | Expr::Pos(a) => effect_free(a, left),
        Expr::In(a, list) => effect_free(a, left) && list.iter().all(|e| effect_free(e, left)),
        Expr::If(a, b, c) => effect_free(a, left) && effect_free(b, left) && effect_free(c, left),
    }
}

/// The share of `rows` estimated rows that `want` of them are.
fn share(want: f64, rows: f64) -> f64 {
    if !rows.is_finite() || rows <= 1.0 {
        return 1.0;
    }
    (want / rows).min(1.0)
}

/// The estimated cost of the first `want` rows of `node`, from the cost and rows the
/// planner summarized for it and its inputs. A streaming operator costs the share of
/// its cost that its first rows take, an operator that reads an input whole before its
/// first row costs that input whole, and a UNION costs its branches in order until
/// they have the rows. A node whose summary is not finite costs its whole cost.
fn first_cost(node: &Node, want: f64, left: &mut usize) -> f64 {
    let whole = if node.cost.is_finite() && node.cost >= 0.0 {
        node.cost
    } else {
        f64::INFINITY
    };
    if *left == 0 || !node.est.is_finite() || node.est < 0.0 {
        return whole;
    }
    *left -= 1;
    let own = |inputs: f64| (whole - inputs).max(0.0) * share(want, node.est);
    // the input rows that give `want` output rows, at the node's estimated ratio
    let input_want = |input: &Node| want * (input.est.max(1.0) / node.est.max(1.0)).max(1.0);
    let cost = match &node.kind {
        Kind::Scan(_) | Kind::RangeScan(..) | Kind::PatternScan(_) | Kind::Values(_) => {
            whole * share(want, node.est)
        }
        Kind::Empty => 0.0,
        Kind::Project(_) | Kind::Extend(..) | Kind::Filter(_) | Kind::IndexJoin(_)
            if node.children.len() == 1 =>
        {
            let input = &node.children[0];
            first_cost(input, input_want(input), left) + own(input.cost)
        }
        Kind::Slice { offset, .. } if node.children.len() == 1 => {
            let input = &node.children[0];
            first_cost(input, want + *offset as f64, left) + own(input.cost)
        }
        Kind::Union => {
            let mut cost = 0.0;
            let mut need = want;
            for branch in &node.children {
                cost += first_cost(branch, need, left);
                need -= branch.est.max(0.0);
                if need <= 0.0 {
                    break;
                }
            }
            cost
        }
        Kind::Join { algo, .. } if node.children.len() == 2 => {
            let [a, b] = [&node.children[0], &node.children[1]];
            let inputs = a.cost + b.cost;
            if *algo == JoinAlgo::Merge {
                // both inputs stream in key order
                first_cost(a, input_want(a), left)
                    + first_cost(b, input_want(b), left)
                    + own(inputs)
            } else {
                // a hash join or a product holds its smaller input whole and streams
                // the larger, as the cursor's binary join does
                let probe = usize::from(a.est < b.est);
                let (streamed, held) = (&node.children[probe], &node.children[1 - probe]);
                held.cost + first_cost(streamed, input_want(streamed), left) + own(inputs)
            }
        }
        Kind::KeyedJoin { ripple: true, .. } if node.children.len() == 2 => {
            let [a, b] = [&node.children[0], &node.children[1]];
            first_cost(a, input_want(a), left)
                + first_cost(b, input_want(b), left)
                + own(a.cost + b.cost)
        }
        // a sort, a grouping and the rest read their input whole first
        _ => whole,
    };
    if cost.is_finite() {
        cost.min(whole)
    } else {
        whole
    }
}

/// `SELECT DISTINCT … ORDER BY` deduplicates below its sort when the query may spill
/// (item 85), so that the DISTINCT can pass rows to later passes: the DISTINCT of a
/// projection of an ORDER BY becomes a projection of an ORDER BY of a numbered DISTINCT
/// of the projection. Equal projected rows have equal sort keys when every key is a pure
/// expression of projected variables without EXISTS, and the numbered DISTINCT keeps
/// each row's first input number, on which the sort orders ties, so the rows and their
/// order are those that sorting first gives. Other shapes keep the DISTINCT above the
/// sort. The planner makes this choice in every execution mode when the query may
/// spill, and the eager executor numbers the rows as the cursor does.
pub(super) fn distinct_below_order(ctx: &Ctx, node: Node) -> Node {
    let rewrites = matches!(node.kind, Kind::Distinct)
        && node.children.len() == 1
        && matches!(&node.children[0].kind, Kind::Project(vars) if *vars == node.vars && !vars.is_empty())
        && node.children[0].children.len() == 1
        && match &node.children[0].children[0].kind {
            Kind::OrderBy { keys, limit: None } => keys.iter().all(|(key, _)| {
                !key.has_exists()
                    && crate::sparql::exprcache::impurity(key).is_none()
                    && key.var_set().iter().all(|v| node.vars.contains(v))
            }),
            _ => false,
        };
    if !rewrites {
        return node;
    }
    let mut distinct = node;
    let mut project = distinct.children.pop().expect("a projection");
    let mut order = project.children.pop().expect("an ORDER BY");
    let input = order.children.pop().expect("the sorted input");
    let Kind::OrderBy { mut keys, .. } = order.kind else {
        unreachable!("an ORDER BY")
    };
    let vars = distinct.vars.clone();
    let number = ctx.var("\u{1}distinct number");
    let below = super::plan::project(input, vars.clone(), ctx);
    let mut numbered = Node::leaf(
        Kind::Distinct,
        [vars.clone(), vec![number]].concat(),
        below.est,
        "below the ORDER BY, so that it may spill".into(),
    );
    numbered.certain = [below.certain.clone(), vec![number]].concat();
    numbered.cost = below.cost;
    numbered.children = vec![below];
    keys.push((Expr::Var(number), true));
    let mut sort = Node::leaf(
        Kind::OrderBy { keys, limit: None },
        numbered.vars.clone(),
        numbered.est,
        format!("{} then the first input row", order.desc),
    );
    sort.certain = numbered.certain.clone();
    sort.cost = numbered.cost + numbered.est * numbered.est.max(2.0).log2() * 0.5;
    sort.children = vec![numbered];
    project.children.clear();
    super::plan::project(sort, vars, ctx)
}

/// Whether a DISTINCT numbers its input rows: it has one variable more than its input,
/// the last, which the input does not bind.
pub(super) fn numbered_distinct(node: &Node) -> bool {
    matches!(node.kind, Kind::Distinct)
        && node.children.first().is_some_and(|child| {
            child.vars.len() + 1 == node.vars.len()
                && node.vars.last().is_some_and(|v| !child.vars.contains(v))
        })
}

#[cfg(test)]
mod tests {
    use super::super::{QueryOptions, parse_query, prepare_query};
    use super::*;
    use crate::io::{RdfFormat, Source};
    use crate::store::{Store, StoreOptions};

    fn people(n: usize) -> Store {
        let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
        for i in 0..n {
            data.push_str(&format!(
                "ex:p{i} ex:name \"n{i}\" ; ex:age {} ; ex:knows ex:p{} .\n",
                i % 90,
                (i * 7 + 1) % n
            ));
        }
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        store
    }

    fn options() -> QueryOptions {
        QueryOptions {
            prefixes: vec![("ex".into(), "http://ex.org/".into())],
            no_cache: true,
            ..Default::default()
        }
    }

    /// Whether the plan of `text` has a LIMIT that runs its input whole.
    fn whole(store: &Store, text: &str) -> bool {
        whole_with(store, text, &options())
    }

    fn whole_with(store: &Store, text: &str, options: &QueryOptions) -> bool {
        let parsed = parse_query(text, None, &options.prefixes).unwrap();
        let prepared = prepare_query(store.snapshot(), &parsed, options, None, None).unwrap();
        fn find(node: &Node) -> bool {
            matches!(node.kind, Kind::Slice { whole: true, .. }) || node.children.iter().any(find)
        }
        find(&prepared.node)
    }

    /// The planner runs a small input whole, bounding a join by the most rows a
    /// pattern with a constant predicate has for one key, and pulls an input that is
    /// larger than a cursor batch, costs much more than its first rows or evaluates
    /// expressions with effects.
    #[test]
    fn small_limit_inputs_run_whole() {
        let small = people(300);
        let large = people(5000);
        let ask = "ASK { ?p ex:knows ?x . ?x ex:age 7 }";
        let join = "SELECT ?p ?n WHERE { ?p ex:knows ?x . ?x ex:name ?n } LIMIT 10";
        assert!(whole(&small, ask));
        assert!(whole(&small, join));
        // 300 rows each give a product of 90,000, but each person has one name
        assert!(whole(&small, &join.replace("LIMIT 10", "LIMIT 500")));
        assert!(!whole(&large, join), "5,000 rows exceed a batch");
        // a string comparison for each of 300 rows costs more than the cursor
        assert!(!whole(
            &small,
            "SELECT ?p WHERE { ?p ex:knows ?x . ?x ex:name ?n FILTER(?n != \"n3\") } LIMIT 100"
        ));
        // RAND gives other values when it runs for more rows
        assert!(!whole(
            &small,
            "SELECT ?p ?r WHERE { ?p ex:knows ?x . ?x ex:name ?n BIND(RAND() AS ?r) } LIMIT 10"
        ));
        // a product has no key to bound it by
        assert!(!whole(
            &small,
            "SELECT ?p ?q WHERE { ?p ex:name ?n . ?q ex:name ?m } LIMIT 10"
        ));
        // a LIMIT over an ORDER BY sorts its input whatever the demand
        assert!(!whole(
            &small,
            "SELECT ?p WHERE { ?p ex:knows ?x . ?x ex:name ?n } ORDER BY ?n LIMIT 10"
        ));
    }

    /// A tight budget of produced rows keeps a small input pulled, since running it
    /// whole produces more rows than its consumer takes.
    #[test]
    fn row_budgets_keep_a_small_input_pulled() {
        let small = people(300);
        let tight = QueryOptions {
            max_rows_produced: Some(100),
            ..options()
        };
        let text = "SELECT ?p ?n WHERE { ?p ex:knows ?x . ?x ex:name ?n } LIMIT 10";
        assert!(!whole_with(&small, text, &tight));
        assert!(whole(&small, text));
    }
}
