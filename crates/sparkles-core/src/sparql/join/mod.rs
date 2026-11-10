//! The join kernels that both executors call.
//!
//! Each kernel knows one algorithm over column batches and nothing about the mode that
//! runs it. The eager executor hands a kernel its whole input tables as one batch and
//! runs the probe in parallel pieces, and the cursor hands it the batches it pulls. The
//! galloping search here is the one that merge joins, lower-bound zippers and the index
//! join step with.

pub(crate) mod gallop;
pub(crate) mod zipper;

pub(crate) use gallop::{gallop, gallop_to, search};
