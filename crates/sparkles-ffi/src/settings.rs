use crate::{ErrorKind, FfiDataset, FfiError, FfiResult};
#[derive(Clone, Debug, uniffi::Record)]
pub struct CompactionSettings {
    pub enabled: Option<bool>,
    pub min_delta_quads: Option<u64>,
    pub delta_ratio: Option<f64>,
    pub max_delta_quads: Option<u64>,
    pub max_delta_mb: Option<u64>,
    pub max_wal_mb: Option<u64>,
    pub idle_seconds: Option<u64>,
    pub max_age_seconds: Option<u64>,
    pub min_interval_seconds: Option<u64>,
    pub partial: Option<String>,
}
fn compaction(s: sparkles::store::CompactionSettings) -> CompactionSettings {
    CompactionSettings {
        enabled: s.enabled,
        min_delta_quads: s.min_delta_quads,
        delta_ratio: s.delta_ratio,
        max_delta_quads: s.max_delta_quads,
        max_delta_mb: s.max_delta_mb,
        max_wal_mb: s.max_wal_mb,
        idle_seconds: s.idle_seconds,
        max_age_seconds: s.max_age_seconds,
        min_interval_seconds: s.min_interval_seconds,
        partial: s.partial.map(|p| format!("{:?}", p).to_lowercase()),
    }
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct QuotaInfo {
    pub max_bytes: Option<u64>,
    pub default_max_bytes: Option<u64>,
    pub used_bytes: u64,
    pub source: String,
}
fn quota(q: sparkles::store::QuotaStatus) -> QuotaInfo {
    QuotaInfo {
        max_bytes: q.max_bytes,
        default_max_bytes: q.default_max_bytes,
        used_bytes: q.used_bytes,
        source: format!("{:?}", q.source).to_lowercase(),
    }
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct SnapshotSchedule {
    pub prefix: String,
    pub every_ms: u64,
    pub keep_last: u32,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct RetentionSettings {
    pub keep_commits: Option<u64>,
    pub keep_age_ms: Option<u64>,
    pub max_bytes: Option<u64>,
    pub catalog_commits: Option<u64>,
    pub catalog_age_ms: Option<u64>,
    pub schedules: Vec<SnapshotSchedule>,
}
#[uniffi::export]
impl FfiDataset {
    pub fn compaction_get(&self) -> CompactionSettings {
        compaction(self.inner.ds.settings().compaction().get())
    }
    pub fn compaction_set(&self, s: CompactionSettings) -> FfiResult<()> {
        self.inner.check_writable()?;
        let partial = s
            .partial
            .map(|s| {
                sparkles::store::PartialMode::parse(&s).ok_or_else(|| {
                    FfiError::new(ErrorKind::Invalid, "unknown partial compaction mode")
                })
            })
            .transpose()?;
        Ok(self
            .inner
            .ds
            .settings()
            .compaction()
            .set(sparkles::store::CompactionSettings {
                enabled: s.enabled,
                min_delta_quads: s.min_delta_quads,
                delta_ratio: s.delta_ratio,
                max_delta_quads: s.max_delta_quads,
                max_delta_mb: s.max_delta_mb,
                max_wal_mb: s.max_wal_mb,
                idle_seconds: s.idle_seconds,
                max_age_seconds: s.max_age_seconds,
                min_interval_seconds: s.min_interval_seconds,
                partial,
            })?)
    }
    pub fn compaction_reset(&self) -> FfiResult<()> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.settings().compaction().reset()?)
    }
    pub fn quota_get(&self) -> QuotaInfo {
        quota(self.inner.ds.settings().quota().get())
    }
    pub fn quota_set(&self, max_bytes: u64) -> FfiResult<QuotaInfo> {
        self.inner.check_writable()?;
        Ok(quota(self.inner.ds.settings().quota().set(max_bytes)?))
    }
    pub fn quota_reset(&self) -> FfiResult<QuotaInfo> {
        self.inner.check_writable()?;
        Ok(quota(self.inner.ds.settings().quota().reset()?))
    }
    pub fn retention_get(&self) -> RetentionSettings {
        let r = self.inner.ds.settings().retention().get();
        RetentionSettings {
            keep_commits: r.retention.keep_commits,
            keep_age_ms: r.retention.keep_age_ms,
            max_bytes: r.retention.max_bytes,
            catalog_commits: r.catalog.keep_commits,
            catalog_age_ms: r.catalog.keep_age_ms,
            schedules: r
                .schedules
                .into_iter()
                .map(|s| SnapshotSchedule {
                    prefix: s.prefix,
                    every_ms: s.every_ms,
                    keep_last: s.keep_last,
                })
                .collect(),
        }
    }
    pub fn retention_set(&self, r: RetentionSettings) -> FfiResult<()> {
        self.inner.check_writable()?;
        self.inner
            .ds
            .settings()
            .retention()
            .set(sparkles::handles::HistoryUpdate {
                retention: Some(sparkles::history::Retention {
                    keep_commits: r.keep_commits,
                    keep_age_ms: r.keep_age_ms,
                    max_bytes: r.max_bytes,
                }),
                catalog: Some(sparkles::history::CatalogHorizon {
                    keep_commits: r.catalog_commits,
                    keep_age_ms: r.catalog_age_ms,
                }),
                schedules: Some(
                    r.schedules
                        .into_iter()
                        .map(|s| sparkles::history::Schedule {
                            prefix: s.prefix,
                            every_ms: s.every_ms,
                            keep_last: s.keep_last,
                        })
                        .collect(),
                ),
            })?;
        Ok(())
    }
    pub fn retention_reset(&self) -> FfiResult<()> {
        self.inner.check_writable()?;
        self.inner.ds.settings().retention().reset()?;
        Ok(())
    }
}
