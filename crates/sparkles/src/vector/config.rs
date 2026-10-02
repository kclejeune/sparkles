//! Configured vector indexes of a dataset (`<root>/vector.json`) and their status
//! (`GET /$/vector/{ds}`).

use super::{MAX_DIM, MAX_K, Metric};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The configuration file of a dataset's vector indexes.
pub const CONFIG_FILE: &str = "vector.json";
/// The version of `vector.json` this build writes and reads.
pub const FORMAT_VERSION: u32 = 1;
/// Indexes per dataset.
pub const MAX_INDEXES: usize = 64;

/// Recall and speed settings of an index's HNSW graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HnswConfig {
    /// links per node (`2M` on the bottom layer); more links raise recall, memory and
    /// build time
    #[serde(default = "default_m")]
    pub m: usize,
    /// candidates kept while a node is inserted; more raise recall and build time
    #[serde(default = "default_ef_construction")]
    pub ef_construction: usize,
    /// candidates kept by a search (at least `k`); more raise recall and latency. A query
    /// can set its own with the `ef:N` option.
    #[serde(default = "default_ef_search")]
    pub ef_search: usize,
}

fn default_m() -> usize {
    16
}
fn default_ef_construction() -> usize {
    128
}
fn default_ef_search() -> usize {
    128
}

impl Default for HnswConfig {
    fn default() -> Self {
        HnswConfig {
            m: default_m(),
            ef_construction: default_ef_construction(),
            ef_search: default_ef_search(),
        }
    }
}

/// Largest `M`, `efConstruction` and `efSearch`.
pub const MAX_M: usize = 128;
pub const MAX_EF: usize = 4096;

fn default_metric() -> Metric {
    Metric::Cosine
}
fn default_hnsw() -> Option<HnswConfig> {
    Some(HnswConfig::default())
}
fn default_exact_threshold() -> usize {
    10_000
}
fn default_format_version() -> u32 {
    FORMAT_VERSION
}

/// `hnsw`: an object (missing fields take their defaults), or `false` / `null` for an
/// index searched exactly.
mod hnsw_option {
    use super::HnswConfig;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<HnswConfig>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(c) => c.serialize(s),
            None => s.serialize_bool(false),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<HnswConfig>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Flag(bool),
            Config(HnswConfig),
        }
        match Option::<Repr>::deserialize(d)? {
            None | Some(Repr::Flag(false)) => Ok(None),
            Some(Repr::Flag(true)) => Ok(Some(HnswConfig::default())),
            Some(Repr::Config(c)) => Ok(Some(c)),
        }
    }
}

/// One configured index: the body of `PUT /$/vector/{ds}/{name}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VectorIndexConfig {
    /// the embedding predicate (an IRI)
    pub predicate: String,
    /// vectors of other dimensions are not indexed (counted as `wrongDimension`)
    pub dimension: usize,
    /// the metric of the graph and the default of searches
    #[serde(default = "default_metric")]
    pub metric: Metric,
    /// a label of the embedding model (not interpreted)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// the approximate index; `false` keeps only the packed vectors (exact search)
    #[serde(default = "default_hnsw", with = "hnsw_option")]
    pub hnsw: Option<HnswConfig>,
    /// searches over at most this many rows are exact
    #[serde(default = "default_exact_threshold")]
    pub exact_threshold: usize,
}

impl VectorIndexConfig {
    /// A configuration with the defaults.
    pub fn new(predicate: &str, dimension: usize) -> VectorIndexConfig {
        VectorIndexConfig {
            predicate: predicate.into(),
            dimension,
            metric: default_metric(),
            model: None,
            hnsw: default_hnsw(),
            exact_threshold: default_exact_threshold(),
        }
    }

