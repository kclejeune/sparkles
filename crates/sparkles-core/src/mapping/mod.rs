//! R2RML, RML and YARRRML mappings (spec F19 Phase 1): one mapping engine with three
//! front ends, and the materialization of a mapping's quads.
//!
//! [`parse`] reads a mapping into the [`model`]. [`run`] streams each logical source
//! once and hands every generated quad to a sink. [`write`] serializes the quads, and
//! [`to_nquads_file`] writes them to the temporary N-Quads file that the loader reads as
//! an ordinary [`Source`](crate::io::Source), as C05 does with a table.

pub mod exec;
pub mod functions;
pub mod jsonpath;
pub mod model;
pub mod rdf;
pub mod source;
pub mod template;
#[cfg(test)]
mod tests;
pub mod yarrrml;

pub use exec::{Stats, run};
pub use model::Mapping;

use crate::error::{Budget, BudgetKind, Error, Result};
use oxrdf::Quad;
use oxrdfio::{RdfFormat, RdfSerializer};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `rr:defaultGraph` and its RML-Core twin: a graph map with this constant writes to the
/// default graph.
pub const DEFAULT_GRAPH_RR: &str = "http://www.w3.org/ns/r2rml#defaultGraph";
pub const DEFAULT_GRAPH_RML: &str = "http://w3id.org/rml/defaultGraph";

/// The IRI prefix by which a mapping names a connection of the server or the run.
pub const CONNECTION_PREFIX: &str = "urn:x-sparkles:connection:";

/// The largest mapping text (spec F19 §5.4).
pub const MAX_MAPPING_BYTES: usize = 1 << 20;
/// The most triples maps of one mapping.
pub const MAX_TRIPLES_MAPS: usize = 10_000;
/// The default limit on a JSON document's size.
pub const DEFAULT_MAX_JSON_BYTES: u64 = 512 << 20;

/// The kinds of mapping a file can hold, read from its content (spec F19 §2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MappingKind {
    /// R2RML or RML in Turtle or N-Triples.
    Rml,
    /// YARRRML in YAML.
    Yarrrml,
    /// CSVW metadata (C05).
    Csvw,
    /// A SPARQL CONSTRUCT template (C05).
    Template,
}

impl MappingKind {
    pub fn name(self) -> &'static str {
        match self {
            MappingKind::Rml => "rml",
            MappingKind::Yarrrml => "yarrrml",
            MappingKind::Csvw => "csvw",
            MappingKind::Template => "template",
        }
    }

    /// A kind by name (`--mapping-kind`): `rml` and `r2rml` are one kind.
    pub fn parse(s: &str) -> Option<MappingKind> {
        Some(match s.to_ascii_lowercase().as_str() {
            "rml" | "r2rml" | "turtle" => MappingKind::Rml,
            "yarrrml" | "yaml" => MappingKind::Yarrrml,
            "csvw" => MappingKind::Csvw,
            "template" | "sparql" | "tarql" => MappingKind::Template,
            _ => return None,
        })
    }

    /// The kind a media type names (`PUT /$/mappings/{ds}/{name}`).
    pub fn from_media_type(mt: &str) -> Option<MappingKind> {
        let base = mt.split(';').next()?.trim().to_ascii_lowercase();
        Some(match base.as_str() {
            "text/turtle" | "application/n-triples" | "application/x-turtle" => MappingKind::Rml,
            "application/yaml" | "application/x-yaml" | "text/yaml" | "text/x-yaml" => {
                MappingKind::Yarrrml
            }
            "application/csvm+json" | "application/json" => MappingKind::Csvw,
            "application/sparql-query" => MappingKind::Template,
            _ => return None,
        })
    }

    /// The media type a stored mapping of this kind is served with.
    pub fn media_type(self) -> &'static str {
        match self {
            MappingKind::Rml => "text/turtle",
            MappingKind::Yarrrml => "application/yaml",
            MappingKind::Csvw => "application/csvm+json",
            MappingKind::Template => "application/sparql-query",
        }
    }
}

