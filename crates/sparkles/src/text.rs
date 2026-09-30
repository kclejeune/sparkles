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
//! configuration; `<root>/text/` the Tantivy index. Commits write it without fsync (see
//! `lazydir`): the WAL is the durable record, the index is checkpointed about once a
//! second, and on open an index that may hold unsynced data is verified, then caught up
//! from the WAL. It is rebuilt from RDF only when it is missing, damaged, or behind the
//! WAL.

use crate::error::{Error, Result};

#[cfg(feature = "text")]
mod lazydir;
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
        }
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
    #[cfg(feature = "text")]
    pub(crate) searcher: tantivy::Searcher,
    #[cfg(feature = "text")]
    pub(crate) index: std::sync::Arc<imp::Shared>,
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
pub(crate) use imp::read_config as imp_read_config;
#[cfg(feature = "text")]
pub use imp::{TextIndex, search};

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

#[cfg(feature = "text")]
mod imp {
    use super::lazydir::LazySyncDir;
    use super::*;
    use crate::id::{Id, Tag};
    use crate::sparql::ctx::Ctx;
    use crate::sparql::plan::{GraphFilter, PathEnd, TextSpec};
    use crate::sparql::table::{Table, VarId};
    use crate::store::Snapshot;
    use parking_lot::Mutex;
    use sha2::Digest;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    use tantivy::collector::TopDocs;
    use tantivy::query::{
        BooleanQuery, ConstScoreQuery, Occur, Query, QueryParser, TermQuery, TermSetQuery,
    };
    use tantivy::schema::{
        BytesOptions, Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing,
        TextOptions, Value,
    };
    use tantivy::tokenizer::{
        AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer,
    };
    use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

    const FORMAT: u32 = 1;
    const TOKENIZER: &str = "sparkles_standard";

    #[derive(Clone, Copy)]
    pub(crate) struct Fields {
        key: Field,
        s: Field,
        p: Field,
        o: Field,
        g: Field,
        lang: Field,
        text: Field,
    }

