//! Full-text search over string literals (cargo feature `text`, backed by Tantivy).
//!
//! One document per quad `(s, p, o, g)` whose object is a string or language-tagged
//! literal (and whose predicate and graph are in the configured scope). Documents are
//! keyed by a hash of the four terms' vocabulary keys, so the index does not depend on
//! generation-specific ids and survives compaction untouched.
//!
//! The index is derived data kept consistent with the store inside the commit path:
//! every snapshot carries a [`TextView`] whose `seq` is the commit it reflects, and a
//! search runs only when that equals the snapshot's commit. `<root>/text.json` holds the
//! configuration; `<root>/text/` the Tantivy index.
//!
//! A write only stages its documents in the Tantivy writer; the Tantivy commit, which
//! flushes a segment and costs more than the indexing, is deferred: the views of the
//! commits in between share a slot that the next search needing one of them fills (read
//! your writes), as do a background tick about once a second, compaction and close. A
//! view may so search a later state than its own: documents of quads its commit does not
//! have are filtered out against the snapshot, and documents of removed quads stay until
//! their batch is sealed, so every view still finds all of its own. Commits write
//! without fsync (see `lazydir`): the WAL is the durable record, the index is
//! checkpointed about once a second, and on open an index that may hold unsynced data is
//! verified, then caught up from the WAL (which also covers what was only staged). It is
//! rebuilt from RDF only when it is missing, damaged, or behind the WAL.

use crate::error::{Error, Result};

#[cfg(feature = "text")]
mod cjk;
#[cfg(feature = "text")]
mod highlight;
#[cfg(feature = "text")]
mod lazydir;
#[cfg(feature = "text")]
mod lucene;
#[cfg(feature = "text")]
mod porter;
#[cfg(feature = "text")]
mod search;
#[cfg(feature = "text")]
mod sloppy;
use serde::{Deserialize, Serialize};

/// Which predicates are indexed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PredicateSet {
    /// every predicate
    #[default]
    All,
    Only(Vec<String>),
}

impl Serialize for PredicateSet {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            PredicateSet::All => s.serialize_str("all"),
            PredicateSet::Only(v) => v.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for PredicateSet {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Word(String),
            List(Vec<String>),
        }
        match Repr::deserialize(d)? {
            Repr::Word(w) if w == "all" => Ok(PredicateSet::All),
            Repr::Word(w) => Err(serde::de::Error::custom(format!(
                "predicates: expected \"all\" or a list of IRIs, got {w:?}"
            ))),
            Repr::List(v) => Ok(PredicateSet::Only(v)),
        }
    }
}

impl PredicateSet {
    pub fn contains(&self, iri: &str) -> bool {
        match self {
            PredicateSet::All => true,
            PredicateSet::Only(v) => v.iter().any(|p| p == iri),
        }
    }
}

/// Graphs whose quads are indexed (IRIs; `urn:x-arq:DefaultGraph` for the default graph).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphScope {
    #[serde(default)]
    pub include: PredicateSet,
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// Full-text configuration of a dataset (`text.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextConfig {
    #[serde(default)]
    pub predicates: PredicateSet,
    #[serde(default)]
    pub graphs: GraphScope,
    /// longer literals are indexed truncated (the documents keep the full term)
    #[serde(default = "default_max_text_bytes")]
    pub max_text_bytes: usize,
    /// hits one `text:query` without a limit may return
    #[serde(default = "default_max_hits")]
    pub max_hits: usize,
    /// compression of the stored documents: `zstd` (the default when built with zstd),
    /// `lz4` or `none`; changing it rebuilds the index
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docstore_compression: Option<DocstoreCompression>,
    /// per-language analyzers: a literal whose language tag has one is also indexed
    /// stemmed, and a search with that language searches the stemmed text
    #[serde(default, skip_serializing_if = "Languages::is_none")]
    pub languages: Languages,
}

/// A language analyzer: Tantivy's Snowball stemmer for the language, after the stop
/// words of the language are removed (where Tantivy has a list for it), `porter`, the
/// English stop words and Porter's stemmer as in Lucene's English analyzer, or `cjk`,
/// the bigrams of Chinese, Japanese and Korean text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Analyzer {
    Arabic,
    Danish,
    Dutch,
    English,
    Finnish,
    French,
    German,
    Greek,
    Hungarian,
    Italian,
    Norwegian,
    Portuguese,
    Romanian,
    Russian,
    Spanish,
    Swedish,
    Tamil,
    Turkish,
    /// overlapping bigrams of Han, Hiragana, Katakana and Hangul, as Lucene's
    /// `CJKAnalyzer` makes them (no stemmer, no dictionary)
    Cjk,
    /// English with the Porter stemmer of Lucene's `EnglishAnalyzer` (and so of
    /// jena-text), where `english` uses the Snowball English stemmer; no tag has it by
    /// default
    Porter,
}

impl Analyzer {
    /// The languages whose default analyzer is `cjk`. `"all"` leaves them out, since it
    /// names the stemmed languages; an index lists them.
    pub const CJK: [&'static str; 3] = ["ja", "ko", "zh"];

    /// Every analyzer with the language tag it is used for by default.
    pub const ALL: [(&'static str, Analyzer); 18] = [
        ("ar", Analyzer::Arabic),
        ("da", Analyzer::Danish),
        ("de", Analyzer::German),
        ("el", Analyzer::Greek),
        ("en", Analyzer::English),
        ("es", Analyzer::Spanish),
        ("fi", Analyzer::Finnish),
        ("fr", Analyzer::French),
        ("hu", Analyzer::Hungarian),
        ("it", Analyzer::Italian),
        ("nl", Analyzer::Dutch),
        ("no", Analyzer::Norwegian),
        ("pt", Analyzer::Portuguese),
        ("ro", Analyzer::Romanian),
        ("ru", Analyzer::Russian),
        ("sv", Analyzer::Swedish),
        ("ta", Analyzer::Tamil),
        ("tr", Analyzer::Turkish),
    ];

    /// The analyzer of a primary language tag, if there is one (`nb` and `nn` are
    /// Norwegian too).
    pub fn for_tag(tag: &str) -> Option<Analyzer> {
        let tag = match tag {
            "nb" | "nn" => "no",
            t => t,
        };
        if Self::CJK.contains(&tag) {
            return Some(Analyzer::Cjk);
        }
        Self::ALL.iter().find(|(t, _)| *t == tag).map(|(_, a)| *a)
    }

    /// The analyzer of a name (`english`, `cjk`, …), in any case.
    pub fn named(name: &str) -> Option<Analyzer> {
        let name = name.to_ascii_lowercase();
        Self::ALL
            .iter()
            .map(|(_, a)| *a)
            .chain([Analyzer::Cjk, Analyzer::Porter])
            .find(|a| a.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            Analyzer::Arabic => "arabic",
            Analyzer::Danish => "danish",
            Analyzer::Dutch => "dutch",
            Analyzer::English => "english",
            Analyzer::Finnish => "finnish",
            Analyzer::French => "french",
            Analyzer::German => "german",
            Analyzer::Greek => "greek",
            Analyzer::Hungarian => "hungarian",
            Analyzer::Italian => "italian",
            Analyzer::Norwegian => "norwegian",
            Analyzer::Portuguese => "portuguese",
            Analyzer::Romanian => "romanian",
            Analyzer::Russian => "russian",
            Analyzer::Spanish => "spanish",
            Analyzer::Swedish => "swedish",
            Analyzer::Tamil => "tamil",
            Analyzer::Turkish => "turkish",
            Analyzer::Cjk => "cjk",
            Analyzer::Porter => "porter",
        }
    }
}

/// The per-language analyzers of an index (`languages` in `text.json`): none, `"all"`
/// (every language Tantivy has a stemmer for, by its tag), a list of language tags, or
/// a map from a language tag to an analyzer name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Languages {
    #[default]
    None,
    All,
    /// primary language tag (lowercase) → analyzer
    Only(std::collections::BTreeMap<String, Analyzer>),
}

impl Languages {
    pub fn is_none(&self) -> bool {
        match self {
            Languages::None => true,
            Languages::All => false,
            Languages::Only(m) => m.is_empty(),
        }
    }

    /// The analyzed languages: each primary tag with its analyzer, by tag.
    pub fn resolve(&self) -> Vec<(String, Analyzer)> {
        match self {
            Languages::None => Vec::new(),
            Languages::All => {
                let mut v: Vec<(String, Analyzer)> = Analyzer::ALL
                    .iter()
                    .map(|(t, a)| (t.to_string(), *a))
                    .collect();
                v.extend(["nb", "nn"].map(|t| (t.to_string(), Analyzer::Norwegian)));
                v.sort();
                v
            }
            Languages::Only(m) => m.iter().map(|(t, a)| (t.clone(), *a)).collect(),
        }
    }