/// The kind of a mapping text, read from its content: CSVW metadata is a JSON object
/// whose `@context` names CSVW, YARRRML is YAML with a `mappings` key, R2RML and RML name
/// their vocabularies, and a template is a CONSTRUCT query.
pub fn detect(text: &str) -> Option<MappingKind> {
    let t = text.trim_start_matches('\u{feff}').trim_start();
    if t.starts_with('{') {
        return t
            .contains("http://www.w3.org/ns/csvw")
            .then_some(MappingKind::Csvw);
    }
    let has = |needle: &str| t.contains(needle);
    if has("http://www.w3.org/ns/r2rml#")
        || has("http://semweb.mmlab.be/ns/rml#")
        || has("http://w3id.org/rml/")
    {
        // a template may declare the prefixes too, but a mapping has no CONSTRUCT
        if !construct_keyword(t) {
            return Some(MappingKind::Rml);
        }
    }
    if construct_keyword(t) {
        return Some(MappingKind::Template);
    }
    if yaml_mappings_key(t) {
        return Some(MappingKind::Yarrrml);
    }
    None
}

fn construct_keyword(t: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bCONSTRUCT\s*(\{|WHERE)").expect("a regex"))
        .is_match(t)
}

fn yaml_mappings_key(t: &str) -> bool {
    t.lines().any(|l| {
        let l = l.trim_end();
        l.starts_with("mappings:") || l.starts_with("\"mappings\":") || l.starts_with("'mappings':")
    })
}

/// Parse an R2RML, RML or YARRRML mapping. `base` is the mapping's location, against
/// which relative IRIs of the mapping text resolve, and the base IRI of generated IRIs
/// when the mapping gives none.
pub fn parse(text: &str, kind: MappingKind, base: Option<&str>) -> Result<Mapping> {
    if text.len() > MAX_MAPPING_BYTES {
        return Err(Error::invalid(format!(
            "the mapping is larger than {}",
            crate::error::human_bytes(MAX_MAPPING_BYTES as u64)
        )));
    }
    let m = match kind {
        MappingKind::Rml => rdf::parse(text, base)?,
        MappingKind::Yarrrml => yarrrml::parse(text, base)?,
        MappingKind::Csvw | MappingKind::Template => {
            return Err(Error::invalid(
                "CSVW metadata and CONSTRUCT templates are read by the CSV converter, not the mapping engine",
            ));
        }
    };
    if m.maps.len() > MAX_TRIPLES_MAPS {
        return Err(Error::invalid(format!(
            "the mapping has {} triples maps; the limit is {MAX_TRIPLES_MAPS}",
            m.maps.len()
        )));
    }
    exec::validate(&m)?;
    Ok(m)
}

/// A database connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    Sqlite { path: PathBuf },
    Postgres { url: String },
    Duckdb { path: PathBuf },
}

impl Connection {
    /// A connection from a URL: `sqlite:PATH`, `postgres://…` or `duckdb:PATH`.
    pub fn from_url(url: &str) -> Result<Connection> {
        if let Some(p) = url
            .strip_prefix("sqlite://")
            .or_else(|| url.strip_prefix("sqlite:"))
        {
            return Ok(Connection::Sqlite { path: p.into() });
        }
        if let Some(p) = url
            .strip_prefix("duckdb://")
            .or_else(|| url.strip_prefix("duckdb:"))
        {
            return Ok(Connection::Duckdb { path: p.into() });
        }
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            return Ok(Connection::Postgres { url: url.into() });
        }
        Err(Error::invalid(format!(
            "connection URL {url:?}: use sqlite:PATH, postgres://… or duckdb:PATH"
        )))
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Connection::Sqlite { .. } => "sqlite",
            Connection::Postgres { .. } => "postgres",
            Connection::Duckdb { .. } => "duckdb",
        }
    }
}

/// A named connection of a run.
#[derive(Clone, Debug)]
pub struct NamedConnection {
    pub name: String,
    pub connection: Connection,
    /// The `d2rq:jdbcDSN` that names this connection in existing RML mappings.
    pub jdbc_dsn: Option<String>,
}

