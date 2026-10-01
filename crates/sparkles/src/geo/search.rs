//! Search kernels over a snapshot's spatial rows: window and nearest-first searches over
//! the base and overlay trees and the tail, checked against the snapshot.
//!
//! Stub: every search is refused.

use super::column::ColumnEntry;
use super::geom::Geom;
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::Ctx;
use crate::sparql::plan::GraphFilter;
use std::sync::Arc;

/// A candidate row: the quad and its geometry.
#[derive(Clone)]
pub struct Hit {
    pub s: Id,
    pub p: Id,
    pub o: Id,
    pub g: Id,
    pub entry: Arc<ColumnEntry>,
}

/// What a search did (explain counters).
#[derive(Clone, Debug, Default)]
pub struct SearchStats {
    /// rows whose envelope matched
    pub candidates: u64,
    /// exact tests run
    pub refined: u64,
    /// rows that passed the exact test
    pub matched: u64,
    /// tree nodes visited
    pub nodes: u64,
    /// the index was not used
    pub fallback: bool,
}

fn not_yet() -> Error {
    Error::Unsupported("spatial index search is not supported yet".into())
}

/// Rows of the predicates `preds` whose envelope intersects one of the CRS84 `windows`,
/// in chunks.
pub fn window(
    ctx: &Ctx,
    preds: &[Id],
    windows: &[[f64; 4]],
    graph: &GraphFilter,
    st: &mut SearchStats,
    sink: &mut dyn FnMut(&[Hit]) -> Result<()>,
) -> Result<()> {
    let _ = (ctx, preds, windows, graph, st, sink);
    Err(not_yet())
}

/// Rows of the predicates `preds` in increasing lower bound of their distance to `q`,
/// in chunks with that bound; the sink returns `false` to stop.
pub fn nearest(
    ctx: &Ctx,
    preds: &[Id],
    q: &Geom,
    graph: &GraphFilter,
    st: &mut SearchStats,
    sink: &mut dyn FnMut(&[Hit], f64) -> Result<bool>,
) -> Result<()> {
    let _ = (ctx, preds, q, graph, st, sink);
    Err(not_yet())
}
