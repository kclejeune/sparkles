//! The choice between a bulk rebuild and a transaction for a load, a replace, a bulk
//! insert and a patch (spec X16, phase 4).
//!
//! A bulk plan builds a new generation from the head and the new quads, which costs a
//! fixed rebuild of about 20 ms and scales far better than per-quad delta inserts. A
//! transactional plan inserts each quad into the delta through a [`WriteTxn`]. Every
//! entry point that can take either path asks [`LoadPlan::choose`], and the table test
//! in `load_plan_tests.rs` records the plan each one chooses at every boundary.
//!
//! One rule serves the four entry points. A size is above the threshold only when it is
//! strictly greater. A load goes into an empty store by a rebuild unless its sources are
//! all streams that end within [`SMALL_STREAM_BYTES`]. A replace, whatever the store
//! holds, and a load into a store that holds data go by a rebuild when their estimate is
//! above the threshold or their streams hold more than [`BYTES_PER_QUAD`] bytes per
//! threshold quad, so a small replace into an empty store takes a transaction. Reading streams ahead to tell holds up to
//! that many bytes in memory before the parse starts, which is the cost of the probe. A
//! batch that a transaction stages, from a bulk insert or a patch, is rebuilt when it is
//! above the threshold. A write limited to some graphs parses its sources in a
//! transaction, since a rebuild from sources does not check each quad's graph, while its
//! batches, whose graphs are checked as they are staged, can still be rebuilt. Triple
//! protections, managed graphs, control records and a patch's delete row keep a batch in
//! the delta.
//!
//! SPARQL `LOAD` is not an entry point of the plan. It inserts per quad whatever its
//! size, so that an update keeps one memory and lock profile (spec X16, decision 4).
//!
//! [`WriteTxn`]: super::WriteTxn

use crate::error::Result;

/// Decompressed bytes of streams, about 100,000 quads, below which a load into an empty
/// store takes a transaction instead of a rebuild. A rebuild has a fixed cost of about
/// 20 ms, and it parses a stream shorter than one parallel chunk of 8 MiB on one thread,
/// so a transaction is faster up to about that size.
pub(crate) const SMALL_STREAM_BYTES: u64 = 8 << 20;

/// Bytes of text per quad that [`estimated_quads`](super::estimated_quads) assumes, by
/// which a stream probe turns the quad threshold into a byte bound.
pub(crate) const BYTES_PER_QUAD: u64 = 80;

/// The path a write takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoadPlan {
    /// One rebuild of the generation, which the commit publishes.
    Bulk,
    /// Per-quad inserts into the delta.
    Transactional,
}

/// The entry point that asks for a plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// [`Store::load_with`](super::Store::load_with)
    Load,
    /// [`Store::replace_with`](super::Store::replace_with)
    Replace,
    /// [`WriteTxn::insert_bulk`](super::WriteTxn::insert_bulk)
    InsertBulk,
    /// [`Store::patch_apply`](super::Store::patch_apply)
    Patch,
}

/// What [`LoadPlan::choose`] reads. The sizes of streams, whose length is unknown until
/// they are read, come from the probe that `choose` takes, and not from here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LoadInputs {
    pub entry: Entry,
    /// The store holds no quads.
    pub empty_head: bool,
    /// The length of the batch of a bulk insert or a patch. A load or a replace reads
    /// its size from [`Sizes`] instead.
    pub quads: u64,
    /// The number of sources of a load or a replace.
    pub sources: usize,
    /// How many of those sources are streams.
    pub streams: usize,
    /// The write is limited to some graphs through
    /// [`WriteOptions::graphs`](crate::guard::WriteOptions::graphs). It keeps sources
    /// out of a rebuild and not a batch, whose graphs are checked as it is staged.
    pub restricted: bool,
    /// The transaction checks triple-level write protections on each quad it is asked
    /// to change.
    pub triple_checks: bool,
    /// The transaction carries the capability to change managed graphs.
    pub managed: bool,
    /// The transaction carries control records.
    pub control: bool,
    /// The patch has met a delete row, whether or not the row deleted anything.
    pub deleted: bool,
    /// [`StoreOptions::bulk_threshold`](super::StoreOptions::bulk_threshold).
    pub threshold: u64,
}

impl LoadInputs {
    /// Inputs of `entry` with no sources, restriction or capability, for an entry point
    /// to fill in.
    pub(crate) fn new(entry: Entry, threshold: u64) -> Self {
        Self {
            entry,
            empty_head: false,
            quads: 0,
            sources: 0,
            streams: 0,
            restricted: false,
            triple_checks: false,
            managed: false,
            control: false,
            deleted: false,
            threshold,
        }
    }
}

/// The sizes of the sources of a load or a replace, which [`LoadPlan::choose`] asks for
/// only when they decide the plan, since finding them reads the sources.
pub(crate) trait Sizes {
    /// The estimate in quads of the sources that are not streams, from their lengths.
    fn estimate(&mut self) -> u64;
    /// Read the streams among the sources ahead and say whether they hold more than
    /// `limit` decompressed bytes.
    fn exceeds(&mut self, limit: u64) -> Result<bool>;
}

/// The sizes of a batch, which has no sources.
pub(crate) struct NoSources;

impl Sizes for NoSources {
    fn estimate(&mut self) -> u64 {
        0
    }

    fn exceeds(&mut self, _: u64) -> Result<bool> {
        Ok(false)
    }
}

impl LoadPlan {
    /// The plan for `i`, with the sizes of its sources from `sizes`.
    pub(crate) fn choose(i: &LoadInputs, sizes: &mut dyn Sizes) -> Result<LoadPlan> {
        let bulk = match i.entry {
            // A rebuild from sources does not check each quad's graph, so a write limited
            // to some graphs parses them in a transaction.
            Entry::Load | Entry::Replace if i.restricted => false,
            Entry::Load if i.empty_head => {
                // A stream that ends within a few thousand quads goes into an empty
                // store by a transaction, which costs less than the fixed cost of a
                // rebuild. Its bytes are read ahead to tell, as only a read can.
                let small_streams = i.sources > 0
                    && i.streams == i.sources
                    && !sizes.exceeds(SMALL_STREAM_BYTES)?;
                !small_streams
            }
            Entry::Load | Entry::Replace => {
                // a bound past the largest byte count is one no stream exceeds
                let bound = i.threshold.checked_mul(BYTES_PER_QUAD);
                sizes.estimate() > i.threshold
                    || match bound {
                        Some(bound) if i.streams > 0 => sizes.exceeds(bound)?,
                        _ => false,
                    }
            }
            // A batch has checked each quad's graph already, so a restriction to some
            // graphs does not keep it out of a rebuild. Triple protections, managed
            // graphs and control records check the state the transaction leaves, which
            // needs the quads in the delta, and a patch's rows apply in order once it
            // has met a delete row.
            Entry::InsertBulk | Entry::Patch => {
                i.quads > i.threshold && !i.triple_checks && !i.managed && !i.control && !i.deleted
            }
        };
        Ok(if bulk {
            LoadPlan::Bulk
        } else {
            LoadPlan::Transactional
        })
    }
}

#[cfg(test)]
#[path = "load_plan_tests.rs"]
mod tests;
