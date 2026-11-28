//! C19 layers for the generation build policy (X11).

use super::{
    Kind, Providers, Scope, Settings, Typed, check_as, defaults_of, normalize_as,
    resolve_with_index_codec,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sparkles::builder::{BuildOptions, VocabCodec};
use sparkles::store::{INDEX_SETTINGS_FILE, IndexSettings, IndexSettingsSource};
use std::sync::Arc;

pub static INDEX: Kind = Kind {
    name: "index",
    scope: Scope::Dataset,
    file: INDEX_SETTINGS_FILE,
    members: &["vocabCodec", "patterns"],
    removable: &[],
    shared: false,
    defaults: defaults_of::<Effective>,
    normalize: normalize_as::<Effective>,
    check: check_as::<Effective>,
};

pub(super) fn is(kind: &Kind) -> bool {
    kind.name == INDEX.name
}

pub(super) fn validate_layer(v: &Value) -> serde_json::Result<()> {
    serde_json::from_value::<IndexSettings>(v.clone()).map(|_| ())
}

#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct Effective {
    vocab_codec: VocabCodec,
    patterns: bool,
}

impl Default for Effective {
    fn default() -> Self {
        Self {
            vocab_codec: VocabCodec::Front,
            patterns: true,
        }
    }
}

impl Typed for Effective {
    fn validate(&self, _: Providers) -> Result<(), String> {
        Ok(())
    }
}

impl Settings {
    pub(crate) fn configure_index_settings(
        &self,
        catalog: &sparkles::Catalog,
        build: &BuildOptions,
    ) {
        let mut declared = (*self.declared()).clone();
        declared.index_defaults = IndexSettings {
            vocab_codec: build.vocab_codec,
            patterns: Some(build.patterns),
        };
        self.declared.store(Arc::new(declared));
        let declared = self.declared.clone();
        catalog.set_index_settings_sources(move |name| source(&declared, name));
    }
}

pub(super) fn current_codec(kind: &Kind, ds: &crate::state::Dataset) -> Option<VocabCodec> {
    is(kind).then(|| ds.store.snapshot().generation.vocab_codec())
}

pub(super) fn build_defaults(declared: &super::Declared, current: Option<VocabCodec>) -> Value {
    let invocation = declared.index_defaults;
    serde_json::to_value(Effective {
        vocab_codec: invocation.vocab_codec.or(current).unwrap_or_default(),
        patterns: invocation.patterns.unwrap_or(true),
    })
    .expect("index defaults serialize")
}

fn source(
    declarations: &Arc<arc_swap::ArcSwap<super::Declared>>,
    name: &str,
) -> IndexSettingsSource {
    let declarations = declarations.clone();
    let name = name.to_owned();
    IndexSettingsSource::new(move |runtime, current| {
        let r = resolve_with_index_codec(
            &INDEX,
            &declarations.load(),
            &name,
            serde_json::to_value(runtime).expect("index settings serialize"),
            Providers::Unchecked,
            Some(current),
        );
        // Every layer was validated where it entered, and the two settings are independent.
        let effective: Effective =
            serde_json::from_value(r.effective).expect("resolved index settings are valid");
        IndexSettings {
            vocab_codec: (r.sources.get(["vocabCodec".to_string()].as_slice()) != Some(&"default"))
                .then_some(effective.vocab_codec),
            patterns: Some(effective.patterns),
        }
    })
}
