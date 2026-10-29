//! Private bounded A01 offline replay; supplied declarations convey no authority.
mod evolution;
mod model;
mod output;
mod resolution;

pub(crate) use model::*;
pub(crate) use output::{encode_comparison, encode_scalar};

pub(crate) use resolution::{resolve, resolve_in_scope};
