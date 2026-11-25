//! A resumable `SERVICE path:search` reader for every path search without `path:limit`
//! (items 829, 1122 and 1123): all four algorithms (`path:shortest`, `path:allShortest`,
//! `path:kShortest` and `path:all`), constant or variable endpoints, optional group input,
//! and optional nested edge patterns.
//!
//! Nothing runs until the first pull, because operators are also built only to see
//! whether an EXISTS pattern streams. When the search reads its group or a nested edge
//! pattern, those inputs are read under the memory budget on first pull before streaming
//! matching paths batch by batch. All retained search state is pre-admitted through the
//! query's memory budget and released on completion, error or early stop.
use super::{Buffer, CursorOptions, Operator, merge};
use crate::error::Result;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::pathsearch::{self, ChargedPatternEdges, Reader};
use crate::sparql::plan::{Kind, Node};
use std::sync::Arc;

/// Whether a plan node runs as a streamed path-search reader.
pub(super) fn eligible(node: &Node) -> bool {
    matches!(&node.kind, Kind::PathSearch(spec) if pathsearch::stream_eligible(spec))
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

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        cap: usize,
    ) -> Result<Option<Buffer>> {
        let r = self.pull(ctx, children, options, cap);
        if r.is_err() {
            if let (Kind::PathSearch(spec), Some(reader)) = (&self.node.kind, &self.reader) {
                self.counters = Some(reader.counters(spec));
            }
            self.release();
        }
        r
    }

    fn pull(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        cap: usize,
    ) -> Result<Option<Buffer>> {
        let Kind::PathSearch(spec) = &self.node.kind else {
            unreachable!("a path search node")
        };
        if !self.opened {
            self.opened = true;
            let reads_group = super::leaf::reads_group(&self.node);
            let input = if reads_group {
                let mut buf = Buffer::new(ctx, &children[0].vars, 0)?;
                while let Some(batch) = children[0].next(ctx, options, options.batch_rows)? {
                    merge::append(ctx, &mut buf, &batch.table, 0..batch.table.len)?;
                }
                Some((buf.table, buf.charge))
            } else {
                None
            };
            let mut charge = OwnedCharge::new(ctx, 0)?;
            let mut pattern = ChargedPatternEdges::new();
            if let Some(edge_vars) = spec.edge_pattern {
                let last = children.len() - 1;
                while let Some(batch) = children[last].next(ctx, options, options.batch_rows)? {
                    pattern.push_batch(ctx, edge_vars, &batch.table, &mut charge)?;
                }
                pattern.finish(ctx)?;
            }
            let reader = Reader::open(ctx, spec, &self.node.vars, input, pattern, charge)?;
            self.counters = Some(reader.counters(spec));
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
                for (chunk_idx, chunk) in b.table.cols.chunks_mut(4096).enumerate() {
                    if chunk_idx > 0 {
                        ctx.check()?;
                    }
                    for col in chunk {
                        if col.capacity() < target {
                            col.reserve_exact(target - col.len());
                        }
                    }
                }
                b.reconcile_checked(ctx)?;
            }
            ended = reader.next(ctx, spec, &self.node.vars, &mut b.table, target)?;
            if ended {
                break;
            }
        }
        self.counters = Some(reader.counters(spec));
        if ended {
            self.reader = None;
        }
        b.reconcile_checked(ctx)?;
        Ok((b.table.len > 0).then_some(b))
    }
}