    /// Check the configuration; the error names the offending field.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(Error::invalid(m));
        if oxiri::Iri::parse(self.predicate.as_str()).is_err() {
            return bad(format!("predicate: {:?} is not an IRI", self.predicate));
        }
        if !(1..=MAX_DIM).contains(&self.dimension) {
            return bad(format!("dimension: must be 1..={MAX_DIM}"));
        }
        if let Some(h) = &self.hnsw {
            if !(2..=MAX_M).contains(&h.m) {
                return bad(format!("hnsw.m: must be 2..={MAX_M}"));
            }
            if !(1..=MAX_EF).contains(&h.ef_construction) {
                return bad(format!("hnsw.efConstruction: must be 1..={MAX_EF}"));
            }
            if !(1..=MAX_EF).contains(&h.ef_search) {
                return bad(format!("hnsw.efSearch: must be 1..={MAX_EF}"));
            }
        }
        if self.model.as_ref().is_some_and(|m| m.len() > 256) {
            return bad("model: at most 256 bytes".into());
        }
        Ok(())
    }

    /// FNV-1a of the fields that decide what is built (predicate, dimension, metric, `M`
    /// and `efConstruction`): the identity of an index's files. `efSearch`, the exact
    /// threshold and the model label change only searches.
    pub fn build_hash(&self) -> u64 {
        let key = serde_json::json!([
            FORMAT_VERSION,
            super::persist::FILE_VERSION,
            self.predicate,
            self.dimension,
            self.metric,
            self.hnsw.map(|h| (h.m, h.ef_construction)),
        ]);
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in serde_json::to_vec(&key).expect("serializable") {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h
    }
}

/// Check an index name: 1–64 ASCII letters, digits, `_`, `-` or `.`, not starting with
/// a dot.
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "index name {name:?}: 1 to 64 letters, digits, '_', '-' or '.', not starting with '.'"
        )))
    }
}

/// `vector.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorConfigFile {
    #[serde(default = "default_format_version")]
    pub format_version: u32,
    #[serde(default)]
    pub indexes: BTreeMap<String, VectorIndexConfig>,
}

impl Default for VectorConfigFile {
    fn default() -> Self {
        VectorConfigFile {
            format_version: FORMAT_VERSION,
            indexes: BTreeMap::new(),
        }
    }
}

impl VectorConfigFile {
    pub fn validate(&self) -> Result<()> {
        if self.format_version != FORMAT_VERSION {
            return Err(Error::invalid(format!(
                "formatVersion: {} is not supported (this build reads {FORMAT_VERSION})",
                self.format_version
            )));
        }
        let mut preds = std::collections::BTreeSet::new();
        for (n, c) in &self.indexes {
            validate_name(n)?;
            c.validate()
                .map_err(|e| Error::invalid(format!("index {n}: {e}")))?;
            if !preds.insert(&c.predicate) {
                return Err(Error::invalid(format!(
                    "index {n}: <{}> is indexed twice",
                    c.predicate
                )));
            }
        }
        Ok(())
    }
}

/// The per-query options of `spk:vectorSearch` that concern the index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchMode {
    /// `exact:true`: never the HNSW graph
    pub exact: bool,
    /// `ef:N`: candidates kept by the graph search
    pub ef: Option<usize>,
}

impl SearchMode {
    pub fn validate_ef(ef: usize) -> Result<usize> {
        if (1..=MAX_EF.max(MAX_K)).contains(&ef) {
            Ok(ef)
        } else {
            Err(Error::invalid(format!(
                "spk:vectorSearch: ef must be 1..={}",
                MAX_EF.max(MAX_K)
            )))
        }
    }
}

/// Status of one configured index (`GET /$/vector/{ds}`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorIndexStatus {
    pub name: String,
    pub predicate: String,
    pub dimension: usize,
    pub metric: Metric,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `ready`, `building`, `failed` or `over-budget`
    pub state: String,
    /// build progress (0–1) while `building`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// the generation the index was built for
    pub generation: String,
    /// vectors in the generation's base (the packed rows)
    pub rows: u64,
    /// changes since the base that searches overlay exactly
    pub overlay: VectorOverlay,
    pub skipped: VectorSkipped,
    pub memory: VectorMemory,
    /// the graph's settings, or `null` for an index searched exactly
    pub hnsw: Option<HnswStatus>,
    pub exact_threshold: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<VectorFiles>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_build: Option<VectorBuild>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorOverlay {
    pub inserts: u64,
    pub deletes: u64,
}