    /// Languages from tags (`all`, or tags each with its default analyzer).
    pub fn from_tags<S: AsRef<str>>(tags: &[S]) -> std::result::Result<Languages, String> {
        if tags.iter().any(|t| t.as_ref().eq_ignore_ascii_case("all")) {
            return Ok(Languages::All);
        }
        let mut m = std::collections::BTreeMap::new();
        for t in tags {
            let tag = primary_tag(t.as_ref())?;
            let a = Analyzer::for_tag(&tag).ok_or_else(|| {
                format!(
                    "languages: no analyzer for {tag:?}; name one, as in {{\"{tag}\": \"english\"}}"
                )
            })?;
            m.insert(tag, a);
        }
        Ok(Languages::Only(m))
    }
}

/// A primary language subtag, lowercased: 2 to 8 ASCII letters.
fn primary_tag(t: &str) -> std::result::Result<String, String> {
    let tag = t.trim().to_ascii_lowercase();
    if (2..=8).contains(&tag.len()) && tag.bytes().all(|b| b.is_ascii_lowercase()) {
        Ok(tag)
    } else {
        Err(format!(
            "languages: {t:?} is not a primary language tag such as \"en\" (2 to 8 letters)"
        ))
    }
}

impl Serialize for Languages {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Languages::None => s.serialize_none(),
            Languages::All => s.serialize_str("all"),
            Languages::Only(m) => m.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Languages {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Word(String),
            List(Vec<String>),
            Map(std::collections::BTreeMap<String, String>),
        }
        let err = serde::de::Error::custom;
        match Option::<Repr>::deserialize(d)? {
            None => Ok(Languages::None),
            Some(Repr::Word(w)) if w == "all" => Ok(Languages::All),
            Some(Repr::Word(w)) => Err(err(format!(
                "languages: expected \"all\", a list of language tags or a map of tags to analyzers, got {w:?}"
            ))),
            Some(Repr::List(v)) => Languages::from_tags(&v).map_err(err),
            Some(Repr::Map(m)) => {
                let mut out = std::collections::BTreeMap::new();
                for (t, a) in m {
                    let tag = primary_tag(&t).map_err(err)?;
                    let a = Analyzer::named(&a).ok_or_else(|| {
                        err(format!(
                            "languages: unknown analyzer {a:?} (one of {}, cjk)",
                            Analyzer::ALL.map(|(_, a)| a.name()).join(", ")
                        ))
                    })?;
                    out.insert(tag, a);
                }
                Ok(Languages::Only(out))
            }
        }
    }
}

/// Compression of the full-text index's document store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocstoreCompression {
    Zstd,
    Lz4,
    None,
}

impl DocstoreCompression {
    /// The effective choice: the configured one, else zstd when this build has it.
    pub fn effective(c: Option<DocstoreCompression>) -> DocstoreCompression {
        c.unwrap_or(if cfg!(feature = "zstd") {
            DocstoreCompression::Zstd
        } else {
            DocstoreCompression::Lz4
        })
    }
}

fn default_max_text_bytes() -> usize {
    256 << 10
}
fn default_max_hits() -> usize {
    1_000_000
}

impl Default for TextConfig {
    fn default() -> Self {
        TextConfig {
            predicates: PredicateSet::All,
            graphs: GraphScope::default(),
            max_text_bytes: default_max_text_bytes(),
            max_hits: default_max_hits(),
            docstore_compression: None,
            languages: Languages::None,
        }
    }
}

/// Jena's `highlight:` options of `text:query`: the literal output becomes the best
/// fragments of the literal with the matched words marked.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HighlightOpts {
    /// fragments kept at most (`m:`, 3)
    pub max_frags: usize,
    /// the length of a fragment in characters (`z:`, 128)
    pub frag_size: usize,
    /// the marks around a match (`s:` and `e:`, ↦ and ↤)
    pub start: String,
    pub end: String,
    /// between fragments (`f:`, ∣)
    pub frag_sep: String,
    /// marks consecutive matches as one (`jh:`, yes)
    pub join_hi: bool,
    /// merges adjacent fragments (`jf:`, yes)
    pub join_frags: bool,
}

impl Default for HighlightOpts {
    fn default() -> Self {
        HighlightOpts {
            max_frags: 3,
            frag_size: 128,
            start: "\u{21a6}".into(),
            end: "\u{21a4}".into(),
            frag_sep: "\u{2223}".into(),
            join_hi: true,
            join_frags: true,
        }
    }
}

impl HighlightOpts {
    /// Parse the options after `highlight:`, separated by `|`, such as
    /// `s:<em> | e:</em> | z:150`.
    pub fn parse(s: &str) -> std::result::Result<HighlightOpts, String> {
        let mut o = HighlightOpts::default();
        for opt in s.split('|').map(str::trim).filter(|o| !o.is_empty()) {
            let (key, val) = opt
                .split_once(':')
                .ok_or_else(|| format!("highlight option {opt:?} is not key:value"))?;
            let num = |v: &str| {
                v.trim()
                    .parse::<usize>()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| format!("highlight option {key}: needs a positive number"))
            };
            let flag = |v: &str| match v.trim().to_ascii_lowercase().as_str() {
                "y" | "yes" | "true" => Ok(true),
                "n" | "no" | "false" => Ok(false),
                _ => Err(format!("highlight option {key}: is y or n")),
            };
            match key.trim() {
                "m" => o.max_frags = num(val)?,
                "z" => o.frag_size = num(val)?,
                "s" => o.start = val.to_string(),
                "e" => o.end = val.to_string(),
                "f" => o.frag_sep = val.to_string(),
                "jh" => o.join_hi = flag(val)?,
                "jf" => o.join_frags = flag(val)?,
                k => return Err(format!("unknown highlight option {k:?}")),
            }
        }
        Ok(o)
    }
}

/// The IRI naming the default graph in text documents (`?g` of a default-graph hit).
pub const DEFAULT_GRAPH_IRI: &str = "urn:x-arq:DefaultGraph";

/// State of a dataset's full-text index.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextStatus {
    pub enabled: bool,
    /// `ready`, `stale` (behind the store after a failed update) or `failed`
    pub state: String,
    pub docs: u64,
    pub seq: u64,
    pub store_seq: u64,
    pub epoch: u64,
    pub disk_bytes: u64,
    pub segments: usize,
    pub config: TextConfig,
    pub format_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_rebuild: Option<RebuildInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RebuildInfo {
    pub at: String,
    pub ms: f64,
    pub docs: u64,
}

/// The search state a snapshot sees.
pub struct TextView {
    /// the commit the view reflects
    pub seq: u64,
    /// +1 per rebuild (part of result-cache keys)
    pub epoch: u64,
    /// the batch the view belongs to: the searcher, once its commits are sealed
    #[cfg(feature = "text")]
    pub(crate) slot: std::sync::Arc<imp::Slot>,
    #[cfg(feature = "text")]
    pub(crate) index: std::sync::Arc<imp::Shared>,
    /// the index that seals the batch on demand
    #[cfg(feature = "text")]
    pub(crate) owner: std::sync::Weak<imp::Inner>,
}

impl std::fmt::Debug for TextView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TextView(seq {}, epoch {})", self.seq, self.epoch)
    }
}

#[cfg_attr(not(feature = "text"), allow(dead_code))]
pub(crate) fn unavailable(name: &str, state: &str, index_seq: u64, data_seq: u64) -> Error {
    Error::TextUnavailable(format!(
        "full-text index of '{name}' is {state} (index seq {index_seq}, data seq {data_seq}); retry later or rebuild it"
    ))
}

/// Error for a build without the `text` feature.
pub fn not_built() -> Error {
    Error::Unsupported("built without full-text search (cargo feature \"text\")".into())
}

/// What [`probe`] found in a dataset's full-text index (read only, for `sparkles check`).
#[derive(Debug, Default)]
pub(crate) struct TextProbe {
    /// `text.json` exists
    pub configured: bool,
    /// `text.json` does not parse
    pub config_error: Option<String>,
    /// this build has no full-text support: only the configuration was read
    pub unsupported: bool,
    /// `<root>/text/` exists
    pub index_dir: bool,
    /// `text.dirty` exists: the index may hold unsynced writes
    pub dirty: bool,
    /// the index could not be opened (or has another schema)
    pub open_error: Option<String>,
    pub payload: Option<ProbePayload>,
    pub payload_error: Option<String>,
    pub segments: usize,
    pub docs: u64,
    /// segment files looked at
    pub files: usize,
    /// (file relative to `text/`, problem)
    pub damaged: Vec<(String, String)>,
}

/// The commit payload of the index's `meta.json`.
#[derive(Debug)]
pub(crate) struct ProbePayload {
    pub format_ok: bool,
    pub seq: u64,
    pub config_matches: bool,
}

#[cfg(feature = "text")]
pub(crate) use imp::probe;

