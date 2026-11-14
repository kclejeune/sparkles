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
use super::expr::Expr;
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

/// Whether a branch holds only operators whose order of evaluation nothing observes.
fn plain(node: &Node, left: &mut usize) -> bool {
    if *left == 0 {
        return false;
    }
    *left -= 1;
    matches!(
        node.kind,
        Kind::Scan(_)
            | Kind::RangeScan(..)
            | Kind::Values(_)
            | Kind::Empty
            | Kind::PatternScan(_)
            | Kind::Join { .. }
            | Kind::KeyedJoin { .. }
            | Kind::IndexJoin(_)
            | Kind::Union
            | Kind::Project(_)
            | Kind::Sort(_)
            | Kind::Extend(..)
            | Kind::Filter(_)
            | Kind::Distinct
            | Kind::Slice { .. }
    ) && node.children.iter().all(|c| plain(c, left))
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
                // the hash join holds one input whole and streams the other
                // the larger input of a hash join, and the first of a product
                let probe = usize::from(*algo == JoinAlgo::Hash && a.est < b.est);
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