/// Literals of the predicate that are not indexed, by reason (rows).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorSkipped {
    pub malformed: u64,
    pub wrong_dimension: u64,
    /// indexed, but never a cosine result
    pub zero_norm: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorMemory {
    /// packed vectors, norms and term ids
    pub segment_bytes: u64,
    /// the graph's links
    pub hnsw_bytes: u64,
    /// `heap` (built in this process) or `mmap` (read in place from the index file)
    pub residency: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HnswStatus {
    pub m: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
    /// nodes in the graph (rows with one (subject, vector) pair count once)
    pub nodes: u64,
    pub layers: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorFiles {
    pub bytes: u64,
    /// read from a file written before, not built
    pub opened: bool,
}

/// A recall measurement (`POST /$/vector/{ds}/{name}/recall`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorRecall {
    pub k: usize,
    pub samples: usize,
    /// the graph search's `ef`
    pub ef: usize,
    /// recall@k against the exact search, 0–1
    pub recall: f64,
    /// mean milliseconds per search through the graph and exactly
    pub hnsw_ms: f64,
    pub exact_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorBuild {
    pub at: String,
    pub ms: f64,
    pub rows: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_forms() {
        let c: VectorIndexConfig =
            serde_json::from_str(r#"{"predicate":"http://x/emb","dimension":3}"#).unwrap();
        assert_eq!(c, VectorIndexConfig::new("http://x/emb", 3));
        assert_eq!(c.hnsw, Some(HnswConfig::default()));
        let off: VectorIndexConfig = serde_json::from_str(
            r#"{"predicate":"http://x/emb","dimension":3,"hnsw":false,"metric":"dot"}"#,
        )
        .unwrap();
        assert_eq!((off.hnsw, off.metric), (None, Metric::Dot));
        let j = serde_json::to_value(&off).unwrap();
        assert_eq!(j["hnsw"], false);
        assert_eq!(serde_json::from_value::<VectorIndexConfig>(j).unwrap(), off);
        let some: VectorIndexConfig =
            serde_json::from_str(r#"{"predicate":"http://x/emb","dimension":3,"hnsw":{"m":8}}"#)
                .unwrap();
        assert_eq!(some.hnsw.unwrap().m, 8);
        assert_eq!(some.hnsw.unwrap().ef_search, 128);
        assert!(
            serde_json::from_str::<VectorIndexConfig>(
                r#"{"predicate":"http://x/emb","dimension":3,"metric":"hamming"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<VectorIndexConfig>(
                r#"{"predicate":"http://x/emb","dimension":3,"typo":1}"#
            )
            .is_err()
        );
    }

    #[test]
    fn validation_and_hashes() {
        let ok = VectorIndexConfig::new("http://x/emb", 3);
        assert!(ok.validate().is_ok());
        let err = |c: VectorIndexConfig| c.validate().unwrap_err().to_string();
        assert!(err(VectorIndexConfig::new("not an iri", 3)).contains("predicate:"));
        assert!(err(VectorIndexConfig::new("http://x/emb", 0)).contains("dimension:"));
        let mut c = ok.clone();
        c.hnsw = Some(HnswConfig {
            m: 1,
            ..Default::default()
        });
        assert!(err(c).contains("hnsw.m"));
        // efSearch and the threshold leave the files valid; M does not
        let mut c = ok.clone();
        c.hnsw.as_mut().unwrap().ef_search = 200;
        c.exact_threshold = 5;
        assert_eq!(c.build_hash(), ok.build_hash());
        c.hnsw.as_mut().unwrap().m = 32;
        assert_ne!(c.build_hash(), ok.build_hash());
        assert!(validate_name("minilm-384_v1.2").is_ok());
        for bad in ["", ".x", "a/b", "a b", &"x".repeat(65)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
        let mut f = VectorConfigFile::default();
        f.indexes.insert("a".into(), ok.clone());
        f.indexes.insert("b".into(), ok);
        assert!(f.validate().unwrap_err().to_string().contains("twice"));
    }
}
