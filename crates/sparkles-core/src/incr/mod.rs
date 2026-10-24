//! Incremental view maintenance: one core for materialized views and live queries
//! (spec F13).
//!
//! A query is compiled ([`compile`]) to a circuit of operators over Z-sets, multisets of
//! id rows with signed weights ([`zset`]). Each commit's net changes enter the circuit
//! as a Z-set of quads ([`circuit::DeltaSet`], read through
//! [`Store::commit_deltas`](crate::store::Store::commit_deltas)), and the circuit
//! produces the change of its result. The [`runtime`] runs the passes of a dataset's
//! circuits after every commit, shares one circuit between the views and live queries
//! that ask the same question with the same access, keeps the [`views`] defined in the
//! dataset, and checkpoints their state ([`persist`]).

pub mod circuit;
pub mod compile;
pub mod footprint;
// pub mod live;
pub mod ops;
pub mod pass;
pub mod persist;
// pub mod runtime;
pub mod terms;
// pub mod views;
pub mod zset;

#[cfg(test)]
mod tests;

pub use circuit::{Circuit, DeltaSet};
pub use compile::{Compiled, Kind, compile};
// pub use live::{LiveEvent, LiveOptions, LiveQuery, LiveRow};
pub use ops::{Mode, PlanNode};
// pub use runtime::{Runtime, RuntimeOptions, RuntimeStats};
// pub use views::{ViewDef, ViewInfo, ViewState};

/// The IRI of the local SERVICE that reads a `table` view (`view:read`).
pub const VIEW_READ: &str = "urn:x-sparkles:view#read";
/// The namespace of the view vocabulary (`view:`).
pub const VIEW_NS: &str = "urn:x-sparkles:view#";

/// An error of the views with its stable code, which the server maps to a status.
pub fn error(code: &str, message: impl Into<String>) -> crate::Error {
    crate::Error::Component(Box::new(crate::error::ComponentError::new(
        "views", code, message,
    )))
}
