//! `COUNT(*)` over a join. The eager kernel reads both inputs whole and counts the
//! matching pairs. A cursor runs the join as its own operator over the two input
//! cursors and counts the rows it produces, as a grouping without keys whose one
//! aggregate is `COUNT(*)`. That grouping asks a merge join for its count alone, which
//! reads key runs and never forms a pair, and otherwise pulls the join's batches and
//! adds up their lengths, so the cursor holds one batch of pairs at a time.
use crate::sparql::ctx::Ctx;
use crate::sparql::plan::{Agg, Kind, Node, mk_join};
use spargebra::algebra::AggregateFunction;

/// What the plan calls the operator, which the cursor keeps for the grouping it runs.
pub(super) const OPERATOR: &str = "CountJoin";

/// How the eager kernel of a `CountJoin` node holds the pairs it counts. It counts a
/// merge join on one shared variable from the key runs, checking nothing per pair, and
/// otherwise forms the pairs, admitting them against the row limit and the work limit
/// as rows of an intermediate result before it counts them. `None` for another node.
pub(super) fn eager_checks_pairs(node: &Node) -> Option<bool> {
    let Kind::CountJoin { algo, .. } = &node.kind else {
        return None;
    };
    let [left, right] = &node.children[..] else {
        return None;
    };
    let shared = right.vars.iter().filter(|v| left.vars.contains(v)).count();
    Some(!(*algo == crate::sparql::plan::JoinAlgo::Merge && shared == 1))
}

/// Whether a cursor runs the eager kernel of a `CountJoin` node as a whole-input
/// operator instead of the join and its count. It does so for a merge join on two or
/// more shared variables, which the eager kernel counts from pair indices, while the
/// join below a streamed count writes every pair into its batches and costs 1.8 times
/// the eager time on forge (item 1129). The operator reads both inputs whole under the
/// memory budget and forms all pair indices at once, where the streamed join held one
/// batch of pairs, so this shape takes more memory under a cursor until item 1129 makes
/// the streamed join as cheap.
pub(super) fn eager_kernel(node: &Node) -> bool {
    let Kind::CountJoin { algo, .. } = &node.kind else {
        return false;
    };
    let [left, right] = &node.children[..] else {
        return false;
    };
    let shared = right.vars.iter().filter(|v| left.vars.contains(v)).count();
    *algo == crate::sparql::plan::JoinAlgo::Merge && shared >= 2
}

/// The grouping and join that answer a `CountJoin` node, or the node itself when it is
/// not one or when its eager kernel runs (`eager_kernel`).
pub(super) fn rewrite(ctx: &Ctx, node: Node) -> Node {
    let Kind::CountJoin { algo, var, keys } = &node.kind else {
        return node;
    };
    if node.children.len() != 2 || eager_kernel(&node) {
        return node;
    }
    let (algo, var, keys) = (algo.clone(), *var, keys.clone());
    let Node {
        mut children,
        vars,
        certain,
        sorted,
        est,
        cost,
        dist,
        desc,
        ..
    } = node;
    let right = children.pop().expect("a count join's right input");
    let left = children.pop().expect("a count join's left input");
    let join = mk_join(left, right, algo, keys, ctx);
    Node {
        kind: Kind::Group {
            keys: Vec::new(),
            aggs: vec![(
                var,
                Agg {
                    func: AggregateFunction::Count,
                    expr: None,
                    distinct: false,
                    fold: None,
                    registered: None,
                },
            )],
        },
        vars,
        certain,
        sorted,
        est,
        cost,
        dist,
        desc,
        children: vec![join],
    }
}
