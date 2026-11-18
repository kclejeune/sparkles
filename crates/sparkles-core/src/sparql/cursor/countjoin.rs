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

/// The grouping and join that answer a `CountJoin` node, or the node itself when it is
/// not one.
pub(super) fn rewrite(ctx: &Ctx, node: Node) -> Node {
    let Kind::CountJoin { algo, var, keys } = &node.kind else {
        return node;
    };
    if node.children.len() != 2 {
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
