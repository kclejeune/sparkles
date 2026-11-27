//! The grouping kernels that both executors call.
//!
//! One accumulator trait describes how an aggregate folds the rows of its groups, and
//! the grouping operator built on it assigns each input row to a group, either through a
//! hash table or by the runs of equal keys in an input that the keys order, and hands
//! the rows to the accumulators a batch at a time. The module knows nothing about the
//! mode that runs it. The eager executor hands the operator its whole input table as one
//! batch, and the cursor hands it the batches it pulls.

pub(crate) mod acc;
mod deferred;
pub(crate) mod op;
pub(crate) mod par;

pub(crate) use op::{Grouping, hashable, order_free, whole};
