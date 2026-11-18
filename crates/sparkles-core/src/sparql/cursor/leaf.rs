//! Operators whose eager kernel answers a whole table at once, run as cursor operators
//! of their own. Two forms count as native.
//!
//! A whole-input operator reads every batch of its input cursors into charged tables,
//! runs its kernel once when they are exhausted, and serves the kernel's charged output
//! batch by batch, as a sort does. Its inputs stream as cursors of their own. This is
//! the form of a kernel that needs all of its input before it can answer any row: a
//! vector or hybrid search that ranks candidates from its input, a path search or an
//! algorithm call over its group or an edge pattern, a registered property function
//! that does not declare a prefix output, whose calls may have effects, a spatial join
//! that builds over its input, and a path of a shape the walk and the expansion do not
//! take.
//!
//! A bounded leaf reads no input. It runs its kernel once into a charged table and
//! serves it batch by batch. It counts as native only when its answer is bounded by
//! `k`, `maxHits` or a limit the planner pushed into the call, or when it is an
//! algorithm call, under decision 2 of the X14 plan. A leaf without such a bound runs
//! the same way, but it is a materialization that the strict policy refuses.
use crate::sparql::plan::{Kind, Node};

/// How an operator without an incremental cursor runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Form {
    /// It reads all of its input cursors before its kernel runs once.
    WholeInput,
    /// It reads no input, and its answer holds at most `bound` rows when one is known.
    Leaf { bound: Option<usize> },
}

impl Form {
    /// Whether the form counts as a native operator, which the strict policy admits.
    pub(super) fn native(self) -> bool {
        match self {
            Form::WholeInput => true,
            Form::Leaf { bound } => bound.is_some(),
        }
    }
}

/// The form of `node`, for a kind whose kernel runs once over whole tables, or `None`
/// for one that is not such a kind.
pub(super) fn form(node: &Node) -> Option<Form> {
    let leaf = node.children.is_empty();
    let bound = match &node.kind {
        // A vector search below an ORDER BY runs its generic plan itself when too few
        // rows have a score, so its subtree is eager.
        Kind::VectorSearch(spec) if spec.order_fallback => return None,
        Kind::VectorSearch(spec) => Some(spec.k),
        Kind::TextSearch(spec) => spec.limit,
        Kind::HybridSearch(spec) => Some(spec.limit),
        Kind::SpatialPf(spec) => spec.limit,
        Kind::PathSearch(spec) => spec.limit,
        Kind::HistoryChanges(spec) => spec.limit.map(|l| usize::try_from(l).unwrap_or(usize::MAX)),
        // Decision 2 of the plan counts an algorithm call as bounded.
        Kind::AlgoCall(_) => Some(usize::MAX),
        Kind::SpatialScan(_)
        | Kind::SpatialRelate(_)
        | Kind::ViewRead(_)
        | Kind::Service { .. }
        | Kind::PropertyFn(_) => None,
        // These read their input whatever their arguments.
        Kind::RegisteredProperty(_)
        | Kind::SpatialJoin(_)
        | Kind::Path { .. }
        | Kind::Lateral(_)
            if !leaf =>
        {
            return Some(Form::WholeInput);
        }
        _ => return None,
    };
    Some(if leaf {
        Form::Leaf { bound }
    } else {
        Form::WholeInput
    })
}

/// Why the operator of `node` holds what it holds, for its cursor plan.
pub(super) fn reason(node: &Node, form: Form) -> String {
    match form {
        Form::WholeInput => format!(
            "{} reads all of its input under the memory budget, runs once and serves its charged answer",
            node.operator()
        ),
        Form::Leaf {
            bound: Some(usize::MAX),
        } => format!(
            "{} runs once into a charged answer that it serves batch by batch",
            node.operator()
        ),
        Form::Leaf { bound: Some(n) } => format!(
            "{} answers at most {n} rows, read once into a charged answer that it serves batch by batch",
            node.operator()
        ),
        Form::Leaf { bound: None } => {
            format!("{} has no resumable cursor implementation", node.operator())
        }
    }
}
