//! The dataset's build policy, shared by the main store and all of its branches.

use super::{Generation, Store, sync_dir};
use crate::builder::{BuildOptions, VocabCodec};
use crate::{Error, Result, formats, fsutil};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

pub const INDEX_SETTINGS_FILE: &str = "index-settings.json";
pub(crate) const INDEX_SETTINGS_TEMP_FILE: &str = "index-settings.tmp";

/// Dataset-local overrides for future generation builds. A missing field inherits
/// the opening's build configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct IndexSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vocab_codec: Option<VocabCodec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patterns: Option<bool>,
}

/// Resolves an opening's declared layers and locks against the dataset-local layer.
#[doc(hidden)]
#[derive(Clone)]
pub struct IndexSettingsSource(
    Arc<dyn Fn(IndexSettings, VocabCodec) -> IndexSettings + Send + Sync>,
);

impl IndexSettingsSource {
    pub fn new(
        resolve: impl Fn(IndexSettings, VocabCodec) -> IndexSettings + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(resolve))
    }
}

#[derive(Default)]
pub(super) struct State {
    pub(super) runtime: Mutex<IndexSettings>,
    pub(super) source: RwLock<Option<IndexSettingsSource>>,
}

impl State {
    pub(super) fn build_options(
        &self,
        base: &BuildOptions,
        current: Option<VocabCodec>,
    ) -> BuildOptions {
        let runtime = self.runtime.lock();
        let source = self.source.read().clone();
        let settings = source.map_or(*runtime, |source| {
            (source.0)(*runtime, current.unwrap_or_default())
        });
        let mut build = base.clone();
        build.vocab_codec = settings.vocab_codec.or(base.vocab_codec).or(current);
        build.patterns = settings.patterns.unwrap_or(base.patterns);
        build
    }
}

impl Store {
    /// The saved dataset-local layer, before server defaults, declarations and locks.
    pub fn index_settings_layer(&self) -> IndexSettings {
        *self.family.index_settings.runtime.lock()
    }

    /// The effective settings for the next build. Inspect the snapshot to see the
    /// codec and pattern tables that a generation actually uses.
    pub fn index_settings(&self) -> IndexSettings {
        let opts = self.build_options(&self.snapshot().generation);
        IndexSettings {
            vocab_codec: Some(opts.vocab_codec.unwrap_or_default()),
            patterns: Some(opts.patterns),
        }
    }

    /// Replace the dataset-local layer. Use the main dataset to change the policy
    /// for its entire branch family.
    pub fn set_index_settings(&self, settings: IndexSettings) -> Result<()> {
        if self.is_branch() {
            return Err(Error::invalid("set index settings on the main dataset"));
        }
        let mut current = self.family.index_settings.runtime.lock();
        let Some(root) = &self.root else {
            *current = settings;
            return Ok(());
        };
        let path = root.join(INDEX_SETTINGS_FILE);
        if settings == IndexSettings::default() {
            let removed = match std::fs::remove_file(&path) {
                Ok(()) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => return Err(e.into()),
            };
            *current = settings;
            if removed {
                sync_dir(root)?;
            }
        } else {
            let bytes = formats::settings_to_vec(INDEX_SETTINGS_FILE, &settings)?;
            // Keep the runtime layer in step with the visible file even if syncing
            // its directory after the rename reports an error.
            let temp = root.join(INDEX_SETTINGS_TEMP_FILE);
            fsutil::write_atomic_via_then(&path, &temp, &bytes, || {
                *current = settings;
            })?;
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn set_index_settings_source(&self, source: IndexSettingsSource) {
        *self.family.index_settings.source.write() = Some(source);
    }

    pub(super) fn build_options(&self, generation: &Generation) -> BuildOptions {
        self.family
            .index_settings
            .build_options(&self.opts.build, Some(generation.vocab_codec()))
    }
}

pub(crate) fn read(root: &Path) -> Result<IndexSettings> {
    match std::fs::read(root.join(INDEX_SETTINGS_FILE)) {
        Ok(bytes) => formats::settings_from_slice(INDEX_SETTINGS_FILE, &bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(IndexSettings::default()),
        Err(e) => Err(e.into()),
    }
}
