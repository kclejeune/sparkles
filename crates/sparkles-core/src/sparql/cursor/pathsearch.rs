//! A resumable `SERVICE path:search` reader without input or an edge pattern and without
//! a `path:limit` (item 829, group 5): `path:shortest` and `path:kShortest` between two
//! constant endpoints, and `path:all` with at least one constant endpoint.
//!
//! Nothing runs until the first pull, because operators are also built only to see
//! whether an EXISTS pattern streams. All retained search state is charged to the query's
//! memory budget and released on completion, error or early stop.
use super::Buffer;
use crate::error::Result;
use crate::sparql::ctx::Ctx;
use crate::sparql::pathsearch::{self, Reader};
use crate::sparql::plan::{Kind, Node};
use std::sync::Arc;

/// Whether a plan node runs as a streamed path-search reader.
pub(super) fn eligible(node: &Node) -> bool {
    node.children.is_empty()
        && matches!(&node.kind, Kind::PathSearch(spec) if pathsearch::stream_eligible(spec))
}

pub(super) struct PathSearch {
    node: Node,
    opened: bool,
    reader: Option<Reader>,
    pub(super) counters: Option<serde_json::Map<String, serde_json::Value>>,
}

impl PathSearch {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            opened: false,
            reader: None,
            counters: None,
        }
    }

    pub(super) fn release(&mut self) {
        self.opened = true;
        self.reader = None;
    }

    pub(super) fn done(&self) -> bool {
        self.opened && self.reader.is_none()
    }

    pub(super) fn next(&mut self, ctx: &Arc<Ctx>, cap: usize) -> Result<Option<Buffer>> {
        let r = self.pull(ctx, cap);
        if r.is_err() {
            self.release();
        }
        r
    }

    fn pull(&mut self, ctx: &Arc<Ctx>, cap: usize) -> Result<Option<Buffer>> {
        let Kind::PathSearch(spec) = &self.node.kind else {
            unreachable!("a path search node")
        };
        if !self.opened {
            self.opened = true;
            let reader = Reader::open(ctx, spec, &self.node.vars)?;
            self.counters = Some(reader.counters().clone());
            self.reader = Some(reader);
        }
        let Some(reader) = &mut self.reader else {
            return Ok(None);
        };
        let want = cap.max(1);
        let width = self.node.vars.len();
        let mut b = Buffer::new(ctx, &self.node.vars, want.min(64))?;
        let mut ended = false;
        while b.table.len < want {
            let target = if b.table.len == 0 {
                want.min(64)
            } else {
                b.table.len.saturating_mul(2).min(want)
            };
            if target > b.table.cols.first().map_or(target, Vec::capacity) {
                let new_bytes = (target as u64)
                    .saturating_mul(width as u64 * 8)
                    .max(target as u64 * 8 * u64::from(width == 0))
                    .saturating_add(width as u64 * 4);
                b.charge
                    .resize(b.charge.bytes().saturating_add(new_bytes))?;
                for col in &mut b.table.cols {
                    if col.capacity() < target {
                        col.reserve_exact(target - col.len());
                    }
                }
                b.reconcile()?;
            }
            ended = reader.next(ctx, &mut b.table, target)?;
            if ended {
                break;
            }
        }
        self.counters = Some(reader.counters().clone());
        if ended {
            self.reader = None;
        }
        b.reconcile()?;
        Ok((b.table.len > 0).then_some(b))
    }
}
