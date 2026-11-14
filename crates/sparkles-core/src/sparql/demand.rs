//! Planning choices for a consumer that takes only the first rows of a plan, and for a
//! plan that may spill (item 827).
//!
//! An ASK and a LIMIT without ORDER BY run their input as cursors that stop once they
//! have the rows they ask for, in every execution mode. The planner therefore makes the
//! choices that suit a few rows here, so that one query has one plan whatever its mode.
//! A UNION under such a limit reads its branches in order, so its cheapest branch to
//! the first rows goes first. An ASK, or a LIMIT of one row, over a keyed join reads
//! both inputs a few rows at a time unless the input it would hold is small and fits
//! the query's budgets, and over a filtered product it streams the larger input against
//! the smaller. A LIMIT or an ASK
//! runs a small input whole when its operators would read their inputs whole anyway. A DISTINCT over an ORDER BY deduplicates below the sort
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
/// that the first matching pair ends it, unless the input it would hold, its second, is
/// estimated within a cursor batch and fits the query's budgets by the bounds that
/// [`holds_whole`] checks. Then it holds that input and probes it, which is about twice
/// as fast as reading both inputs alternately when no pair matches. Either way a hash
/// join in either input that would read one of its own inputs whole becomes an index
/// join where it can. A filtered product streams its larger input against the smaller
/// one, which it holds.
fn orient_first_row(ctx: &Ctx, node: &mut Node) {
    let filtered = below_projections(node);
    if let Kind::KeyedJoin { ripple, .. } = &mut filtered.kind {
        let batch_rows = super::cursor::CursorOptions::default().batch_rows;
        *ripple = filtered.children.len() == 2
            && !(filtered.children[1].est <= batch_rows as f64
                && holds_whole(ctx, &filtered.children[1]));
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

/// Whether a LIMIT without ORDER BY, an ASK or the per-row test of an EXISTS pattern,
/// which take `want` rows, runs its input `node` whole, as a SELECT does, rather than
/// pulling its first rows through cursor operators. It does when the planner's cost
/// of the whole input exceeds its cost of the first `want` rows by less than building
/// and running cursor operators costs, so a small input runs whole and a larger one
/// stops early. The input must also be small by upper bounds from exact index counts:
/// no operator of it may give more rows than a cursor's batch or the query's row limit,
/// and together their rows stay within half the query's memory budget. The bounds do
/// not cover every allocation, the server pool's headroom or what the query already
/// holds, so a whole run that a row or memory budget refuses gives way to pulling the
/// input ([`super::exec::whole_or_refused`]), which therefore answers whatever pulling
/// answers. A scan is bounded by the rows of
/// its prefix, a join by the product of its inputs or, where an input that always binds
/// a key meets a pattern with a constant predicate, by the rows of that input times the
/// most rows the pattern has for one key, which the predicate's statistics bound. An
/// input that evaluates expressions, such as a FILTER, a BIND or a range scan that tests
/// its rows, is pulled, since an expression may form new terms whose memory no row count
/// bounds, and so is every input of a query with a budget of produced rows, which the
/// rows its consumer takes and a refused whole run would count against as well. The choice depends on the plan, the snapshot and the query's
/// budgets, never on the execution mode.
pub(super) fn runs_whole(ctx: &Ctx, node: &Node, want: usize) -> bool {
    if ctx.calls_extensions
        || ctx.max_rows_produced != u64::MAX
        || !node.cost.is_finite()
        || !plain(node, &mut MAX_NODES.clone())
    {
        return false;
    }
    let more = node.cost - first_cost(node, want as f64, &mut MAX_NODES.clone());
    if !more.is_finite() || more > PULL_COST {
        return false;
    }
    let mut bytes = 0u64;
    upper_rows(ctx, node, &mut MAX_NODES.clone(), &mut bytes).is_some()
        && bytes <= ctx.mem_limit / 2
}

/// Whether a join may hold `input` whole in place of reading it a few rows at a time:
/// exact index bounds keep it and every operator below it within a cursor batch and the
/// query's row limit, their rows within half its memory budget, and the query has no
/// budget of produced rows, which reading the input whole would count against. The
/// bounds are those of [`runs_whole`].
fn holds_whole(ctx: &Ctx, input: &Node) -> bool {
    if ctx.calls_extensions || ctx.max_rows_produced != u64::MAX {
        return false;
    }
    let mut bytes = 0u64;
    upper_rows(ctx, input, &mut MAX_NODES.clone(), &mut bytes).is_some()
        && bytes <= ctx.mem_limit / 2
}

/// An upper bound of the rows `node` gives, when it evaluates no expression and the
/// bound of it and of each operator below it are at most [`WHOLE_ROWS`] and the query's
/// output row limit. Adds an estimate of the bytes of the rows to `bytes`, with room for
/// the index of a hash join and the growth of a table, as for the columns of every row.
fn upper_rows(ctx: &Ctx, node: &Node, left: &mut usize, bytes: &mut u64) -> Option<u64> {
    if *left == 0 {
        return None;
    }
    *left -= 1;
    let only = || (node.children.len() == 1).then(|| &node.children[0]);
    let rows = match &node.kind {
        Kind::Scan(spec) => scan_rows(ctx, spec)?,
        // a range whose ids do not all pass tests its rows with the pushed filter
        Kind::RangeScan(spec, range) if range.ranges.iter().all(|r| r.exact) => {
            scan_rows(ctx, spec)?
        }
        Kind::Values(table) => table.len() as u64,
        Kind::Empty => 0,
        Kind::Project(_) | Kind::Distinct | Kind::Sort(_) => upper_rows(ctx, only()?, left, bytes)?,
        Kind::Slice { offset, limit, .. } => upper_rows(ctx, only()?, left, bytes)?
            .saturating_sub(*offset as u64)
            .min(limit.map_or(u64::MAX, |l| l as u64)),
        Kind::Union => {
            let mut sum = 0u64;
            for branch in &node.children {
                sum = sum.saturating_add(upper_rows(ctx, branch, left, bytes)?);
            }
            sum
        }
        Kind::Join { algo, .. } if node.children.len() == 2 => {
            let [a, b] = [&node.children[0], &node.children[1]];
            let (ra, rb) = (
                upper_rows(ctx, a, left, bytes)?,
                upper_rows(ctx, b, left, bytes)?,
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
        Kind::IndexJoin(spec) if spec.probes.iter().all(|p| p.filter.is_empty()) => {
            // The input always binds the key, and each of its rows meets the rows of
            // every probe for its key. The rows the probes read for the input's keys
            // are held together, and count against the limits as well.
            let input = upper_rows(ctx, only()?, left, bytes)?;
            let mut rows = input;
            let mut read = 0u64;
            for probe in &spec.probes {
                let each = scan_rows(ctx, &probe.scan)?.min(
                    super::indexjoin::key_fanout(ctx, &probe.scan, spec.key).unwrap_or(u64::MAX),
                );
                rows = rows.saturating_mul(each);
                read = read.saturating_add(input.saturating_mul(each));
            }
            if read > WHOLE_ROWS || read > ctx.max_rows as u64 {
                return None;
            }
            *bytes = bytes.saturating_add(read.saturating_mul(4 * 16 + 64));
            rows
        }
        _ => return None,
    };
    if rows > WHOLE_ROWS || rows > ctx.max_rows as u64 {
        return None;
    }
    *bytes = bytes.saturating_add(rows.saturating_mul(node.vars.len() as u64 * 16 + 64));
    Some(rows)
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
    /// expressions.
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
        // an expression may form terms that no row count bounds
        assert!(!whole(
            &small,
            "SELECT ?p WHERE { ?p ex:knows ?x . ?x ex:name ?n FILTER(?n != \"n3\") } LIMIT 100"
        ));
        assert!(!whole(
            &small,
            "ASK { ?p ex:knows ?x . ?x ex:age ?a FILTER(?a + 1 = 8) }"
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

    /// A budget of produced rows keeps a small input pulled, since running it whole
    /// produces more rows than its consumer takes, and the consumer's rows and earlier
    /// work count against the same budget (review round 3).
    #[test]
    fn row_budgets_keep_a_small_input_pulled() {
        let small = people(300);
        let tight = QueryOptions {
            max_rows_produced: Some(100),
            ..options()
        };
        let text = "SELECT ?p ?n WHERE { ?p ex:knows ?x . ?x ex:name ?n } LIMIT 10";
        assert!(!whole_with(&small, text, &tight));
        let ample = QueryOptions {
            max_rows_produced: Some(1_000_000),
            ..options()
        };
        assert!(!whole_with(&small, text, &ample));
        assert!(whole(&small, text));
        // one row of each branch, which a whole run produces four of before the ASK's
        // fifth, while pulling produces three
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<urn:a> <urn:p> 1 .\n<urn:b> <urn:q> 1 .\n".to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let four = QueryOptions {
            max_rows_produced: Some(4),
            ..options()
        };
        let r = super::super::query(
            store.snapshot(),
            "ASK { { ?s <urn:p> 1 } UNION { ?s <urn:q> 1 } }",
            &four,
        )
        .unwrap();
        assert!(r.boolean);
    }

    /// An input that evaluates an expression is pulled, since the terms an expression
    /// forms take memory that no row count bounds: a LIMIT 1 over a BIND that forms a
    /// 64 KiB string for each of 100 rows answers within a 1 MiB budget in eager and
    /// automatic execution (review round 3). A streaming cursor evaluates the BIND for
    /// a batch of rows, as it did before.
    #[test]
    fn expressions_keep_a_small_input_pulled() {
        use super::super::cursor::{CursorOptions, ExecutionMode, QueryExecution, query_execution};
        let store = people(300);
        let big = "x".repeat(64 << 10);
        let values: String = (0..100).map(|i| format!("{i} ")).collect();
        let text = format!(
            "SELECT ?s WHERE {{ VALUES ?n {{ {values} }} BIND(CONCAT(STR(?n), \"{big}\") AS ?s) }} LIMIT 1"
        );
        let tight = QueryOptions {
            max_memory_bytes: Some(1 << 20),
            ..options()
        };
        for mode in [ExecutionMode::Eager, ExecutionMode::Auto] {
            let rows = match query_execution(
                store.snapshot(),
                &text,
                &tight,
                &CursorOptions::default(),
                mode,
            )
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"))
            {
                QueryExecution::Select(mut c) => {
                    let mut n = 0;
                    while let Some(b) = c.next_batch().unwrap() {
                        n += b.len();
                    }
                    n
                }
                QueryExecution::Eager(r) => r.result().len(),
                _ => unreachable!("a SELECT"),
            };
            assert_eq!(rows, 1, "{mode:?}");
        }
        assert!(!whole_with(&store, &text, &tight));
    }

    /// A whole run that a budget refuses gives way to pulling the input, so running a
    /// LIMIT's input whole never fails a query that pulling answers (review round 4).
    /// The test marks a LIMIT over a UNION of two 3,000-row scans to run whole, which
    /// the planner would not, under a row limit that the UNION's whole output passes
    /// and pulling its first row does not.
    #[test]
    fn a_refused_whole_run_pulls_its_input() {
        use super::super::execute_prepared;
        let store = people(3000);
        let options = QueryOptions {
            max_rows: Some(4000),
            ..options()
        };
        let text = "SELECT ?p WHERE { { ?p ex:name ?n } UNION { ?p ex:age ?a } } LIMIT 1";
        let parsed = parse_query(text, None, &options.prefixes).unwrap();
        let mut prepared = prepare_query(store.snapshot(), &parsed, &options, None, None).unwrap();
        fn mark(node: &mut Node) -> bool {
            if let Kind::Slice { whole, .. } = &mut node.kind {
                *whole = true;
                return true;
            }
            node.children.iter_mut().any(mark)
        }
        assert!(mark(&mut prepared.node));
        let result = execute_prepared(&parsed, &options, 0.0, 0.0, prepared)
            .unwrap_or_else(|failed| panic!("{}", failed.error));
        assert_eq!(result.len(), 1);
    }

    /// A range scan that tests the rows of a range with its filter, as the ranges of
    /// doubles and of the vocabulary's literals do, is pulled (review round 4).
    #[test]
    fn a_range_scan_that_tests_its_rows_is_pulled() {
        let ask = "ASK { ?p ex:age ?a FILTER(?a > 80) }";
        let ranged = |store: &Store| {
            let parsed = parse_query(ask, None, &options().prefixes).unwrap();
            let prepared =
                prepare_query(store.snapshot(), &parsed, &options(), None, None).unwrap();
            fn find(node: &Node) -> Option<bool> {
                if let Kind::RangeScan(_, range) = &node.kind {
                    return Some(range.ranges.iter().all(|r| r.exact));
                }
                node.children.iter().find_map(find)
            }
            find(&prepared.node)
        };
        // a range of integers tests the rows of the vocabulary's literals as well
        let ints = people(300);
        assert_eq!(ranged(&ints), Some(false));
        assert!(!whole(&ints, ask));
        let doubles = people(300);
        doubles
            .load(&[Source::from_bytes(
                b"<http://ex.org/q> <http://ex.org/age> \"85.5\"^^<http://www.w3.org/2001/XMLSchema#double> .\n"
                    .to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        assert_eq!(ranged(&doubles), Some(false));
        assert!(!whole(&doubles, ask));
    }
}