/// What a run reads: files by name, and connections.
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    /// Files given by name: an upload's parts, or `--source NAME=PATH`. A source's file
    /// is matched by its whole name first and then by its last path segment.
    pub files: Vec<(String, PathBuf)>,
    /// The directory that other relative file names resolve against (the mapping
    /// file's directory on the command line). `None` reads the given files only.
    pub dir: Option<PathBuf>,
    pub connections: Vec<NamedConnection>,
}

impl Inputs {
    /// The path of a file source, or `mapping-source-missing`.
    pub fn file(&self, name: &str) -> Result<PathBuf> {
        self.optional_file(name).ok_or_else(|| {
            Error::invalid(format!(
                "mapping-source-missing: the mapping reads the file {name:?}, and no file of that name was given"
            ))
        })
    }

    /// The path of a file, when one was given or exists in [`Inputs::dir`].
    pub fn optional_file(&self, name: &str) -> Option<PathBuf> {
        if let Some((_, p)) = self.files.iter().find(|(n, _)| n == name) {
            return Some(p.clone());
        }
        let last = |s: &str| s.rsplit(['/', '\\']).next().unwrap_or(s).to_string();
        let want = last(name);
        if let Some((_, p)) = self.files.iter().find(|(n, _)| last(n) == want) {
            return Some(p.clone());
        }
        let dir = self.dir.as_ref()?;
        let name = name.strip_prefix("file://").unwrap_or(name);
        let p = Path::new(name);
        let p = if p.is_absolute() {
            p.to_path_buf()
        } else {
            dir.join(p)
        };
        p.is_file().then_some(p)
    }

    /// A connection by name: `""` is the run's default connection (the only one, or
    /// the one named `default`), which R2RML logical tables read since an R2RML
    /// mapping names no database.
    pub fn connection(&self, name: &str) -> Result<&Connection> {
        if name.is_empty() {
            return match self.connections.as_slice() {
                [only] => Ok(&only.connection),
                cs => cs
                    .iter()
                    .find(|c| c.name == "default")
                    .map(|c| &c.connection)
                    .ok_or_else(|| {
                        Error::invalid(if cs.is_empty() {
                            "the mapping reads a database table, and no connection was given (--connection NAME=URL)".to_string()
                        } else {
                            "the mapping's logical tables name no connection, and several were given: name one `default`".to_string()
                        })
                    }),
            };
        }
        self.connections
            .iter()
            .find(|c| c.name == name || c.jdbc_dsn.as_deref() == Some(name))
            .map(|c| &c.connection)
            .ok_or_else(|| {
                Error::invalid(format!(
                    "mapping-connection-missing: the mapping reads connection {name:?}, which is not declared"
                ))
            })
    }
}

/// A timeout or cancellation check, called every 65,536 rows.
pub type Check = Arc<dyn Fn() -> Result<()> + Send + Sync>;

/// How a run reads and what it may spend.
#[derive(Clone)]
pub struct RunOptions {
    pub check: Option<Check>,
    /// The decompressed-size limit of compressed file sources.
    pub max_decompressed: Option<u64>,
    pub max_json_bytes: u64,
    /// The memory the parent tables of in-memory joins may hold (`None`: unbounded, as
    /// on the command line).
    pub memory_budget: Option<u64>,
    /// The base IRI of generated relative IRIs, over the mapping's own.
    pub base: Option<String>,
    /// Bytes written by [`write`] and [`to_nquads_file`].
    pub max_output_bytes: Option<u64>,
    /// Write every quad into this graph, and refuse a mapping whose graph maps name
    /// another (a Graph Store request addresses one graph: spec F19 §2.3).
    pub only_graph: Option<oxrdf::NamedNode>,
}

impl Default for RunOptions {
    fn default() -> RunOptions {
        RunOptions {
            check: None,
            max_decompressed: None,
            max_json_bytes: DEFAULT_MAX_JSON_BYTES,
            memory_budget: None,
            base: None,
            max_output_bytes: None,
            only_graph: None,
        }
    }
}