/// Without full-text support only the configuration can be checked.
#[cfg(not(feature = "text"))]
pub(crate) fn probe(root: &std::path::Path, _checksums: bool) -> TextProbe {
    let mut p = TextProbe::default();
    match std::fs::read(root.join("text.json")) {
        Ok(b) => {
            p.configured = true;
            p.unsupported = true;
            if let Err(e) = serde_json::from_slice::<TextConfig>(&b) {
                p.config_error = Some(e.to_string());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            p.configured = true;
            p.config_error = Some(e.to_string());
        }
    }
    p
}

#[cfg(feature = "text")]
pub use imp::TextIndex;
#[cfg(feature = "text")]
pub(crate) use imp::read_config as imp_read_config;
#[cfg(feature = "text")]
pub use search::{search, search_in};

#[cfg(not(feature = "text"))]
/// Placeholder: full-text search is not compiled in.
pub struct TextIndex;

#[cfg(not(feature = "text"))]
pub fn search(
    _ctx: &crate::sparql::ctx::Ctx,
    _spec: &crate::sparql::plan::TextSpec,
    _vars: &[crate::sparql::table::VarId],
) -> Result<crate::sparql::table::Table> {
    Err(not_built())
}

#[cfg(not(feature = "text"))]
pub fn search_in(
    _ctx: &crate::sparql::ctx::Ctx,
    _spec: &crate::sparql::plan::TextSpec,
    _vars: &[crate::sparql::table::VarId],
    _subjects: Option<&[crate::id::Id]>,
    _lenient: bool,
) -> Result<crate::sparql::table::Table> {
    Err(not_built())
}

#[cfg(feature = "text")]
mod imp {
    use super::lazydir::LazySyncDir;
    use super::*;
    use crate::id::{Id, Tag};
    use crate::store::Snapshot;
    use parking_lot::Mutex;
    use rustc_hash::{FxHashMap, FxHashSet};
    use sha2::Digest;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, Instant};
    use tantivy::schema::{
        BytesOptions, FAST, Field, IndexRecordOption, STRING, Schema, TextFieldIndexing,
        TextOptions,
    };
    use tantivy::tokenizer::{
        AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer,
    };
    use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

    /// A bulk commit is applied to the index document by document when it adds at most
    /// one document per `BULK_SHARE` the index holds, or `BULK_MIN`; more are faster to
    /// index by a rebuild, which runs several writer threads.
    const BULK_SHARE: usize = 8;
    const BULK_MIN: usize = 20_000;

    /// The index format. Format 2 keeps each document's terms in columns (fast fields),
    /// where format 1 kept them in the doc store. An index of another format is rebuilt
    /// on open.
    const FORMAT: u32 = 2;
    const TOKENIZER: &str = "sparkles_standard";

    #[derive(Clone)]
    pub(crate) struct Fields {
        key: Field,
        pub(super) s: Field,
        pub(super) p: Field,
        o: Field,
        pub(super) g: Field,
        pub(super) lang: Field,
        pub(super) text: Field,
        /// the stemmed text of each analyzed language (`text_<tag>`), by primary tag
        stemmed: Vec<(String, Field, Analyzer)>,
    }

    impl Fields {
        /// The stemmed field of a language tag's primary subtag and its analyzer, if it
        /// has one.
        fn stemmed_for(&self, tag: &str) -> Option<(Field, Analyzer)> {
            let primary = tag.split('-').next().unwrap_or(tag);
            self.stemmed
                .binary_search_by(|(t, ..)| t.as_str().cmp(primary))
                .ok()
                .map(|i| (self.stemmed[i].1, self.stemmed[i].2))
        }

        /// The field a search with language `lang` searches: the language's stemmed
        /// text when it has an analyzer, else the standard text (`None`).
        pub(super) fn text_for(&self, lang: Option<&str>) -> (Field, Option<Analyzer>) {
            match lang.and_then(|l| self.stemmed_for(l)) {
                Some((f, a)) => (f, Some(a)),
                None => (self.text, None),
            }
        }
    }

    fn schema(config: &TextConfig) -> (Schema, Fields) {
        let mut b = Schema::builder();
        let bytes_indexed = BytesOptions::default().set_indexed();
        let text = |tokenizer: &str| {
            TextOptions::default().set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer(tokenizer)
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions),
            )
        };
        // A hit's terms are read from columns (fast fields), not from stored documents:
        // a column read costs far less than decompressing a doc store block.
        let fields = Fields {
            key: b.add_bytes_field("key", bytes_indexed.clone()),
            s: b.add_bytes_field("s", bytes_indexed.set_fast()),
            p: b.add_text_field("p", STRING | FAST),
            o: b.add_bytes_field("o", BytesOptions::default().set_fast()),
            g: b.add_text_field("g", STRING | FAST),
            lang: b.add_text_field("lang", STRING),
            text: b.add_text_field("text", text(TOKENIZER)),
            stemmed: config
                .languages
                .resolve()
                .into_iter()
                .map(|(tag, a)| {
                    let f = b.add_text_field(&format!("text_{tag}"), text(&tokenizer_name(a)));
                    (tag, f, a)
                })
                .collect(),
        };
        (b.build(), fields)
    }

    /// The longest token the text field indexes, in bytes.
    pub(super) const MAX_TOKEN: usize = 40;

    fn tokenizer_name(a: Analyzer) -> String {
        format!("sparkles_{}", a.name())
    }

    fn register_tokenizer(index: &Index) {
        index.tokenizers().register(
            TOKENIZER,
            TextAnalyzer::builder(SimpleTokenizer::default())
                .filter(RemoveLongFilter::limit(MAX_TOKEN))
                .filter(LowerCaser)
                .filter(AsciiFoldingFilter)
                .build(),
        );
        for a in Analyzer::ALL
            .map(|(_, a)| a)
            .into_iter()
            .chain([Analyzer::Cjk, Analyzer::Porter])
        {
            index
                .tokenizers()
                .register(&tokenizer_name(a), language_analyzer(a));
        }
    }

    /// A language's analyzer: the standard tokens, lowercased, without the language's
    /// stop words, and stemmed. As in Lucene's language analyzers, the stems are not
    /// ASCII-folded: folding would merge words the language keeps apart, such as
    /// Swedish `städer` and `stad` or Spanish `año` and `ano`. Removed stop words leave
    /// gaps in the positions, as in Lucene, so a phrase across one still needs the gap.
    pub(super) fn language_analyzer(a: Analyzer) -> TextAnalyzer {
        use tantivy::tokenizer::{Language as L, Stemmer, StopWordFilter};
        let lang = match a {
            Analyzer::Cjk => {
                return TextAnalyzer::builder(super::cjk::CjkTokenizer)
                    .filter(RemoveLongFilter::limit(MAX_TOKEN))
                    .filter(LowerCaser)
                    .build();
            }
            Analyzer::Porter => {
                return TextAnalyzer::builder(SimpleTokenizer::default())
                    .filter(RemoveLongFilter::limit(MAX_TOKEN))
                    .filter(LowerCaser)
                    .filter(StopWordFilter::new(L::English).expect("English stop words"))
                    .filter(super::porter::PorterStemmer)
                    .build();
            }
            Analyzer::Arabic => L::Arabic,
            Analyzer::Danish => L::Danish,
            Analyzer::Dutch => L::Dutch,
            Analyzer::English => L::English,
            Analyzer::Finnish => L::Finnish,
            Analyzer::French => L::French,
            Analyzer::German => L::German,
            Analyzer::Greek => L::Greek,
            Analyzer::Hungarian => L::Hungarian,
            Analyzer::Italian => L::Italian,
            Analyzer::Norwegian => L::Norwegian,
            Analyzer::Portuguese => L::Portuguese,
            Analyzer::Romanian => L::Romanian,
            Analyzer::Russian => L::Russian,
            Analyzer::Spanish => L::Spanish,
            Analyzer::Swedish => L::Swedish,
            Analyzer::Tamil => L::Tamil,
            Analyzer::Turkish => L::Turkish,
        };
        let b = TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(MAX_TOKEN))
            .filter(LowerCaser)
            .dynamic();
        let b = match StopWordFilter::new(lang) {
            Some(stop) => b.filter_dynamic(stop),
            None => b,
        };
        b.filter_dynamic(Stemmer::new(lang)).build()
    }

    /// A text field's analysis without its tokenizer: a prefix, wildcard, fuzzy or
    /// regular expression term is normalized as the indexed tokens are, but not split or
    /// stemmed. Terms of the standard text (`None`) are lowercased and ASCII-folded, and
    /// those of a language only lowercased, as Lucene's analyzers normalize them.
    pub(super) fn normalizer(analyzer: Option<Analyzer>) -> TextAnalyzer {
        let b = TextAnalyzer::builder(tantivy::tokenizer::RawTokenizer::default())
            .filter(LowerCaser)
            .dynamic();
        match analyzer {
            None => b.filter_dynamic(AsciiFoldingFilter).build(),
            Some(_) => b.build(),
        }
    }

    /// What every view of one index generation shares.
    pub(crate) struct Shared {
        pub(crate) fields: Fields,
        pub(crate) config: TextConfig,
        /// ids of terms the searches of this index have looked up
        pub(crate) ids: super::search::IdCache,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Payload {
        format: u32,
        seq: u64,
        epoch: u64,
        config: String,
    }

    /// What the views of one batch search: a searcher at or after their commits, and
    /// the documents in it that may not match a view's commit (hashes of their subject
    /// and object keys), which are checked against the snapshot.
    pub(crate) struct Resolved {
        pub(crate) searcher: tantivy::Searcher,
        pub(super) uncertain: Arc<FxHashSet<u64>>,
    }

    /// The views of the commits applied between two seals share a slot, set when the
    /// batch is sealed (to `None` if the index failed first).
    #[derive(Default)]
    pub(crate) struct Slot(OnceLock<Option<Resolved>>);

    impl Slot {
        fn sealed(searcher: tantivy::Searcher, uncertain: Arc<FxHashSet<u64>>) -> Arc<Slot> {
            let slot = Slot::default();
            let _ = slot.0.set(Some(Resolved {
                searcher,
                uncertain,
            }));
            Arc::new(slot)
        }
    }

    impl super::TextView {
        /// The searcher of this view, sealing its batch first if needed.
        pub(crate) fn resolved(&self) -> Result<&Resolved> {
            if self.slot.0.get().is_none() {
                match self.owner.upgrade() {
                    Some(owner) => owner.seal_for(&self.slot),
                    None => {
                        let _ = self.slot.0.set(None);
                    }
                }
            }
            match self.slot.0.get() {
                Some(Some(r)) => Ok(r),
                _ => Err(unavailable("dataset", "stale", self.seq, self.seq)),
            }
        }
    }

    impl Live {
        fn writer(&mut self) -> Result<&mut IndexWriter<TantivyDocument>> {
            self.writer
                .as_mut()
                .ok_or_else(|| text_err("the index is closed"))
        }
    }

    struct Live {
        shared: Arc<Shared>,
        /// `None` only once the index is closing
        writer: Option<IndexWriter<TantivyDocument>>,
        reader: IndexReader,
        /// the on-disk directory (`None` in memory or while an index is being built)
        dir: Option<LazySyncDir>,
        /// the first commit since the last checkpoint
        dirty_since: Option<Instant>,
        /// the commit the index reflects (staged or committed), and the one its on-disk
        /// payload names (behind after commits that changed no document, while documents
        /// of removed quads are kept, and until the next Tantivy commit)
        applied: u64,
        committed: u64,
        /// operations (additions and deletions) no Tantivy commit has made visible yet
        staged: usize,
        /// the batch of the views handed out since the last seal, if any
        open: Option<Arc<Slot>>,
        /// the last sealed batch: views of later commits share it while they change
        /// no document
        last: Arc<Slot>,
        /// documents the searcher of the next seal may hold for quads some view of it
        /// does not have (see [`Resolved`])
        uncertain: FxHashSet<u64>,
        /// documents of quads removed since the last seal (key → hash), deleted once it
        /// is sealed: views of the batch from before the removal still find them
        removed: FxHashMap<[u8; 16], u64>,
        /// the first commit that removed one of them
        removed_since: Option<u64>,
    }

    /// The index shared by its [`TextIndex`], the commit tick and the views (which seal
    /// their batch on demand).
    pub(crate) struct Inner {
        root: Option<PathBuf>,
        config: TextConfig,
        live: Mutex<Live>,
        epoch: AtomicU64,
        /// set when an update's text maintenance failed: queries get 503 until a rebuild
        stale: Mutex<Option<String>>,
        last_rebuild: Mutex<Option<RebuildInfo>>,
        fail_next_commit: AtomicBool,
        /// the tick seals and checkpoints (off only in tests)
        ticks: AtomicBool,
        /// the quads of the commits since an online rebuild's snapshot, while one runs
        journal: Mutex<Option<Vec<[Id; 4]>>>,
        /// held for the whole of a rebuild, so that two never build at once
        rebuilding: Mutex<()>,
    }

    /// An online rebuild in progress: other rebuilds wait, and commits are kept in the
    /// journal until it is installed or dropped.
    pub struct RebuildGuard<'a> {
        inner: &'a Arc<Inner>,
        _guard: parking_lot::MutexGuard<'a, ()>,
    }

    impl RebuildGuard<'_> {
        fn take(&self) -> Vec<[Id; 4]> {
            self.inner.journal.lock().take().unwrap_or_default()
        }
    }

    impl Drop for RebuildGuard<'_> {
        fn drop(&mut self) {
            *self.inner.journal.lock() = None;
        }
    }

    /// An index built from a snapshot, not yet in use (see [`TextIndex::build`]).
    pub struct Built {
        index: Index,
        fields: Fields,
        dir: Option<PathBuf>,
        /// the epoch the index gets once installed
        epoch: u64,
        docs: u64,
        started: Instant,
    }

    /// A dataset's full-text index.
    pub struct TextIndex {
        inner: Arc<Inner>,
        /// the commit tick: dropping the sender stops it
        ticker: Option<(std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>)>,
    }

    /// The hash that stands for a document in [`Resolved::uncertain`].
    pub(super) fn doc_hash(s: &[u8], o: &[u8]) -> u64 {
        key_hash(s) ^ key_hash(o).rotate_left(32)
    }

    /// One term's part of [`doc_hash`] (a search hashes each distinct term once).
    pub(super) fn key_hash(k: &[u8]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = rustc_hash::FxHasher::default();
        k.hash(&mut h);
        h.finish()
    }

    fn config_hash(c: &TextConfig) -> String {
        let bytes = serde_json::to_vec(c).unwrap();
        let d = sha2::Sha256::digest(&bytes);
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub(super) fn text_err(e: impl std::fmt::Display) -> Error {
        Error::Invalid(format!("full-text index: {e}"))
    }

    /// `<root>/text.json`, if the dataset has full-text search enabled.
    pub(crate) fn read_config(root: &Path) -> Result<Option<TextConfig>> {
        match std::fs::read(root.join("text.json")) {
            Ok(b) => serde_json::from_slice(&b)
                .map(Some)
                .map_err(|e| Error::Invalid(format!("text.json: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// A new, empty index (in `dir`, replacing anything there, or in memory).
    fn new_index(dir: Option<&Path>, config: &TextConfig) -> Result<(Index, Fields)> {
        let (schema, fields) = schema(config);
        let docstore_compression = match DocstoreCompression::effective(config.docstore_compression)
        {
            #[cfg(feature = "zstd")]
            DocstoreCompression::Zstd => {
                tantivy::store::Compressor::Zstd(tantivy::store::ZstdCompressor {
                    compression_level: Some(3),
                })
            }
            #[cfg(not(feature = "zstd"))]
            DocstoreCompression::Zstd => {
                return Err(Error::Unsupported(
                    "full-text index: docstoreCompression zstd needs a build with zstd".into(),
                ));
            }
            DocstoreCompression::Lz4 => tantivy::store::Compressor::Lz4,
            DocstoreCompression::None => tantivy::store::Compressor::None,
        };
        let builder = Index::builder()
            .schema(schema)
            .settings(tantivy::IndexSettings {
                docstore_compression,
                ..Default::default()
            });
        let index = match dir {
            None => builder.create_in_ram().map_err(text_err)?,
            Some(d) => {
                if d.exists() {
                    std::fs::remove_dir_all(d)?;
                }
                std::fs::create_dir_all(d)?;
                builder.create_in_dir(d).map_err(text_err)?
            }
        };
        register_tokenizer(&index);
        Ok((index, fields))
    }

    /// Marker of an index that may hold unsynced writes (next to `<root>/text/`).
    fn marker(root: &Path) -> PathBuf {
        root.join("text.dirty")
    }

    /// Open the index in `<root>/text/` for maintenance without per-commit fsync.
    fn open_index(root: &Path, config: &TextConfig) -> Result<(Index, Fields, LazySyncDir)> {
        let dir = LazySyncDir::open(&root.join("text"), marker(root))?;
        let index = Index::open(dir.clone()).map_err(text_err)?;
        register_tokenizer(&index);
        let (expected, fields) = schema(config);
        if index.schema() != expected {
            return Err(text_err("unexpected index schema"));
        }
        Ok((index, fields, dir))
    }

    /// Check an index that may hold unsynced writes: every file of the committed
    /// segments must exist and match its checksum.
    fn verify(index: &Index) -> Result<()> {
        let d = index.directory();
        for meta in index.searchable_segment_metas().map_err(text_err)? {
            for f in meta.list_files() {
                // `list_files` names a deletes file even for segments that have none
                if meta.delete_opstamp().is_none() && f.extension().is_some_and(|e| e == "del") {
                    continue;
                }
                let ok = tantivy::directory::Directory::exists(d, &f).map_err(text_err)?
                    && d.validate_checksum(&f).map_err(text_err)?;
                if !ok {
                    return Err(text_err(format!("{} is missing or damaged", f.display())));
                }
            }
        }
        Ok(())
    }

    /// Inspect `<root>/text` without writing anything: the configuration, the commit
    /// payload, and that every file of the committed segments exists (and, with
    /// `checksums`, matches its checksum). A server may commit and merge meanwhile, so
    /// damage is re-checked against a fresh `meta.json` before it is reported.
    pub(crate) fn probe(root: &Path, checksums: bool) -> super::TextProbe {
        use tantivy::directory::Directory;
        let mut p = super::TextProbe::default();
        let config = match read_config(root) {
            Ok(Some(c)) => c,
            Ok(None) => return p,
            Err(e) => {
                p.configured = true;
                p.config_error = Some(e.to_string());
                return p;
            }
        };
        p.configured = true;
        p.dirty = marker(root).exists();
        let dir = root.join("text");
        p.index_dir = dir.is_dir();
        if !p.index_dir {
            return p;
        }
        for attempt in 0..3 {
            // opening reads `meta.json` and the managed file list; nothing is written
            let index = match Index::open_in_dir(&dir) {
                Ok(i) => i,
                Err(e) => {
                    p.open_error = Some(e.to_string());
                    return p;
                }
            };
            if index.schema() != schema(&config).0 {
                p.open_error = Some("unexpected index schema".into());
                return p;
            }
            let meta = match index.load_metas() {
                Ok(m) => m,
                Err(e) => {
                    p.open_error = Some(e.to_string());
                    return p;
                }
            };
            p.payload = None;
            p.payload_error = None;
            match meta.payload.as_deref().map(serde_json::from_str::<Payload>) {
                Some(Ok(pl)) => {
                    p.payload = Some(super::ProbePayload {
                        format_ok: pl.format == FORMAT,
                        seq: pl.seq,
                        config_matches: pl.config == config_hash(&config),
                    })
                }
                Some(Err(e)) => p.payload_error = Some(e.to_string()),
                None => p.payload_error = Some("no commit payload".into()),
            }
            p.segments = meta.segments.len();
            p.docs = meta.segments.iter().map(|s| s.num_docs() as u64).sum();
            p.files = 0;
            p.damaged.clear();
            let d = index.directory();
            for m in &meta.segments {
                for f in m.list_files() {
                    // `list_files` names a deletes file even for segments that have none
                    if m.delete_opstamp().is_none() && f.extension().is_some_and(|e| e == "del") {
                        continue;
                    }
                    p.files += 1;
                    let problem = match Directory::exists(d, &f) {
                        Ok(false) => Some("missing".to_string()),
                        Ok(true) if checksums => match d.validate_checksum(&f) {
                            Ok(true) => None,
                            Ok(false) => Some("checksum mismatch".to_string()),
                            Err(e) => Some(e.to_string()),
                        },
                        Ok(true) => None,
                        Err(e) => Some(e.to_string()),
                    };
                    if let Some(problem) = problem {
                        p.damaged.push((f.display().to_string(), problem));
                    }
                }
            }
            if p.damaged.is_empty() || attempt == 2 {
                break;
            }
            // damage is reported only against the latest committed state
            match index.load_metas() {
                Ok(m) if m.opstamp != meta.opstamp => continue,
                _ => break,
            }
        }
        p
    }

    /// How long the index may hold unsynced commits before a checkpoint, and the period
    /// of the tick that seals batches.
    const CHECKPOINT_AFTER: Duration = Duration::from_secs(1);

    /// Staged operations at which a write seals its batch itself. Commit time grows with
    /// the batch (roughly 20 ms for 10k operations, 55 ms for 40k, measured on 1k-triple
    /// updates), and the first search after a burst would pay it.
    const STAGED_MAX: usize = 16_384;

    /// The single-threaded writer and the reader used between rebuilds.
    fn live_of(
        index: Index,
        fields: Fields,
        config: &TextConfig,
        dir: Option<LazySyncDir>,
        seq: u64,
    ) -> Result<Live> {
        let writer = index
            .writer_with_num_threads(1, 32 << 20)
            .map_err(text_err)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(text_err)?;
        Ok(Live {
            shared: Arc::new(Shared {
                fields,
                config: config.clone(),
                ids: Default::default(),
            }),
            writer: Some(writer),
            last: Slot::sealed(reader.searcher(), Default::default()),
            reader,
            dir,
            dirty_since: None,
            applied: seq,
            committed: seq,
            staged: 0,
            open: None,
            uncertain: Default::default(),
            removed: Default::default(),
            removed_since: None,
        })
    }

    /// Document key: a hash of the length-prefixed vocabulary keys of s, p, o, g.
    fn doc_key(keys: [&[u8]; 4]) -> [u8; 16] {
        let mut h = sha2::Sha256::new();
        for k in keys {
            let mut len = Vec::new();
            crate::vocab::write_varint(&mut len, k.len() as u64);
            h.update(&len);
            h.update(k);
        }
        h.finalize()[..16].try_into().unwrap()
    }

    /// The lexical form and language tag of a string literal key; `None` for any other
    /// term (IRIs, typed literals, blank nodes, triple terms).
    fn string_literal(key: &[u8]) -> Option<(&str, Option<&str>)> {
        let rest = key.strip_prefix(b"\"")?;
        let sep = rest.iter().rposition(|&b| b == 0xFF)?;
        let lex = std::str::from_utf8(&rest[..sep]).ok()?;
        match &rest[sep + 1..] {
            [] => Some((lex, None)),
            [b'@', tag @ ..] => {
                let tag = std::str::from_utf8(tag).ok()?;
                // drop a base direction (`en--ltr`)
                Some((lex, Some(tag.split("--").next().unwrap_or(tag))))
            }
            _ => None,
        }
    }

    /// Key bytes of an id for documents (`_` + big-endian id for blank nodes).
    pub(super) fn term_key(snap: &Snapshot, id: Id) -> Option<Vec<u8>> {
        match id.tag() {
            Tag::BNode => {
                let mut k = vec![b'_'];
                k.extend_from_slice(&id.payload().to_be_bytes());
                Some(k)
            }
            Tag::Vocab | Tag::Delta => snap.key(id).map(|k| k.into_owned()),
            _ => None,
        }
    }

    pub(super) fn graph_name(snap: &Snapshot, g: Id) -> Option<String> {
        if g == Id::DEFAULT_GRAPH {
            return Some(DEFAULT_GRAPH_IRI.to_string());
        }
        match snap.term(g)? {
            oxrdf::Term::NamedNode(n) => Some(n.into_string()),
            oxrdf::Term::BlankNode(b) => Some(format!("_:{}", b.as_str())),
            _ => None,
        }
    }

    /// The quads of `snap` that can be documents under `cfg`: those of the configured
    /// predicates, or with every predicate those whose object is a literal.
    fn for_each_candidate(
        snap: &Snapshot,
        cfg: &TextConfig,
        mut f: impl FnMut(&[Id; 4]) -> Result<()>,
    ) -> Result<()> {
        use crate::index::Perm;
        match &cfg.predicates {
            PredicateSet::Only(ps) => {
                for p in ps {
                    let Some(pid) = snap.lookup_iri(p) else {
                        continue;
                    };
                    snap.scan(Perm::Pso, &[pid.0], |c| {
                        match c {
                            crate::store::Chunk::Block(b, s, e) => {
                                for i in s..e {
                                    f(&Perm::Pso.to_quad(&b.key(i)))?;
                                }
                            }
                            crate::store::Chunk::Row(k) => f(&Perm::Pso.to_quad(&k))?,
                        }
                        Ok(true)
                    })?;
                }
                Ok(())
            }
            PredicateSet::All => for_each_literal_quad(snap, f),
        }
    }

    /// The quads whose object is a literal of the vocabulary, the only ones that can be
    /// documents, in OPS order: the base vocabulary's keys that start with `"`, then
    /// every delta id (the delta vocabulary is not sorted, so its IRIs are read too).
    fn for_each_literal_quad(
        snap: &Snapshot,
        mut f: impl FnMut(&[Id; 4]) -> Result<()>,
    ) -> Result<()> {
        use crate::index::Perm;
        let (lo, hi) = snap.generation.vocab.prefix_range(b"\"");
        let mut ranges = Vec::with_capacity(2);
        if lo < hi {
            ranges.push((Id::vocab(lo).0, Id::vocab(hi - 1).0));
        }
        ranges.push((Id::delta(0).0, Id::local(0).0 - 1));
        for (a, b) in ranges {
            let (from, to) = ([a, 0, 0, 0], [b, u64::MAX, u64::MAX, u64::MAX]);
            snap.scan_between(Perm::Ops, from, to, |c| {
                match c {
                    crate::store::Chunk::Block(b, s, e) => {
                        for i in s..e {
                            f(&Perm::Ops.to_quad(&b.key(i)))?;
                        }
                    }
                    crate::store::Chunk::Row(k) => f(&Perm::Ops.to_quad(&k))?,
                }
                Ok(true)
            })?;
        }
        Ok(())
    }

    struct Doc {
        key: [u8; 16],
        /// see [`doc_hash`]
        hash: u64,
        doc: Option<TantivyDocument>,
    }

    /// Entries a [`Terms`] memo holds before it starts over (graphs can be many).
    const MEMO_MAX: usize = 1 << 16;

    /// What documents need of predicates and graphs, decoded once per batch: the IRI (or
    /// graph name), its key, and whether it is in scope.
    #[derive(Default)]
    struct Terms {
        preds: rustc_hash::FxHashMap<Id, Option<(String, Vec<u8>, bool)>>,
        graphs: rustc_hash::FxHashMap<Id, Option<(String, Vec<u8>, bool)>>,
        /// the last object and its key: a rebuild reads quads in object order
        last_o: Option<(Id, Option<Arc<[u8]>>)>,
    }

    impl Terms {
        /// The document for a quad (`doc: None` when the quad is out of scope: then only
        /// its key matters, for deletion).
        fn document(
            &mut self,
            snap: &Snapshot,
            f: &Fields,
            cfg: &TextConfig,
            q: &[Id; 4],
        ) -> Option<Doc> {
            self.document_or_key(snap, f, cfg, q, true)
        }

        /// [`document`](Self::document), or with `build` false only the key and hash: an
        /// in-scope quad then gets an empty document.
        fn document_or_key(
            &mut self,
            snap: &Snapshot,
            f: &Fields,
            cfg: &TextConfig,
            q: &[Id; 4],
            build: bool,
        ) -> Option<Doc> {
            if !matches!(q[2].tag(), Tag::Vocab | Tag::Delta) {
                return None;
            }
            let o = match &self.last_o {
                Some((id, o)) if *id == q[2] => o.clone()?,
                _ => {
                    let o: Option<Arc<[u8]>> = snap.key(q[2]).map(|k| Arc::from(&*k));
                    self.last_o = Some((q[2], o.clone()));
                    o?
                }
            };
            let (lex, lang) = string_literal(&o)?;
            if self.preds.len() >= MEMO_MAX {
                self.preds.clear();
            }
            if self.graphs.len() >= MEMO_MAX {
                self.graphs.clear();
            }
            let (p, pk, p_in) = self
                .preds
                .entry(q[1])
                .or_insert_with(|| match snap.term(q[1])? {
                    oxrdf::Term::NamedNode(n) => {
                        let p = n.into_string();
                        let pk = crate::id::iri_key(&p);
                        let p_in = cfg.predicates.contains(&p);
                        Some((p, pk, p_in))
                    }
                    _ => None,
                })
                .as_ref()?;
            let (g, gk, g_in) = self
                .graphs
                .entry(q[3])
                .or_insert_with(|| {
                    let gk = if q[3] == Id::DEFAULT_GRAPH {
                        Vec::new()
                    } else {
                        term_key(snap, q[3])?
                    };
                    let g = graph_name(snap, q[3])?;
                    let g_in = cfg.graphs.include.contains(&g) && !cfg.graphs.exclude.contains(&g);
                    Some((g, gk, g_in))
                })
                .as_ref()?;
            let s = term_key(snap, q[0])?;
            let key = doc_key([&s, pk, &o, gk]);
            let hash = doc_hash(&s, &o);
            if !(*p_in && *g_in) {
                return Some(Doc {
                    key,
                    hash,
                    doc: None,
                });
            }
            if !build {
                return Some(Doc {
                    key,
                    hash,
                    doc: Some(TantivyDocument::default()),
                });
            }
            let mut d = TantivyDocument::default();
            d.add_bytes(f.key, &key);
            d.add_bytes(f.s, &s);
            d.add_text(f.p, p);
            d.add_bytes(f.o, &o);
            d.add_text(f.g, g);
            if let Some(tag) = lang {
                let tag = tag.to_ascii_lowercase();
                if let Some((primary, _)) = tag.split_once('-') {
                    d.add_text(f.lang, primary);
                }
                d.add_text(f.lang, &tag);
            }
            let mut end = lex.len().min(cfg.max_text_bytes);
            while !lex.is_char_boundary(end) {
                end -= 1;
            }
            d.add_text(f.text, &lex[..end]);
            // a language with an analyzer: the stemmed text too
            if let Some((field, _)) = lang.and_then(|t| f.stemmed_for(&t.to_ascii_lowercase())) {
                d.add_text(field, &lex[..end]);
            }
            Some(Doc {
                key,
                hash,
                doc: Some(d),
            })
        }
    }

    impl TextIndex {
        /// Open the index of a store at its current state `snap` (`root` = `None` for an
        /// in-memory store). An index on disk with the same configuration is reused:
        /// verified first when it may hold unsynced writes, then caught up from `wal` when
        /// it is behind. `wal` holds the quads each commit of the WAL changed, oldest
        /// first, by commit. Otherwise the index is rebuilt. Returns the index and `snap`'s
        /// view.
        pub fn open(
            root: Option<&Path>,
            config: TextConfig,
            snap: &Snapshot,
            wal: &[(u64, Vec<[Id; 4]>)],
        ) -> Result<(TextIndex, Arc<TextView>)> {
            if let Some(r) = root {
                for stale in ["text.new", "text.old"] {
                    let _ = std::fs::remove_dir_all(r.join(stale));
                }
            }
            let hash = config_hash(&config);
            let ti = |live: Live, epoch: u64| {
                Arc::new(Inner {
                    root: root.map(Path::to_path_buf),
                    config: config.clone(),
                    live: Mutex::new(live),
                    epoch: AtomicU64::new(epoch),
                    stale: Mutex::new(None),
                    last_rebuild: Mutex::new(None),
                    fail_next_commit: Default::default(),
                    ticks: AtomicBool::new(true),
                    journal: Default::default(),
                    rebuilding: Default::default(),
                })
            };
            let reusable = root.filter(|r| r.join("text").exists()).and_then(|r| {
                let (index, fields, dir) = open_index(r, &config)
                    .inspect_err(|e| tracing::warn!("{e}; rebuilding"))
                    .ok()?;
                if dir.is_marked()
                    && let Err(e) = verify(&index)
                {
                    tracing::warn!("{e}; rebuilding");
                    return None;
                }
                let meta = index.load_metas().ok()?;
                let p: Payload = serde_json::from_str(meta.payload.as_deref()?).ok()?;
                if p.format != FORMAT || p.config != hash || p.seq > snap.commit {
                    return None;
                }
                // the quads changed since the index's commit: all in the WAL, or rebuild
                let mut touched = Vec::new();
                if p.seq < snap.commit {
                    if wal.first().is_none_or(|(first, _)| *first > p.seq + 1) {
                        return None;
                    }
                    for (_, qs) in wal.iter().filter(|(seq, _)| *seq > p.seq) {
                        touched.extend(qs.iter().map(|q| (*q, true)));
                    }
                }
                let live = live_of(index, fields, &config, Some(dir), p.seq).ok()?;
                Some((live, p.epoch, p.seq, touched))
            });
            if let Some((live, epoch, seq, touched)) = reusable {
                let t = ti(live, epoch);
                if seq < snap.commit {
                    tracing::info!(
                        "full-text index is at commit {seq}, the data at {}: catching up from the WAL",
                        snap.commit
                    );
                }
                // a verified or caught-up index is made durable before it is used
                match t
                    .apply(snap, &touched)
                    .and_then(|v| t.checkpoint().map(|()| v))
                {
                    Ok(view) => return Ok((TextIndex::start(t), view)),
                    Err(e) => tracing::warn!("full-text index: {e}; rebuilding"),
                }
            } else if root.is_some_and(|r| r.join("text").exists()) {
                tracing::info!("full-text index is missing, damaged or behind the WAL; rebuilding");
            }
            // an empty placeholder until the rebuild below swaps the real one in
            let (index, fields) = new_index(None, &config)?;
            let t = ti(live_of(index, fields, &config, None, 0)?, 0);
            let built = t.build(snap)?;
            let view = t.install(built, snap, &[])?;
            Ok((TextIndex::start(t), view))
        }

        /// Start the tick that seals batches and checkpoints the index about once a
        /// second. Without it (if the thread cannot start) searches, compaction and close
        /// still seal.
        fn start(inner: Arc<Inner>) -> TextIndex {
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            let weak = Arc::downgrade(&inner);
            let ticker = std::thread::Builder::new()
                .name("sparkles-text".into())
                .spawn(move || {
                    while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                        rx.recv_timeout(CHECKPOINT_AFTER)
                    {
                        match weak.upgrade() {
                            Some(inner) => inner.tick(),
                            None => break,
                        }
                    }
                })
                .inspect_err(|e| tracing::warn!("full-text commit tick: {e}"))
                .ok();
            TextIndex {
                inner,
                ticker: ticker.map(|h| (tx, h)),
            }
        }

        pub fn config(&self) -> &TextConfig {
            &self.inner.config
        }

        /// Whether updates are still applied (not stale after a failure).
        pub fn healthy(&self) -> bool {
            self.inner.healthy()
        }

        /// Apply a commit: `log` holds the transaction's effective inserts and deletes
        /// (WAL records) and `snap` the state after it (commit `snap.commit`). The
        /// documents are staged, not committed. Returns the new view, or `None` (the
        /// index is marked stale) on failure.
        pub fn apply_commit(
            &self,
            snap: &Snapshot,
            log: &[(u8, [Id; 4])],
            prev: Option<&Arc<TextView>>,
        ) -> Option<Arc<TextView>> {
            let inner = &self.inner;
            if let Some(j) = inner.journal.lock().as_mut() {
                j.extend(log.iter().map(|(_, q)| *q));
            }
            if !inner.healthy() {
                return prev.cloned();
            }
            // a quad the transaction inserted first was absent before it: no document
            // of it can exist yet
            let mut first = FxHashSet::default();
            let touched: Vec<([Id; 4], bool)> = log
                .iter()
                .filter(|(_, q)| first.insert(*q))
                .map(|(op, q)| (*q, *op != crate::store::WAL_INSERT))
                .collect();
            match inner.apply(snap, &touched) {
                Ok(v) => Some(v),
                Err(e) => {
                    inner.fail_locked(&mut inner.live.lock(), &e);
                    prev.cloned()
                }
            }
        }

        /// Make the on-disk index durable, with a payload naming the last applied commit:
        /// reopening it then needs neither verification nor WAL catch-up. Runs about once
        /// a second after writes, before compaction, and on drop.
        pub fn checkpoint(&self) -> Result<()> {
            self.inner.checkpoint()
        }

        /// Rebuild the whole index from `snap` and return its view. On disk, the new
        /// index is built in `text.new/` and swapped in.
        pub fn rebuild(&self, snap: &Snapshot) -> Result<Arc<TextView>> {
            let _one = self.inner.rebuilding.lock();
            let built = self.inner.build(snap)?;
            self.inner.install(built, snap, &[])
        }

        /// Wait for any other rebuild of the index to finish, then hold off others until
        /// the returned guard is dropped. Take it before the store's writer lock: a
        /// rebuild holds it while it waits for that lock.
        pub fn lock_rebuild(&self) -> RebuildGuard<'_> {
            RebuildGuard {
                inner: &self.inner,
                _guard: self.inner.rebuilding.lock(),
            }
        }

        /// [`rebuild`](Self::rebuild), unless another rebuild runs (`None`): for callers
        /// that hold the store's writer lock.
        pub fn try_rebuild(&self, snap: &Snapshot) -> Result<Option<Arc<TextView>>> {
            let Some(_one) = self.inner.rebuilding.try_lock() else {
                return Ok(None);
            };
            let built = self.inner.build(snap)?;
            self.inner.install(built, snap, &[]).map(Some)
        }

        /// The first step of an online rebuild, under the store's writer lock: from now
        /// on the quads of each commit are kept, for [`install`](Self::install).
        pub fn start_journal(&self, _guard: &RebuildGuard<'_>) {
            *self.inner.journal.lock() = Some(Vec::new());
        }

        /// Build an index from `snap`, while writes go on and the current index serves
        /// searches.
        pub fn build(&self, _guard: &RebuildGuard<'_>, snap: &Snapshot) -> Result<Built> {
            self.inner.build(snap)
        }

        /// Finish an online rebuild under the store's writer lock: bring the built index
        /// from its snapshot to `head` by the quads the commits since then changed, both
        /// of one generation, and put it in place of the current index. Returns `head`'s
        /// view.
        pub fn install(
            &self,
            guard: &RebuildGuard<'_>,
            built: Built,
            head: &Snapshot,
        ) -> Result<Arc<TextView>> {
            let journal = guard.take();
            self.inner.install(built, head, &journal)
        }

        /// Rebuild from `snap` with the guard of a rebuild already held.
        pub fn rebuild_held(
            &self,
            _guard: &RebuildGuard<'_>,
            snap: &Snapshot,
        ) -> Result<Arc<TextView>> {
            let built = self.inner.build(snap)?;
            self.inner.install(built, snap, &[])
        }

        /// After a bulk commit from `old` to `new` (states of different generations):
        /// bring the index to `new` by the documents that differ, and return `new`'s
        /// view. `None` when that would not pay: the index does not reflect `old`, or
        /// the commit adds too many documents for one writer thread, and a rebuild is
        /// then cheaper. Nothing has changed in that case.
        pub fn apply_bulk(&self, old: &Snapshot, new: &Snapshot) -> Result<Option<Arc<TextView>>> {
            if !self.inner.healthy() {
                return Ok(None);
            }
            self.inner.apply_bulk(old, new)
        }

        pub fn status(&self, view: Option<&TextView>, store_seq: u64) -> TextStatus {
            self.inner.status(view, store_seq)
        }

        /// Test hook: make the next update fail.
        #[doc(hidden)]
        pub fn fail_next_commit(&self) {
            self.inner.fail_next_commit.store(true, Ordering::SeqCst);
        }

        /// Test hook: pause (or resume) the tick, so staged documents stay uncommitted
        /// until a search, compaction or close.
        #[doc(hidden)]
        pub fn set_ticks(&self, on: bool) {
            self.inner.ticks.store(on, Ordering::SeqCst);
        }
    }

    impl Inner {
        /// A view of commit `seq` in `slot`.
        fn view_in(self: &Arc<Self>, live: &Live, seq: u64, slot: Arc<Slot>) -> Arc<TextView> {
            Arc::new(TextView {
                seq,
                epoch: self.epoch.load(Ordering::SeqCst),
                slot,
                index: live.shared.clone(),
                owner: Arc::downgrade(self),
            })
        }

        fn payload(&self, seq: u64) -> String {
            self.payload_with(seq, self.epoch.load(Ordering::SeqCst))
        }

        fn payload_with(&self, seq: u64, epoch: u64) -> String {
            serde_json::to_string(&Payload {
                format: FORMAT,
                seq,
                epoch,
                config: config_hash(&self.config),
            })
            .unwrap()
        }

        fn healthy(&self) -> bool {
            self.stale.lock().is_none()
        }

        /// Mark the index stale after a failure: staged operations are dropped, and the
        /// views of the open batch cannot search.
        fn fail_locked(&self, live: &mut Live, e: &Error) {
            tracing::error!("{e}; the full-text index is stale until it is rebuilt");
            if let Ok(w) = live.writer() {
                let _ = w.rollback();
            }
            if let Some(slot) = live.open.take() {
                let _ = slot.0.set(None);
            }
            live.staged = 0;
            live.removed.clear();
            live.removed_since = None;
            *self.stale.lock() = Some(e.to_string());
        }

        /// Bring the index to `snap` given the quads changed since its commit, each with
        /// whether a document of it may exist, and return the view of `snap`. A present
        /// quad's document is (re-)added and a removed one's kept until the batch is
        /// sealed, so the result depends only on the final state. Nothing is committed.
        fn apply(
            self: &Arc<Self>,
            snap: &Snapshot,
            touched: &[([Id; 4], bool)],
        ) -> Result<Arc<TextView>> {
            let mut live = self.live.lock();
            let live = &mut *live;
            let shared = live.shared.clone();
            let fields = &shared.fields;
            let mut terms = Terms::default();
            let mut changed = false;
            let mut seen = FxHashSet::default();
            for (q, indexed) in touched {
                if !seen.insert(*q) {
                    continue;
                }
                // out of scope: no document can exist (the configuration is the index's)
                let Some(Doc {
                    key,
                    hash,
                    doc: Some(doc),
                }) = terms.document(snap, fields, &self.config, q)
                else {
                    continue;
                };
                let present = snap.contains(q)?;
                if !indexed && !present {
                    // inserted and removed again by the same transaction
                    continue;
                }
                changed = true;
                live.uncertain.insert(hash);
                if present {
                    // deleting is not free even when nothing matches (every commit with
                    // deletes opens each segment to apply them): a fresh quad has no
                    // document unless one was kept for its removal earlier in the batch
                    if live.removed.remove(&key).is_some() || *indexed {
                        live.writer()?
                            .delete_term(Term::from_field_bytes(fields.key, &key));
                        live.staged += 1;
                    }
                    live.writer()?.add_document(doc).map_err(text_err)?;
                    live.staged += 1;
                } else {
                    live.removed.insert(key, hash);
                    // reopening catches up from the first commit that may have
                    // removed it
                    let since = live.applied + 1;
                    live.removed_since.get_or_insert(since);
                }
            }
            if changed && self.fail_next_commit.swap(false, Ordering::SeqCst) {
                return Err(text_err("injected failure"));
            }
            live.applied = snap.commit;
            let slot = if changed || live.open.is_some() {
                live.open.get_or_insert_with(Default::default).clone()
            } else {
                live.last.clone()
            };
            // a large batch is sealed right away: the search that would seal it pays
            // about as much as the commit costs
            if live.staged + live.removed.len() >= STAGED_MAX {
                self.seal_locked(live)?;
            }
            Ok(self.view_in(live, snap.commit, slot))
        }

        /// See [`TextIndex::apply_bulk`]. The documents of `old` are known by their keys,
        /// and `new`'s candidates are compared against them: a document of `new` whose
        /// key is not among them is added, and the keys of `old` that `new` does not
        /// have are deleted. The change is committed and synced at once, because the
        /// write-ahead log of the new generation does not hold it.
        fn apply_bulk(
            self: &Arc<Self>,
            old: &Snapshot,
            new: &Snapshot,
        ) -> Result<Option<Arc<TextView>>> {
            let t0 = Instant::now();
            let mut live = self.live.lock();
            let live = &mut *live;
            self.settle_locked(live)?;
            if live.applied != old.commit {
                return Ok(None);
            }
            let fields = live.shared.fields.clone();
            let cfg = &self.config;
            let mut keys: Vec<[u8; 16]> = Vec::new();
            let mut terms = Terms::default();
            for_each_candidate(old, cfg, |q| {
                if let Some(Doc {
                    key, doc: Some(_), ..
                }) = terms.document_or_key(old, &fields, cfg, q, false)
                {
                    keys.push(key);
                }
                Ok(())
            })?;
            keys.sort_unstable();
            keys.dedup();
            // past this many additions one writer thread is slower than a rebuild
            let most = (keys.len() / BULK_SHARE).max(BULK_MIN);
            let mut kept = vec![false; keys.len()];
            let mut terms = Terms::default();
            let (mut added, mut too_many) = (0usize, false);
            let writer = live.writer()?;
            for_each_candidate(new, cfg, |q| {
                if too_many {
                    return Ok(());
                }
                let Some(Doc {
                    key, doc: Some(_), ..
                }) = terms.document_or_key(new, &fields, cfg, q, false)
                else {
                    return Ok(());
                };
                match keys.binary_search(&key) {
                    Ok(i) => kept[i] = true,
                    Err(_) => {
                        if added == most {
                            too_many = true;
                            return Ok(());
                        }
                        if let Some(Doc { doc: Some(d), .. }) = terms.document(new, &fields, cfg, q)
                        {
                            writer.add_document(d).map_err(text_err)?;
                            added += 1;
                        }
                    }
                }
                Ok(())
            })?;
            if too_many {
                writer.rollback().map_err(text_err)?;
                return Ok(None);
            }
            let mut deleted = 0usize;
            for (key, _) in keys.iter().zip(&kept).filter(|(_, k)| !**k) {
                writer.delete_term(Term::from_field_bytes(fields.key, key));
                deleted += 1;
            }
            live.applied = new.commit;
            live.staged = added + deleted;
            self.commit_locked(live)?;
            live.uncertain.clear();
            live.last = Slot::sealed(live.reader.searcher(), Default::default());
            self.checkpoint_locked(live)?;
            tracing::info!(
                "full-text index after a bulk commit: {added} documents added and {deleted} removed in {:?}",
                t0.elapsed()
            );
            Ok(Some(self.view_in(live, new.commit, live.last.clone())))
        }

        /// Commit the writer and reload the reader. The payload names the applied commit,
        /// or the one before the first removal whose document is still kept (reopening
        /// catches up from there).
        fn commit_locked(&self, live: &mut Live) -> Result<()> {
            let seq = live.removed_since.map_or(live.applied, |s| s - 1);
            let payload = self.payload(seq);
            let mut prepared = live.writer()?.prepare_commit().map_err(text_err)?;
            prepared.set_payload(&payload);
            prepared.commit().map_err(text_err)?;
            live.reader.reload().map_err(text_err)?;
            live.committed = seq;
            live.staged = 0;
            if live.dir.is_some() && live.dirty_since.is_none() {
                live.dirty_since = Some(Instant::now());
            }
            Ok(())
        }

        /// Seal the open batch: commit what is staged (or reuse the searcher when only
        /// removals happened) and give the batch's views their searcher. The documents
        /// of removed quads are then deleted, with the next commit.
        fn seal_locked(&self, live: &mut Live) -> Result<()> {
            let Some(slot) = live.open.clone() else {
                return Ok(());
            };
            let committed = live.staged > 0;
            if committed {
                self.commit_locked(live)?;
            }
            let _ = slot.0.set(Some(Resolved {
                searcher: live.reader.searcher(),
                uncertain: Arc::new(live.uncertain.clone()),
            }));
            live.open = None;
            live.last = slot;
            let removed = std::mem::take(&mut live.removed);
            if committed {
                // what later views may still disagree with: the kept documents
                live.uncertain = removed.values().copied().collect();
            }
            let field = live.shared.fields.key;
            for key in removed.keys() {
                live.writer()?
                    .delete_term(Term::from_field_bytes(field, key));
                live.staged += 1;
            }
            live.removed_since = None;
            Ok(())
        }

        /// Seal, then commit the deletions of removed quads' documents: the searcher
        /// then holds exactly the applied state.
        fn settle_locked(&self, live: &mut Live) -> Result<()> {
            self.seal_locked(live)?;
            if live.staged > 0 {
                self.commit_locked(live)?;
                live.uncertain.clear();
                live.last = Slot::sealed(live.reader.searcher(), Default::default());
            }
            Ok(())
        }

        /// Seal the batch of `slot` for a search (read your writes).
        fn seal_for(&self, slot: &Arc<Slot>) {
            let mut live = self.live.lock();
            if slot.0.get().is_some() {
                return;
            }
            if live.open.as_ref().is_some_and(|o| Arc::ptr_eq(o, slot)) {
                if let Err(e) = self.seal_locked(&mut live) {
                    self.fail_locked(&mut live, &e);
                }
            } else {
                // not open any more and never sealed: abandoned by a failure
                let _ = slot.0.set(None);
            }
        }

        /// The tick: seal and settle what the last second staged, and checkpoint an
        /// index that has held unsynced commits for a second.
        fn tick(&self) {
            if !self.ticks.load(Ordering::SeqCst) || !self.healthy() {
                return;
            }
            let mut live = self.live.lock();
            let r = self.settle_locked(&mut live).and_then(|()| {
                if live
                    .dirty_since
                    .is_some_and(|t| t.elapsed() >= CHECKPOINT_AFTER)
                {
                    self.checkpoint_locked(&mut live)
                } else {
                    Ok(())
                }
            });
            if let Err(e) = r {
                self.fail_locked(&mut live, &e);
            }
        }

        fn checkpoint_locked(&self, live: &mut Live) -> Result<()> {
            self.settle_locked(live)?;
            let Some(dir) = live.dir.clone() else {
                return Ok(());
            };
            if live.committed != live.applied {
                self.commit_locked(live)?;
            }
            dir.checkpoint()?;
            live.dirty_since = None;
            Ok(())
        }

        fn checkpoint(&self) -> Result<()> {
            if !self.healthy() {
                return Ok(());
            }
            let mut live = self.live.lock();
            self.checkpoint_locked(&mut live)
        }

        /// Build the index of `snap` in `text.new/` (in memory for an in-memory store),
        /// with several writer threads, and commit it. The current index is not touched.
        fn build(self: &Arc<Self>, snap: &Snapshot) -> Result<Built> {
            let t0 = std::time::Instant::now();
            let new_dir = self.root.as_ref().map(|r| r.join("text.new"));
            let (index, fields) = new_index(new_dir.as_deref(), &self.config)?;
            let threads = std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .min(8);
            let mut docs = 0u64;
            let epoch = self.epoch.load(Ordering::SeqCst) + 1;
            {
                let mut writer: IndexWriter<TantivyDocument> = index
                    .writer_with_num_threads(threads, threads * (64 << 20))
                    .map_err(text_err)?;
                let mut terms = Terms::default();
                let mut add = |q: &[Id; 4]| -> Result<()> {
                    if let Some(Doc { doc: Some(d), .. }) =
                        terms.document(snap, &fields, &self.config, q)
                    {
                        writer.add_document(d).map_err(text_err)?;
                        docs += 1;
                    }
                    Ok(())
                };
                for_each_candidate(snap, &self.config, &mut add)?;
                let payload = self.payload_with(snap.commit, epoch);
                let mut prepared = writer.prepare_commit().map_err(text_err)?;
                prepared.set_payload(&payload);
                prepared.commit().map_err(text_err)?;
                writer.wait_merging_threads().map_err(text_err)?;
            }
            Ok(Built {
                index,
                fields,
                dir: new_dir,
                epoch,
                docs,
                started: t0,
            })
        }

        /// Put a built index in place of the current one, after bringing it from its
        /// snapshot to `head` by the quads in `journal` (ids of `head`'s generation):
        /// each document of such a quad is replaced, or removed when `head` does not
        /// have the quad.
        fn install(
            self: &Arc<Self>,
            built: Built,
            head: &Snapshot,
            journal: &[[Id; 4]],
        ) -> Result<Arc<TextView>> {
            let Built {
                index,
                fields,
                dir: new_dir,
                epoch,
                docs,
                started: t0,
            } = built;
            if !journal.is_empty() {
                let mut writer: IndexWriter<TantivyDocument> = index
                    .writer_with_num_threads(1, 32 << 20)
                    .map_err(text_err)?;
                // writes wait for this step: the index's own writer merges later
                writer.set_merge_policy(Box::new(tantivy::indexer::NoMergePolicy));
                let mut terms = Terms::default();
                let mut seen = FxHashSet::default();
                for q in journal {
                    if !seen.insert(*q) {
                        continue;
                    }
                    let Some(Doc {
                        key,
                        doc: Some(doc),
                        ..
                    }) = terms.document(head, &fields, &self.config, q)
                    else {
                        continue;
                    };
                    writer.delete_term(Term::from_field_bytes(fields.key, &key));
                    if head.contains(q)? {
                        writer.add_document(doc).map_err(text_err)?;
                    }
                }
                let payload = self.payload_with(head.commit, epoch);
                let mut prepared = writer.prepare_commit().map_err(text_err)?;
                prepared.set_payload(&payload);
                prepared.commit().map_err(text_err)?;
                writer.wait_merging_threads().map_err(text_err)?;
            }
            let snap = head;
            self.epoch.store(epoch, Ordering::SeqCst);
            let mut live = self.live.lock();
            // the views of the old index's open batch get its searcher
            if let Err(e) = self.seal_locked(&mut live) {
                tracing::warn!("full-text index: {e}; replaced by the rebuild");
                if let Some(slot) = live.open.take() {
                    let _ = slot.0.set(None);
                }
            }
            let old = match (&self.root, new_dir) {
                (Some(root), Some(new_dir)) => {
                    // swap directories, then reopen the index in its final place
                    let cur = root.join("text");
                    let old = root.join("text.old");
                    drop(index);
                    crate::store::sync_dir(&new_dir)?;
                    if cur.exists() {
                        std::fs::rename(&cur, &old)?;
                    }
                    std::fs::rename(&new_dir, &cur)?;
                    crate::store::sync_dir(root)?;
                    // the new index was synced by its build: a marker left by the old one
                    // is stale
                    match std::fs::remove_file(marker(root)) {
                        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                        _ => {}
                    }
                    crate::store::sync_dir(root)?;
                    let (index, fields, dir) = open_index(root, &self.config)?;
                    *live = live_of(index, fields, &self.config, Some(dir), snap.commit)?;
                    Some(old)
                }
                _ => {
                    *live = live_of(index, fields, &self.config, None, snap.commit)?;
                    None
                }
            };
            let view = self.view_in(&live, snap.commit, live.last.clone());
            drop(live);
            if let Some(old) = old {
                let _ = std::fs::remove_dir_all(old);
            }
            *self.stale.lock() = None;
            *self.last_rebuild.lock() = Some(RebuildInfo {
                at: crate::commit::rfc3339_ms(crate::commit::now_ms()),
                ms: t0.elapsed().as_secs_f64() * 1000.0,
                docs,
            });
            tracing::info!(
                "full-text index rebuilt: {docs} documents in {:?}",
                t0.elapsed()
            );
            Ok(view)
        }

        fn status(&self, view: Option<&TextView>, store_seq: u64) -> TextStatus {
            let mut live = self.live.lock();
            // counts without staged or kept documents
            if self.healthy()
                && let Err(e) = self.settle_locked(&mut live)
            {
                self.fail_locked(&mut live, &e);
            }
            let searcher = live.reader.searcher();
            let stale = self.stale.lock().clone();
            let seq = view.map_or(0, |v| v.seq);
            let state = if stale.is_some() || seq != store_seq {
                "stale"
            } else {
                "ready"
            };
            TextStatus {
                enabled: true,
                state: state.into(),
                docs: searcher.num_docs(),
                seq,
                store_seq,
                epoch: self.epoch.load(Ordering::SeqCst),
                disk_bytes: self
                    .root
                    .as_ref()
                    .map_or(0, |r| crate::store::dir_size(&r.join("text"))),
                segments: searcher.segment_readers().len(),
                config: self.config.clone(),
                format_version: FORMAT,
                last_rebuild: self.last_rebuild.lock().clone(),
                message: stale,
            }
        }

        /// Seal and commit, let running merges finish (they write metadata too), then
        /// sync.
        fn close(&self) -> Result<()> {
            if !self.healthy() {
                return Ok(());
            }
            let mut live = self.live.lock();
            self.settle_locked(&mut live)?;
            if live.committed != live.applied {
                self.commit_locked(&mut live)?;
            }
            if let Some(w) = live.writer.take() {
                w.wait_merging_threads().map_err(text_err)?;
            }
            match live.dir.clone() {
                Some(dir) => Ok(dir.checkpoint()?),
                None => Ok(()),
            }
        }
    }

    impl Drop for TextIndex {
        fn drop(&mut self) {
            if let Some((tx, ticker)) = self.ticker.take() {
                drop(tx);
                let _ = ticker.join();
            }
            if let Err(e) = self.inner.close() {
                tracing::warn!("full-text index checkpoint on close: {e}");
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn tokens(a: Analyzer, text: &str) -> Vec<(usize, String)> {
            let mut an = language_analyzer(a);
            let mut st = an.token_stream(text);
            let mut out = Vec::new();
            while let Some(t) = st.next() {
                out.push((t.position, t.text.clone()));
            }
            out
        }

        #[test]
        fn language_analyzers_stem_and_drop_stop_words() {
            let words =
                |a, t| -> Vec<String> { tokens(a, t).into_iter().map(|(_, w)| w).collect() };
            assert_eq!(
                words(Analyzer::English, "The theories of running foxes"),
                ["theori", "run", "fox"]
            );
            // stop words leave gaps in the positions, as in Lucene
            assert_eq!(
                tokens(Analyzer::English, "Ada and the fox"),
                [(0, "ada".to_string()), (3, "fox".to_string())]
            );
            assert_eq!(words(Analyzer::French, "Les chevaux"), ["cheval"]);
            // the German stemmer removes the umlaut itself
            assert_eq!(words(Analyzer::German, "die Häuser"), ["haus"]);
            // stems are not folded: Swedish keeps städer (städ) apart from stad
            assert_eq!(words(Analyzer::Swedish, "städer stad"), ["städ", "stad"]);
            assert_eq!(words(Analyzer::Spanish, "las canciones"), ["cancion"]);
            // a language without a stop word list in Tantivy still stems
            assert_eq!(words(Analyzer::Turkish, "kitaplar"), ["kitap"]);
        }
    }
}
