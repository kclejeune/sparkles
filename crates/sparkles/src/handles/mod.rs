//! The handles of a [`Dataset`]: small objects for snapshots, history, indexes,
//! settings, schema, stored queries, reasoning, validation, GraphQL and backups.
//!
//! Each handle holds a clone of the dataset, which is an `Arc`, so handles are `Clone`,
//! `Send`, `Sync` and `'static`. Their verbs are fixed: `list` returns everything, `get`
//! returns `None` for a missing item, `put` creates or replaces, `delete` returns whether
//! the item existed, and settings have `get`, `set` and `reset`. Long-running calls take
//! a [`Control`](crate::task::Control) in their `_with` form.
//!
//! ```
//! use sparkles::Dataset;
//! use sparkles::history::{At, SnapshotOptions};
//!
//! let ds = Dataset::memory();
//! ds.update("INSERT DATA { <urn:a> <urn:p> 1 }")?;
//! let (s, created) = ds
//!     .snapshots()
//!     .create("before", &At::Head, &SnapshotOptions::default())?;
//! assert!(created);
//! assert_eq!(ds.snapshots().get("before").unwrap().seq, s.seq);
//! assert!(ds.snapshots().get("missing").is_none());
//! # Ok::<_, sparkles::Error>(())
//! ```

#[cfg(feature = "backup")]
pub mod backups;
#[cfg(feature = "graphql")]
pub mod graphql;
pub mod history;
pub mod indexes;
pub mod queries;
pub mod reasoning;
pub mod schema;
pub mod settings;
pub mod validation;

#[cfg(feature = "backup")]
pub use backups::Backups;
#[cfg(feature = "graphql")]
pub use graphql::GraphQl;
pub use history::{CommitDetail, CommitRef, History, Snapshots};
pub use indexes::{
    GeoIndex, Indexes, RecallOptions, TextHit, TextHits, TextIndex, TextSearch, VectorIndexes,
};
pub use queries::StoredQueries;
pub use reasoning::{RdfsSetting, Reasoning};
pub use schema::{
    Computed, ConstraintsRequest, ReportOutcome, ReportRequest, Schema, ShapesRequest,
    schema_error_of,
};
pub use settings::{
    ChangeLogSetting, CompactionSetting, DescribeSetting, HistorySettings, HistoryUpdate,
    QuotaSetting, RetentionSetting, Settings,
};
pub use settings::{
    CompactionReadings, CompactionState, CompactionStatus, DescribeStatus, SettingSource,
};
pub use validation::{GuardOutcome, GuardSetting, Validation};

use crate::Dataset;

impl Dataset {
    /// The named snapshots (pinned commits).
    pub fn snapshots(&self) -> Snapshots {
        Snapshots { ds: self.clone() }
    }

    /// The commit history: commits, diffs, the change feed and the history's upkeep.
    pub fn history(&self) -> History {
        History { ds: self.clone() }
    }

    /// The full-text, vector and spatial indexes.
    pub fn indexes(&self) -> Indexes {
        Indexes { ds: self.clone() }
    }

    /// The dataset's settings: compaction, DESCRIBE, quota, retention and change log.
    pub fn settings(&self) -> Settings {
        Settings { ds: self.clone() }
    }

    /// Schema discovery: class profiles and draft shapes.
    pub fn schema(&self) -> Schema {
        Schema { ds: self.clone() }
    }

    /// The stored queries (`queries.json`).
    pub fn queries(&self) -> StoredQueries {
        StoredQueries { ds: self.clone() }
    }

    /// Materialized inferences and RDFS on read.
    pub fn reasoning(&self) -> Reasoning {
        Reasoning { ds: self.clone() }
    }

    /// The write guard, and SHACL and ShEx validation.
    pub fn validation(&self) -> Validation {
        Validation { ds: self.clone() }
    }

    /// The GraphQL configuration.
    #[cfg(feature = "graphql")]
    pub fn graphql(&self) -> GraphQl {
        GraphQl { ds: self.clone() }
    }

    /// The dataset's backups in `repo`.
    #[cfg(feature = "backup")]
    pub fn backups<'r>(&self, repo: &'r crate::backup::Repository) -> Backups<'r> {
        Backups {
            ds: self.clone(),
            repo,
        }
    }
}

/// An error of a satellite crate's `anyhow` result: an engine error inside it keeps its
/// variant, so cancellations and budgets stay recognizable, and anything else is
/// `Invalid` with the whole message.
#[cfg_attr(
    not(any(feature = "reasoning", feature = "shacl", feature = "shex")),
    allow(dead_code)
)]
pub(crate) fn from_anyhow(e: anyhow::Error) -> crate::Error {
    match e.downcast::<crate::Error>() {
        Ok(e) => e,
        Err(e) => crate::Error::invalid(format!("{e:#}")),
    }
}