    fn schema() -> (Schema, Fields) {
        let mut b = Schema::builder();
        let bytes_indexed = BytesOptions::default().set_indexed();
        let text = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );
        let fields = Fields {
            key: b.add_bytes_field("key", bytes_indexed.clone()),
            s: b.add_bytes_field("s", bytes_indexed.set_stored()),
            p: b.add_text_field("p", STRING | STORED),
            o: b.add_bytes_field("o", BytesOptions::default().set_stored()),
            g: b.add_text_field("g", STRING | STORED),
            lang: b.add_text_field("lang", STRING),
            text: b.add_text_field("text", text),
        };
        (b.build(), fields)
    }

    fn register_tokenizer(index: &Index) {
        index.tokenizers().register(
            TOKENIZER,
            TextAnalyzer::builder(SimpleTokenizer::default())
                .filter(RemoveLongFilter::limit(40))
                .filter(LowerCaser)
                .filter(AsciiFoldingFilter)
                .build(),
        );
    }

    /// What every view of one index generation shares.
    pub(crate) struct Shared {
        pub(crate) index: Index,
        pub(crate) fields: Fields,
        pub(crate) config: TextConfig,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Payload {
        format: u32,
        seq: u64,
        epoch: u64,
        config: String,
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
        /// the commit the index reflects, and the one its on-disk payload names (behind
        /// after commits that changed no document)
        applied: u64,
        committed: u64,
    }

    /// A dataset's full-text index.
    pub struct TextIndex {
        root: Option<PathBuf>,
        config: TextConfig,
        live: Mutex<Live>,
        epoch: AtomicU64,
        /// set when an update's text maintenance failed: queries get 503 until a rebuild
        stale: Mutex<Option<String>>,
        last_rebuild: Mutex<Option<RebuildInfo>>,
        #[doc(hidden)]
        pub fail_next_commit: std::sync::atomic::AtomicBool,
    }

    fn config_hash(c: &TextConfig) -> String {
        let bytes = serde_json::to_vec(c).unwrap();
        let d = sha2::Sha256::digest(&bytes);
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn text_err(e: impl std::fmt::Display) -> Error {
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
    fn new_index(dir: Option<&Path>) -> Result<(Index, Fields)> {
        let (schema, fields) = schema();
        let index = match dir {
            None => Index::create_in_ram(schema),
            Some(d) => {
                if d.exists() {
                    std::fs::remove_dir_all(d)?;
                }
                std::fs::create_dir_all(d)?;
                Index::create_in_dir(d, schema).map_err(text_err)?
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
    fn open_index(root: &Path) -> Result<(Index, Fields, LazySyncDir)> {
        let dir = LazySyncDir::open(&root.join("text"), marker(root))?;
        let index = Index::open(dir.clone()).map_err(text_err)?;
        register_tokenizer(&index);
        let (_, fields) = schema();
        if index.schema() != schema().0 {
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
            if index.schema() != schema().0 {
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

    /// How long the index may hold unsynced commits before a checkpoint.
    const CHECKPOINT_AFTER: Duration = Duration::from_secs(1);

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
                index,
                fields,
                config: config.clone(),
            }),
            writer: Some(writer),
            reader,
            dir,
            dirty_since: None,
            applied: seq,
            committed: seq,
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
    fn term_key(snap: &Snapshot, id: Id) -> Option<Vec<u8>> {
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

    fn graph_name(snap: &Snapshot, g: Id) -> Option<String> {
        if g == Id::DEFAULT_GRAPH {
            return Some(DEFAULT_GRAPH_IRI.to_string());
        }
        match snap.term(g)? {
            oxrdf::Term::NamedNode(n) => Some(n.into_string()),
            oxrdf::Term::BlankNode(b) => Some(format!("_:{}", b.as_str())),
            _ => None,
        }
    }

    struct Doc {
        key: [u8; 16],
        doc: Option<TantivyDocument>,
    }

    /// The document for a quad (`doc: None` when the quad is out of scope: then only its
    /// key matters, for deletion).
    fn document(snap: &Snapshot, f: &Fields, cfg: &TextConfig, q: &[Id; 4]) -> Option<Doc> {
        if !matches!(q[2].tag(), Tag::Vocab | Tag::Delta) {
            return None;
        }
        let o = snap.key(q[2])?;
        let (lex, lang) = string_literal(&o)?;
        let p = match snap.term(q[1])? {
            oxrdf::Term::NamedNode(n) => n.into_string(),
            _ => return None,
        };
        let s = term_key(snap, q[0])?;
        let pk = crate::id::iri_key(&p);
        let gk = if q[3] == Id::DEFAULT_GRAPH {
            Vec::new()
        } else {
            term_key(snap, q[3])?
        };
        let key = doc_key([&s, &pk, &o, &gk]);
        let g = graph_name(snap, q[3])?;
        let in_scope = cfg.predicates.contains(&p)
            && cfg.graphs.include.contains(&g)
            && !cfg.graphs.exclude.contains(&g);
        if !in_scope {
            return Some(Doc { key, doc: None });
        }
        let mut d = TantivyDocument::default();
        d.add_bytes(f.key, &key);
        d.add_bytes(f.s, &s);
        d.add_text(f.p, &p);
        d.add_bytes(f.o, &o);
        d.add_text(f.g, &g);
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
        Some(Doc { key, doc: Some(d) })
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
            let ti = |live: Live, epoch: u64| TextIndex {
                root: root.map(Path::to_path_buf),
                config: config.clone(),
                live: Mutex::new(live),
                epoch: AtomicU64::new(epoch),
                stale: Mutex::new(None),
                last_rebuild: Mutex::new(None),
                fail_next_commit: Default::default(),
            };
            let reusable = root.filter(|r| r.join("text").exists()).and_then(|r| {
                let (index, fields, dir) = open_index(r)
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
                        touched.extend_from_slice(qs);
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
                    Ok(view) => return Ok((t, view)),
                    Err(e) => tracing::warn!("full-text index: {e}; rebuilding"),
                }
            } else if root.is_some_and(|r| r.join("text").exists()) {
                tracing::info!("full-text index is missing, damaged or behind the WAL; rebuilding");
            }
            // an empty placeholder until the rebuild below swaps the real one in
            let (index, fields) = new_index(None)?;
            let t = ti(live_of(index, fields, &config, None, 0)?, 0);
            let view = t.rebuild(snap)?;
            Ok((t, view))
        }

        pub fn config(&self) -> &TextConfig {
            &self.config
        }

        fn view(&self, seq: u64) -> Result<Arc<TextView>> {
            let live = self.live.lock();
            Ok(Arc::new(TextView {
                seq,
                epoch: self.epoch.load(Ordering::SeqCst),
                searcher: live.reader.searcher(),
                index: live.shared.clone(),
            }))
        }

        fn payload(&self, seq: u64) -> String {
            serde_json::to_string(&Payload {
                format: FORMAT,
                seq,
                epoch: self.epoch.load(Ordering::SeqCst),
                config: config_hash(&self.config),
            })
            .unwrap()
        }

        /// Whether updates are still applied (not stale after a failure).
        pub fn healthy(&self) -> bool {
            self.stale.lock().is_none()
        }

        /// Apply a commit: `touched` are the quads the transaction changed and `snap` the
        /// state after it (commit `snap.commit`). Returns the new view, or `None` (the
        /// index is marked stale) on failure.
        pub fn apply_commit(
            &self,
            snap: &Snapshot,
            touched: &[[Id; 4]],
            prev: Option<&Arc<TextView>>,
        ) -> Option<Arc<TextView>> {
            if !self.healthy() {
                return prev.cloned();
            }
            match self.apply(snap, touched) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::error!("{e}; the full-text index is stale until it is rebuilt");
                    if let Ok(w) = self.live.lock().writer() {
                        let _ = w.rollback();
                    }
                    *self.stale.lock() = Some(e.to_string());
                    prev.cloned()
                }
            }
        }

        /// Bring the index to `snap` given the quads changed since its commit. Each touched
        /// quad in scope is deleted and re-added if still present, so the result depends
        /// only on the final state. The Tantivy commit is not synced: see `lazydir`.
        fn apply(&self, snap: &Snapshot, touched: &[[Id; 4]]) -> Result<Arc<TextView>> {
            let mut live = self.live.lock();
            let fields = live.shared.fields;
            let mut changed = false;
            let mut seen = rustc_hash::FxHashSet::default();
            for q in touched {
                if !seen.insert(*q) {
                    continue;
                }
                let Some(d) = document(snap, &fields, &self.config, q) else {
                    continue;
                };
                live.writer()?
                    .delete_term(Term::from_field_bytes(fields.key, &d.key));
                changed = true;
                if let Some(doc) = d.doc
                    && snap.contains(q)?
                {
                    live.writer()?.add_document(doc).map_err(text_err)?;
                }
            }
            live.applied = snap.commit;
            if changed {
                if self.fail_next_commit.swap(false, Ordering::SeqCst) {
                    return Err(text_err("injected failure"));
                }
                self.commit_locked(&mut live)?;
            }
            if live
                .dirty_since
                .is_some_and(|t| t.elapsed() >= CHECKPOINT_AFTER)
            {
                self.checkpoint_locked(&mut live)?;
            }
            drop(live);
            self.view(snap.commit)
        }

        /// Commit the writer (payload: the applied commit) and reload the reader.
        fn commit_locked(&self, live: &mut Live) -> Result<()> {
            let payload = self.payload(live.applied);
            let mut prepared = live.writer()?.prepare_commit().map_err(text_err)?;
            prepared.set_payload(&payload);
            prepared.commit().map_err(text_err)?;
            live.reader.reload().map_err(text_err)?;
            live.committed = live.applied;
            if live.dir.is_some() && live.dirty_since.is_none() {
                live.dirty_since = Some(Instant::now());
            }
            Ok(())
        }

        fn checkpoint_locked(&self, live: &mut Live) -> Result<()> {
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

        /// Make the on-disk index durable, with a payload naming the last applied commit:
        /// reopening it then needs neither verification nor WAL catch-up. Runs about once
        /// a second during writes, before compaction, and on drop.
        pub fn checkpoint(&self) -> Result<()> {
            if !self.healthy() {
                return Ok(());
            }
            let mut live = self.live.lock();
            self.checkpoint_locked(&mut live)
        }

        /// Rebuild the whole index from `snap` and return its view. On disk, the new
        /// index is built in `text.new/` and swapped in.
        pub fn rebuild(&self, snap: &Snapshot) -> Result<Arc<TextView>> {
            let t0 = std::time::Instant::now();
            let new_dir = self.root.as_ref().map(|r| r.join("text.new"));
            let (index, fields) = new_index(new_dir.as_deref())?;
            let threads = std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .min(8);
            let mut docs = 0u64;
            {
                let mut writer: IndexWriter<TantivyDocument> = index
                    .writer_with_num_threads(threads, threads * (64 << 20))
                    .map_err(text_err)?;
                let mut add = |q: &[Id; 4]| -> Result<()> {
                    if let Some(Doc { doc: Some(d), .. }) = document(snap, &fields, &self.config, q)
                    {
                        writer.add_document(d).map_err(text_err)?;
                        docs += 1;
                    }
                    Ok(())
                };
                match &self.config.predicates {
                    PredicateSet::Only(ps) => {
                        use crate::index::Perm;
                        for p in ps {
                            let Some(pid) = snap.lookup_iri(p) else {
                                continue;
                            };
                            snap.scan(Perm::Pso, &[pid.0], |c| {
                                match c {
                                    crate::store::Chunk::Block(b, s, e) => {
                                        for i in s..e {
                                            add(&Perm::Pso.to_quad(&b.key(i)))?;
                                        }
                                    }
                                    crate::store::Chunk::Row(k) => add(&Perm::Pso.to_quad(&k))?,
                                }
                                Ok(true)
                            })?;
                        }
                    }
                    PredicateSet::All => snap.for_each_quad(&mut add)?,
                }
                self.epoch.fetch_add(1, Ordering::SeqCst);
                let payload = self.payload(snap.commit);
                let mut prepared = writer.prepare_commit().map_err(text_err)?;
                prepared.set_payload(&payload);
                prepared.commit().map_err(text_err)?;
                writer.wait_merging_threads().map_err(text_err)?;
            }
            let mut live = self.live.lock();
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
                    let (index, fields, dir) = open_index(root)?;
                    *live = live_of(index, fields, &self.config, Some(dir), snap.commit)?;
                    Some(old)
                }
                _ => {
                    *live = live_of(index, fields, &self.config, None, snap.commit)?;
                    None
                }
            };
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
            self.view(snap.commit)
        }

        pub fn status(&self, view: Option<&TextView>, store_seq: u64) -> TextStatus {
            let live = self.live.lock();
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
    }

    impl Drop for TextIndex {
        fn drop(&mut self) {
            // commit, let running merges finish (they write metadata too), then sync
            let close = || -> Result<()> {
                if !self.healthy() {
                    return Ok(());
                }
                let mut live = self.live.lock();
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
            };
            if let Err(e) = close() {
                tracing::warn!("full-text index checkpoint on close: {e}");
            }
        }
    }

    /// Evaluate a `text:query` call against the snapshot's text view.
    pub fn search(ctx: &Ctx, spec: &TextSpec, vars: &[VarId]) -> Result<Table> {
        let snap = &ctx.snap;
        let Some(view) = &snap.text else {
            return Err(Error::invalid(
                "dataset has no full-text index; enable it with `sparkles text-index` or --text",
            ));
        };
        if view.seq != snap.commit {
            return Err(unavailable("dataset", "stale", view.seq, snap.commit));
        }
        let sh = &view.index;
        let f = sh.fields;
        for p in &spec.predicates {
            if !sh.config.predicates.contains(p) {
                return Err(Error::invalid(format!(
                    "text:query: <{p}> is not text-indexed"
                )));
            }
        }
        let parser = QueryParser::for_index(&sh.index, vec![f.text]);
        let text = parser
            .parse_query(&spec.query)
            .map_err(|e| Error::invalid(format!("text:query: {e}")))?;
        let mut filters: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let str_terms = |field: Field, vals: &mut dyn Iterator<Item = String>| -> Box<dyn Query> {
            Box::new(TermSetQuery::new(
                vals.map(|v| Term::from_field_text(field, &v)),
            ))
        };
        if !spec.predicates.is_empty() {
            filters.push((
                Occur::Must,
                str_terms(f.p, &mut spec.predicates.iter().cloned()),
            ));
        }
        if let Some(lang) = &spec.lang {
            filters.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(f.lang, lang),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let PathEnd::Const(s) = &spec.subject {
            match term_key(snap, *s) {
                Some(k) => filters.push((
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_bytes(f.s, &k),
                        IndexRecordOption::Basic,
                    )),
                )),
                None => return Ok(Table::empty(vars.to_vec())),
            }
        }
        let default_term = || Term::from_field_text(f.g, DEFAULT_GRAPH_IRI);
        match &spec.graph {
            GraphFilter::All => {}
            GraphFilter::Default => filters.push((
                Occur::Must,
                Box::new(TermQuery::new(default_term(), IndexRecordOption::Basic)),
            )),
            GraphFilter::Named => filters.push((
                Occur::MustNot,
                Box::new(TermQuery::new(default_term(), IndexRecordOption::Basic)),
            )),
            GraphFilter::One(g) => {
                let names: Vec<String> = graph_name(snap, Id(*g)).into_iter().collect();
                filters.push((Occur::Must, str_terms(f.g, &mut names.into_iter())));
            }
            GraphFilter::Set(gs) => {
                let names: Vec<String> =
                    gs.iter().filter_map(|g| graph_name(snap, Id(*g))).collect();
                filters.push((Occur::Must, str_terms(f.g, &mut names.into_iter())));
            }
        }
        let query: Box<dyn Query> = if filters.is_empty() {
            text
        } else {
            let mut must = vec![(Occur::Must, text)];
            let has_positive = filters.iter().any(|(o, _)| *o == Occur::Must);
            let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
            for (o, q) in filters {
                match o {
                    Occur::Must => clauses.push((Occur::Must, q)),
                    other => must.push((other, q)),
                }
            }
            if has_positive {
                must.push((
                    Occur::Must,
                    Box::new(ConstScoreQuery::new(
                        Box::new(BooleanQuery::new(clauses)),
                        0.0,
                    )),
                ));
            }
            Box::new(BooleanQuery::new(must))
        };
        ctx.check()?;
        let max = sh.config.max_hits;
        let want = spec.limit.unwrap_or(max);
        // with dedup (a merged default graph), fetch more until enough distinct hits
        let mut fetch = if spec.dedup {
            want.saturating_mul(2)
        } else {
            want
        }
        .saturating_add(1)
        .min(max.saturating_add(1));
        let hits = loop {
            let hits = view
                .searcher
                .search(&query, &TopDocs::with_limit(fetch.max(1)).order_by_score())
                .map_err(text_err)?;
            if !spec.dedup || hits.len() < fetch || fetch > max {
                break hits;
            }
            fetch = fetch.saturating_mul(2).min(max.saturating_add(1));
            if fetch > max {
                continue;
            }
        };
        ctx.check()?;
        if spec.limit.is_none() && hits.len() > max {
            // more hits than a search may return without a limit
            return Err(Error::BudgetExceeded(crate::Budget {
                kind: crate::BudgetKind::Rows,
                limit: max as u64,
                requested: hits.len() as u64,
            }));
        }
        // the per-query memory budget, before the output is built
        ctx.check_output(hits.len(), vars.len())?;
        // columns
        let mut t = Table::new(vars.to_vec());
        let col = |v: Option<VarId>| v.and_then(|v| vars.iter().position(|x| *x == v));
        let (cs, cscore, clit, cg_out, cgv, cprop) = (
            match spec.subject {
                PathEnd::Var(v) => col(Some(v)),
                _ => None,
            },
            col(spec.score),
            col(spec.literal),
            col(spec.graph_out),
            col(spec.graph_var),
            col(spec.prop),
        );
        let mut seen: rustc_hash::FxHashSet<(Id, Id, Id)> = Default::default();
        let mut row = vec![Id::UNDEF; vars.len()];
        let mut stale_hits = 0usize;
        for (i, (score, addr)) in hits.into_iter().enumerate() {
            if i % 4096 == 4095 {
                ctx.check()?;
            }
            if t.len() >= want {
                break;
            }
            let d: TantivyDocument = view.searcher.doc(addr).map_err(text_err)?;
            let get_bytes = |fld: Field| d.get_first(fld).and_then(|v| v.as_bytes());
            let get_str = |fld: Field| d.get_first(fld).and_then(|v| v.as_str());
            let (Some(sk), Some(ok), Some(p), Some(g)) =
                (get_bytes(f.s), get_bytes(f.o), get_str(f.p), get_str(f.g))
            else {
                stale_hits += 1;
                continue;
            };
            let s_id = match sk.split_first() {
                Some((b'_', rest)) if rest.len() == 8 => {
                    Some(Id::bnode(u64::from_be_bytes(rest.try_into().unwrap())))
                }
                _ => snap.lookup_key(sk),
            };
            let (Some(s_id), Some(o_id), Some(p_id)) =
                (s_id, snap.lookup_key(ok), snap.lookup_iri(p))
            else {
                stale_hits += 1;
                continue;
            };
            let g_id = if g == DEFAULT_GRAPH_IRI {
                Some(Id::DEFAULT_GRAPH)
            } else if let Some(label) = g.strip_prefix("_:") {
                crate::store::parse_bnode_label(label)
            } else {
                snap.lookup_iri(g)
            };
            let Some(g_id) = g_id else {
                stale_hits += 1;
                continue;
            };
            if spec.dedup && !seen.insert((s_id, p_id, o_id)) {
                continue;
            }
            row.fill(Id::UNDEF);
            if let Some(c) = cs {
                row[c] = s_id;
            }
            if let Some(c) = cscore {
                row[c] = ctx.intern_value(&crate::sparql::value::Value::Float(score.into()));
            }
            if let Some(c) = clit {
                row[c] = o_id;
            }
            // the graph slot names the default graph by its IRI (a term, not the
            // store's default-graph marker)
            let g_term = if g_id == Id::DEFAULT_GRAPH {
                ctx.intern_term(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
                    DEFAULT_GRAPH_IRI,
                )))
            } else {
                g_id
            };
            if let Some(c) = cg_out {
                row[c] = g_term;
            }
            if let Some(c) = cgv {
                if row[c] != Id::UNDEF && row[c] != g_id {
                    continue; // ?g used both as GRAPH ?g and as the graph slot
                }
                row[c] = g_id;
            }
            if let Some(c) = cprop {
                row[c] = p_id;
            }
            t.push_row(&row);
            ctx.check_rows(t.len())?;
        }
        if stale_hits > 0 {
            tracing::warn!(
                "text:query skipped {stale_hits} hits whose terms are not in the snapshot"
            );
        }
        Ok(t)
    }
}
