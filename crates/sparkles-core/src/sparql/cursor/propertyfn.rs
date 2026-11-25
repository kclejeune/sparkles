//! Resumable streaming reader for leaf ARQ property functions (`Kind::PropertyFn` with
//! no plan input children, item 829).
//!
//! Store-backed property functions (`list:member`, `list:index`, `list:length`,
//! `apf:container`/`bag`/`seq`/`alt`, `rdfs:member`) stream index ranges and collection
//! walks a batch at a time under the query's memory budget, and `apf:strSplit` streams
//! tokens without materializing the split vector. Scalar constant functions (`apf:concat`,
//! `apf:str`, `apf:splitIRI`, `apf:assign`, `apf:bnode`, `apf:versionARQ`) emit their
//! 0-or-1 row on first pull.
//!
//! Nothing runs before the first pull, because operators are also built only to test
//! whether an EXISTS pattern streams.
use super::Buffer;
use crate::error::Result;
use crate::sparql::arqpf::LeafReader;
use crate::sparql::ctx::Ctx;
use crate::sparql::plan::{Kind, Node};
use std::sync::Arc;

/// Whether a plan node runs as a native leaf property-function reader.
pub(super) fn eligible(node: &Node) -> bool {
    matches!(node.kind, Kind::PropertyFn(_)) && node.children.is_empty()
}

pub(super) struct PropertyFn {
    node: Node,
    opened: bool,
    reader: Option<LeafReader>,
}

impl PropertyFn {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            opened: false,
            reader: None,
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
        if !self.opened {
            self.opened = true;
            let Kind::PropertyFn(spec) = &self.node.kind else {
                unreachable!("a PropertyFn node")
            };
            let reader = LeafReader::open(ctx, spec, &self.node.vars)?;
            self.reader = Some(reader);
        }
        let Some(reader) = &mut self.reader else {
            return Ok(None);
        };
        let want = cap.max(1);
        let mut b = Buffer::new(ctx, &self.node.vars, want.min(64))?;
        let mut ended = false;
        while b.table.len < want {
            let target = if b.table.len == 0 {
                want.min(64)
            } else {
                b.table.len.saturating_mul(2).min(want)
            };
            let width = self.node.vars.len();
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
        if ended {
            self.reader = None;
        }
        b.reconcile()?;
        Ok((b.table.len > 0).then_some(b))
    }
}