/// A writer that fails past a byte limit.
struct Counting<W> {
    inner: W,
    n: u64,
    limit: Option<u64>,
}

const OUTPUT_LIMIT: &str = "sparkles: mapping output limit";

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.n += buf.len() as u64;
        if let Some(l) = self.limit
            && self.n > l
        {
            return Err(std::io::Error::other(OUTPUT_LIMIT));
        }
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Run a mapping and serialize its quads in `format`. A triples-only format gets the
/// default graph's triples and fails on a quad in a named graph.
pub fn write(
    m: &Mapping,
    inputs: &Inputs,
    opts: &RunOptions,
    out: impl Write,
    format: RdfFormat,
) -> Result<Stats> {
    let limit = opts.max_output_bytes;
    let mut prefixes = vec![
        (
            "rdf".to_string(),
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#".to_string(),
        ),
        (
            "xsd".to_string(),
            "http://www.w3.org/2001/XMLSchema#".to_string(),
        ),
    ];
    for (p, ns) in &m.prefixes {
        if !prefixes.iter().any(|(q, _)| q == p)
            && !ns.starts_with("http://www.w3.org/ns/r2rml#")
            && !ns.starts_with("http://semweb.mmlab.be/ns/")
            && !ns.starts_with("http://w3id.org/rml/")
        {
            prefixes.push((p.clone(), ns.clone()));
        }
    }
    let ser = crate::io::with_prefixes(RdfSerializer::from_format(format), prefixes);
    let mut w = ser.for_writer(Counting {
        inner: out,
        n: 0,
        limit,
    });
    let map_io = |e: std::io::Error| -> Error {
        if e.to_string() == OUTPUT_LIMIT {
            Error::BudgetExceeded(Budget {
                kind: BudgetKind::DecompressedBytes,
                limit: limit.unwrap_or(0),
                requested: limit.unwrap_or(0) + 1,
            })
        } else {
            Error::Io(e)
        }
    };
    let quads = format.supports_datasets();
    let stats = run(m, inputs, opts, &mut |q: Quad| {
        if quads {
            w.serialize_quad(&q).map_err(map_io)
        } else if q.graph_name.is_default_graph() {
            w.serialize_triple(oxrdf::TripleRef::from(q.as_ref()))
                .map_err(map_io)
        } else {
            Err(Error::invalid(format!(
                "the mapping writes into the graph {}; use a quad format such as N-Quads",
                q.graph_name
            )))
        }
    })?;
    let mut inner = w.finish().map_err(map_io)?;
    inner.flush().map_err(map_io)?;
    Ok(stats)
}

/// Run a mapping into a temporary N-Quads file in `dir` (the system's temporary
/// directory by default), for the loader. The file is removed when the path is dropped.
pub fn to_nquads_file(
    m: &Mapping,
    inputs: &Inputs,
    opts: &RunOptions,
    dir: Option<&Path>,
) -> Result<(tempfile::TempPath, Stats)> {
    let mut b = tempfile::Builder::new();
    b.prefix("sparkles-map-").suffix(".nq");
    let f = match dir {
        Some(d) => b.tempfile_in(d)?,
        None => b.tempfile()?,
    };
    let (file, path) = f.into_parts();
    let stats = write(
        m,
        inputs,
        opts,
        std::io::BufWriter::with_capacity(1 << 20, file),
        RdfFormat::NQuads,
    )?;
    Ok((path, stats))
}

/// An N-Quads file written by [`to_nquads_file`] as a loader source, named `name` in
/// messages, with its default-graph triples loaded into `graph`.
pub fn source(path: &Path, graph: Option<oxrdf::NamedNode>, name: &str) -> crate::io::Source {
    let mut s = crate::io::Source::from_bytes(Vec::new(), RdfFormat::NQuads, graph);
    s.data = crate::io::SourceData::File(path.to_path_buf());
    s.name = name.to_string();
    s
}
