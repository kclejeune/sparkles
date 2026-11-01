//! A builder of the options of one query: the dataset's defaults, the budgets the query
//! runs under, the inferred data it reads and its cancellation.
//!
//! [`QueryRequest`] is the one recipe for the options of a caller's query. A server
//! starts it from the dataset, applies its own budgets and then those a request asks
//! for, which only lower them, and the bindings apply their callers' limits the same way.

use crate::Dataset;
use crate::reasoning::InferenceView;
use crate::sparql::QueryOptions;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// The limits a query runs under. A field that is `None` sets no limit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Budgets {
    /// how long the query may run
    pub timeout: Option<Duration>,
    /// the most rows of the answer
    pub max_rows: Option<usize>,
    /// the most bytes of memory the query's state may hold
    pub max_memory_bytes: Option<u64>,
    /// the most rows the query's operators may produce together
    pub max_rows_produced: Option<u64>,
    /// the most bytes the query may spill to disk
    pub max_spill_bytes: Option<u64>,
}

/// The smaller of two limits, where `None` is no limit.
fn lower<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

impl Budgets {
    /// Set the limits of `opts` to these, including the absent ones.
    pub fn apply(&self, opts: &mut QueryOptions) {
        opts.timeout = self.timeout;
        opts.max_rows = self.max_rows;
        opts.max_memory_bytes = self.max_memory_bytes;
        opts.max_rows_produced = self.max_rows_produced;
        opts.max_spill_bytes = self.max_spill_bytes;
    }

    /// Lower the limits of `opts` to these. A limit only lowers its counterpart, so a
    /// caller's budgets never raise those of a server, and the timeout is left alone.
    pub fn lower(&self, opts: &mut QueryOptions) {
        opts.max_memory_bytes = lower(opts.max_memory_bytes, self.max_memory_bytes);
        opts.max_spill_bytes = lower(opts.max_spill_bytes, self.max_spill_bytes);
        opts.max_rows_produced = lower(opts.max_rows_produced, self.max_rows_produced);
        opts.max_rows = lower(opts.max_rows, self.max_rows);
    }
}

/// The options of one query, built from a dataset's defaults.
#[derive(Clone, Debug)]
pub struct QueryRequest {
    options: QueryOptions,
}

impl QueryRequest {
    /// The dataset's query defaults, as [`Dataset::query_options`] gives them.
    pub fn new(ds: &Dataset) -> QueryRequest {
        QueryRequest {
            options: ds.query_options(),
        }
    }

    /// A request that starts from `options`.
    pub fn from_options(options: QueryOptions) -> QueryRequest {
        QueryRequest { options }
    }

    /// The query runs under `budgets`, in place of the limits it had.
    pub fn budgets(mut self, budgets: &Budgets) -> QueryRequest {
        budgets.apply(&mut self.options);
        self
    }

    /// The query's limits are lowered to `budgets`, as a request lowers a server's.
    pub fn lower_budgets(mut self, budgets: &Budgets) -> QueryRequest {
        budgets.lower(&mut self.options);
        self
    }

    /// The query reads the inferred data of `view`.
    pub fn with_reasoning(mut self, view: &InferenceView) -> QueryRequest {
        view.apply(&mut self.options);
        self
    }

    /// The query stops at its next check once `flag` is set.
    pub fn cancel(mut self, flag: Arc<AtomicBool>) -> QueryRequest {
        self.options.cancel = Some(flag);
        self
    }

    /// The options built so far, to change a field that the builder does not set.
    pub fn options_mut(&mut self) -> &mut QueryOptions {
        &mut self.options
    }

    /// The options of the query.
    pub fn build(self) -> QueryOptions {
        self.options
    }
}
