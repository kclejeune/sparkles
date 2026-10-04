//! The dataset's settings. Each has `get`, `set` and `reset`, which match `GET`, `PUT`
//! and `DELETE` on its route: `set` stores a new value in the dataset's file and applies
//! it at once, and `reset` removes the file so the defaults apply again.

use crate::Dataset;
use crate::error::Result;
use crate::history::{CatalogHorizon, HistoryStatus, Retention, Schedule};
use crate::sparql::describe::{DescribeMode, DescribeOptions};
use crate::store::{
    ChangeLogSettings, ChangeLogStatus, CompactionMeasures, CompactionSettings, QuotaStatus,
};
use serde::Serialize;

/// The dataset's settings (from [`Dataset::settings`]).
#[derive(Clone)]
pub struct Settings {
    pub(crate) ds: Dataset,
}

impl Settings {
    /// Automatic compaction (`compaction.json`).
    pub fn compaction(&self) -> CompactionSetting {
        CompactionSetting {
            ds: self.ds.clone(),
        }
    }

    /// How DESCRIBE describes a resource (`describe.json`).
    pub fn describe(&self) -> DescribeSetting {
        DescribeSetting {
            ds: self.ds.clone(),
        }
    }

    /// The storage quota (`quota.json`).
    pub fn quota(&self) -> QuotaSetting {
        QuotaSetting {
            ds: self.ds.clone(),
        }
    }

    /// The retention window, the snapshot schedules and the catalog horizon.
    pub fn retention(&self) -> RetentionSetting {
        RetentionSetting {
            ds: self.ds.clone(),
        }
    }

    /// The change log (`changelog.json`).
    pub fn change_log(&self) -> ChangeLogSetting {
        ChangeLogSetting {
            ds: self.ds.clone(),
        }
    }
}

/// The dataset's own compaction settings. Unset ones follow the caller's base policy
/// (`serve --auto-compact-*`).
#[derive(Clone)]
pub struct CompactionSetting {
    ds: Dataset,
}

impl CompactionSetting {
    pub fn get(&self) -> CompactionSettings {
        self.ds.store().compaction_settings()
    }

    pub fn set(&self, s: CompactionSettings) -> Result<()> {
        self.ds.store().set_compaction_settings(Some(s))
    }

    pub fn reset(&self) -> Result<()> {
        self.ds.store().set_compaction_settings(None)
    }

    /// What a compaction policy looks at, measured now.
    pub fn measures(&self) -> CompactionMeasures {
        self.ds.store().compaction_measures()
    }
}

/// How DESCRIBE describes a resource.
#[derive(Clone)]
pub struct DescribeSetting {
    ds: Dataset,
}

impl DescribeSetting {
    pub fn get(&self) -> DescribeOptions {
        self.ds.store().describe_settings()
    }

    pub fn set(&self, o: DescribeOptions) -> Result<()> {
        self.ds.store().set_describe_settings(Some(o))
    }

    pub fn reset(&self) -> Result<()> {
        self.ds.store().set_describe_settings(None)
    }

    /// The setting as `GET /$/describe/{ds}` reports it.
    pub fn status(&self) -> DescribeStatus {
        DescribeStatus::of(&self.get())
    }
}

/// Whether a setting is the dataset's own or the defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum SettingSource {
    /// the defaults apply
    Default,
    /// the dataset has a setting of its own
    Dataset,
}

/// The DESCRIBE setting as `GET /$/describe/{ds}` reports it: every option, limits
/// `null` when there are none, whether the dataset has a setting of its own, and the
/// modes there are.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DescribeStatus {
    pub mode: DescribeMode,
    pub labels: bool,
    pub reifiers: bool,
    pub max_triples: Option<u64>,
    pub max_depth: Option<u32>,
    pub source: SettingSource,
    pub modes: Vec<DescribeMode>,
}

impl DescribeStatus {
    /// The status of the options `o`. They are the dataset's own unless they are the
    /// defaults.
    pub fn of(o: &DescribeOptions) -> DescribeStatus {
        DescribeStatus {
            mode: o.mode,
            labels: o.labels,
            reifiers: o.reifiers,
            max_triples: o.max_triples,
            max_depth: o.max_depth,
            source: if o.is_default() {
                SettingSource::Default
            } else {
                SettingSource::Dataset
            },
            modes: DescribeMode::ALL.to_vec(),
        }
    }
}

/// The storage quota of a persistent dataset.
#[derive(Clone)]
pub struct QuotaSetting {
    ds: Dataset,
}

impl QuotaSetting {
    /// The quota in effect and the bytes the dataset uses.
    pub fn get(&self) -> QuotaStatus {
        self.ds.store().quota()
    }

    /// A quota of `max_bytes` (0: unlimited) of the dataset's own.
    pub fn set(&self, max_bytes: u64) -> Result<QuotaStatus> {
        self.ds.store().set_quota(Some(max_bytes))
    }

    /// Remove the dataset's own quota, so that `StoreOptions::max_disk_bytes` applies.
    pub fn reset(&self) -> Result<QuotaStatus> {
        self.ds.store().set_quota(None)
    }
}

/// The history settings: the retention window, the snapshot schedules and the catalog
/// horizon.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct HistorySettings {
    pub retention: Retention,
    pub schedules: Vec<Schedule>,
    pub catalog: CatalogHorizon,
}

/// A change of the history settings, as `PUT /$/history/{ds}` makes one: a `None` field
/// keeps its value.
#[derive(Clone, Debug, Default)]
pub struct HistoryUpdate {
    pub retention: Option<Retention>,
    pub schedules: Option<Vec<Schedule>>,
    pub catalog: Option<CatalogHorizon>,
}

/// The retention window, the snapshot schedules and the catalog horizon.
#[derive(Clone)]
pub struct RetentionSetting {
    ds: Dataset,
}

impl RetentionSetting {
    pub fn get(&self) -> HistorySettings {
        let store = self.ds.store();
        HistorySettings {
            retention: store.retention(),
            schedules: store.schedules(),
            catalog: store.history().catalog,
        }
    }

    /// Apply the fields `u` sets, then collect what the retention no longer needs.
    pub fn set(&self, u: HistoryUpdate) -> Result<HistoryStatus> {
        let store = self.ds.store();
        if let Some(s) = u.schedules {
            store.set_schedules(s)?;
        }
        if let Some(c) = u.catalog {
            store.set_catalog_horizon(c)?;
        }
        match u.retention {
            Some(r) => store.set_retention(r),
            None => Ok(store.history()),
        }
    }

    /// The default retention, no schedules and the default catalog horizon.
    pub fn reset(&self) -> Result<HistoryStatus> {
        self.set(HistoryUpdate {
            retention: Some(Retention::default()),
            schedules: Some(Vec::new()),
            catalog: Some(CatalogHorizon::default()),
        })
    }
}

/// The change log's settings.
#[derive(Clone)]
pub struct ChangeLogSetting {
    ds: Dataset,
}

impl ChangeLogSetting {
    pub fn get(&self) -> ChangeLogSettings {
        self.ds.store().change_log_settings()
    }

    pub fn set(&self, s: ChangeLogSettings) -> Result<ChangeLogStatus> {
        self.ds.store().set_change_log_settings(s)
    }

    pub fn reset(&self) -> Result<ChangeLogStatus> {
        self.ds
            .store()
            .set_change_log_settings(ChangeLogSettings::default())
    }
}
