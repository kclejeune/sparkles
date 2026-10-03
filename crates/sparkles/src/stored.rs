//! Stored, parameterized queries: named SPARQL queries kept per dataset in
//! `<db>/queries.json`, each with typed parameters, a description, a default result
//! format and a version history.
//!
//! A parameter is a variable of the query. A run binds it to one RDF term through
//! [`QueryOptions::initial_bindings`](crate::sparql::QueryOptions::initial_bindings),
//! which substitutes the term into the parsed query (Jena's `QueryExec.substitution`):
//! the value never becomes query text, so it cannot change the query's shape. Values are
//! checked against the parameter's type first. A query that assigns a parameter itself
//! (`BIND … AS ?p`, `VALUES ?p`, `(… AS ?p)`) is refused when it is stored.
//!
//! Every change of a query is a new version with the time, the author, an optional
//! message, the dataset's head commit when it was saved and a digest that chains the
//! versions, in the manner of commit identity. The last [`MAX_VERSIONS`] versions are
//! kept.

use crate::error::{Error, Result};
use oxrdf::{Literal, NamedNode, Term};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use spargebra::algebra::GraphPattern;
use spargebra::{Query, SparqlParser};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The file of a database directory that holds its stored queries.
pub const FILE: &str = "queries.json";

/// Version of the file's JSON shape.
pub const FORMAT: u32 = 1;

/// Versions kept per query; older ones are dropped.
pub const MAX_VERSIONS: usize = 100;

/// Longest query name.
pub const MAX_NAME: usize = 64;

/// Largest query text, in bytes.
pub const MAX_QUERY_BYTES: usize = 1 << 20;

/// Request parameters of `/{ds}/queries/{name}` that are not query parameters, so no
/// query parameter may be named so.
pub const RESERVED: &[&str] = &[
    "query",
    "update",
    "format",
    "output",
    "results",
    "timeout",
    "nocache",
    "reasoning",
    "at",
    "version",
    "send",
    "receipt",
    "default-graph-uri",
    "named-graph-uri",
    "memory-mb",
    "max-result-mb",
    "max-rows",
    "max-rows-produced",
];

/// Result formats a definition may name as its default: solutions formats for SELECT
/// and ASK, RDF syntaxes for CONSTRUCT and DESCRIBE.
pub const SOLUTION_FORMATS: &[&str] = &["json", "xml", "csv", "tsv"];
pub const GRAPH_FORMATS: &[&str] = &["turtle", "ntriples", "jsonld", "rdfxml"];

/// The type of a parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ParamType {
    /// An absolute IRI, `<iri>` or a prefixed name of the dataset's prefixes.
    Iri,
    /// A plain string (or, with `language`, a language-tagged one).
    String,
    Integer,
    Decimal,
    Double,
    Boolean,
    Date,
    DateTime,
    /// A literal in SPARQL syntax (`"x"@en`, `"5"^^xsd:int`, `5`), or, with `datatype`,
    /// the lexical form of a literal of that datatype.
    Literal,
    /// An IRI or a literal in SPARQL syntax.
    Term,
}

impl ParamType {
    pub fn name(self) -> &'static str {
        match self {
            ParamType::Iri => "iri",
            ParamType::String => "string",
            ParamType::Integer => "integer",
            ParamType::Decimal => "decimal",
            ParamType::Double => "double",
            ParamType::Boolean => "boolean",
            ParamType::Date => "date",
            ParamType::DateTime => "dateTime",
            ParamType::Literal => "literal",
            ParamType::Term => "term",
        }
    }

    /// The XSD datatype of the typed kinds.
    fn xsd(self) -> Option<&'static str> {
        Some(match self {
            ParamType::Integer => "http://www.w3.org/2001/XMLSchema#integer",
            ParamType::Decimal => "http://www.w3.org/2001/XMLSchema#decimal",
            ParamType::Double => "http://www.w3.org/2001/XMLSchema#double",
            ParamType::Boolean => "http://www.w3.org/2001/XMLSchema#boolean",
            ParamType::Date => "http://www.w3.org/2001/XMLSchema#date",
            ParamType::DateTime => "http://www.w3.org/2001/XMLSchema#dateTime",
            _ => return None,
        })
    }
}

/// A declared parameter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Parameter {
    #[serde(rename = "type")]
    pub kind: ParamType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The value used when a run gives none (in the request's form: a string, or a JSON
    /// number or boolean).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Whether a run must give a value. Defaults to `true` without a `default`; an
    /// optional parameter without a default stays unbound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// `literal`: the datatype IRI of the value's lexical form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datatype: Option<String>,
    /// `string`: the language tag of the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// The values a run may give (in the request's form).
    #[serde(default, rename = "enum", skip_serializing_if = "Option::is_none")]
    pub allowed: Option<Vec<Value>>,
}

impl Parameter {
    /// Whether a run must give a value.
    pub fn is_required(&self) -> bool {
        self.required.unwrap_or(self.default.is_none())
    }
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

/// What a client stores: the query and how to run it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Definition {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Parameters by variable name (without `?`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, Parameter>,
    /// The result format of runs that ask for none (`json`, `xml`, `csv`, `tsv`, or
    /// `turtle`, `ntriples`, `jsonld`, `rdfxml` for graphs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<String>,
    /// Offer the query as an MCP tool.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub mcp: bool,
}

/// The kind of a stored query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Kind {
    Select,
    Ask,
    Construct,
    Describe,
}

/// A query name: `[A-Za-z0-9_-]`, starting with a letter or digit, at most
/// [`MAX_NAME`] characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

fn valid_param_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}

/// Whether `?name` or `$name` occurs in the text as a whole variable.
fn mentions(text: &str, name: &str) -> bool {
    let bytes = text.as_bytes();
    for (i, _) in text.match_indices(name) {
        let before = i.checked_sub(1).map(|j| bytes[j]);
        let after = bytes.get(i + name.len()).copied();
        if matches!(before, Some(b'?' | b'$'))
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80)
        {
            return true;
        }
    }
    false
}

/// Variables a pattern assigns: `BIND … AS ?v`, `VALUES ?v`, aggregates and projected
/// expressions (`(… AS ?v)`, which become `Extend`).
fn assigned(p: &GraphPattern, out: &mut Vec<String>) {
    use GraphPattern as G;
    match p {
        G::Bgp { .. } | G::Path { .. } => {}
        G::Join { left, right }
        | G::Lateral { left, right }
        | G::LeftJoin { left, right, .. }
        | G::Union { left, right }
        | G::Minus { left, right } => {
            assigned(left, out);
            assigned(right, out);
        }
        G::Filter { inner, .. }
        | G::Graph { inner, .. }
        | G::Project { inner, .. }
        | G::Distinct { inner }
        | G::Reduced { inner }
        | G::Slice { inner, .. }
        | G::OrderBy { inner, .. }
        | G::Service { inner, .. } => assigned(inner, out),
        G::Extend {
            inner, variable, ..
        } => {
            out.push(variable.as_str().to_string());
            assigned(inner, out);
        }
        G::Values { variables, .. } => {
            out.extend(variables.iter().map(|v| v.as_str().to_string()));
        }
        G::Group {
            inner, aggregates, ..
        } => {
            out.extend(aggregates.iter().map(|(v, _)| v.as_str().to_string()));
            assigned(inner, out);
        }
    }
}

/// Parse a stored query's text (no predeclared prefixes: stored queries are
/// self-contained).
pub fn parse(text: &str) -> Result<Query> {
    Ok(SparqlParser::new().parse_query(text)?)
}

fn kind_of(q: &Query) -> Kind {
    match q {
        Query::Select { .. } => Kind::Select,
        Query::Ask { .. } => Kind::Ask,
        Query::Construct { .. } => Kind::Construct,
        Query::Describe { .. } => Kind::Describe,
    }
}

fn pattern(q: &Query) -> &GraphPattern {
    match q {
        Query::Select { pattern, .. }
        | Query::Ask { pattern, .. }
        | Query::Construct { pattern, .. }
        | Query::Describe { pattern, .. } => pattern,
    }
}

impl Definition {
    /// Check the definition: the text is one query, every parameter is a variable of it
    /// that the query does not assign, and defaults, allowed values and the result format
    /// fit. Returns the query's kind.
    pub fn check(&self) -> Result<Kind> {
        if self.query.len() > MAX_QUERY_BYTES {
            return Err(invalid(format!("query: at most {MAX_QUERY_BYTES} bytes")));
        }
        let parsed = parse(&self.query).map_err(|e| {
            if SparqlParser::new().parse_update(&self.query).is_ok() {
                invalid("only queries can be stored, not updates")
            } else {
                e
            }
        })?;
        let kind = kind_of(&parsed);
        let mut assigns = Vec::new();
        assigned(pattern(&parsed), &mut assigns);
        for (name, p) in &self.parameters {
            if !valid_param_name(name) {
                return Err(invalid(format!(
                    "parameter '{name}': a name is a SPARQL variable name of letters, digits and _, without ?"
                )));
            }
            if RESERVED.contains(&name.as_str()) {
                return Err(invalid(format!(
                    "parameter '{name}': the name is reserved for a request parameter"
                )));
            }
            if !mentions(&self.query, name) {
                return Err(invalid(format!(
                    "parameter '{name}': the query has no variable ?{name}"
                )));
            }
            if assigns.iter().any(|a| a == name) {
                return Err(invalid(format!(
                    "parameter '{name}': the query assigns ?{name} itself (BIND, VALUES or AS)"
                )));
            }
            if p.datatype.is_some() && p.kind != ParamType::Literal {
                return Err(invalid(format!(
                    "parameter '{name}': datatype applies to type literal only"
                )));
            }
            if let Some(dt) = &p.datatype {
                NamedNode::new(dt.as_str())
                    .map_err(|e| invalid(format!("parameter '{name}': datatype: {e}")))?;
            }
            if p.language.is_some() && p.kind != ParamType::String {
                return Err(invalid(format!(
                    "parameter '{name}': language applies to type string only"
                )));
            }
            if let Some(l) = &p.language {
                Literal::new_language_tagged_literal("", l.as_str())
                    .map_err(|e| invalid(format!("parameter '{name}': language: {e}")))?;
            }
            let none = BTreeMap::new();
            if let Some(d) = &p.default {
                term(name, p, d, &none)
                    .map_err(|e| invalid(format!("parameter '{name}': default: {e}")))?;
            }
            for v in p.allowed.iter().flatten() {
                term(name, p, v, &none)
                    .map_err(|e| invalid(format!("parameter '{name}': enum: {e}")))?;
            }
        }
        if let Some(r) = &self.results {
            let ok = match kind {
                Kind::Select | Kind::Ask => SOLUTION_FORMATS.contains(&r.as_str()),
                Kind::Construct | Kind::Describe => GRAPH_FORMATS.contains(&r.as_str()),
            };
            if !ok {
                let list = match kind {
                    Kind::Select | Kind::Ask => SOLUTION_FORMATS,
                    _ => GRAPH_FORMATS,
                };
                return Err(invalid(format!(
                    "results: '{r}' is not a format of this query; use {}",
                    list.join(", ")
                )));
            }
        }
        Ok(kind)
    }

    /// The terms of a run: each parameter's value from `given` (by name), its default,
    /// or nothing for an optional one. A value that does not fit its type, a missing
    /// required value and a name that is not a parameter are errors that name the
    /// parameter.
    pub fn bind(
        &self,
        given: &BTreeMap<String, Value>,
        prefixes: &BTreeMap<String, String>,
    ) -> Result<Vec<(String, Term)>> {
        for k in given.keys() {
            if !self.parameters.contains_key(k) {
                let known: Vec<&str> = self.parameters.keys().map(String::as_str).collect();
                return Err(invalid(if known.is_empty() {
                    format!("unknown parameter '{k}': this query has no parameters")
                } else {
                    format!(
                        "unknown parameter '{k}': the parameters are {}",
                        known.join(", ")
                    )
                }));
            }
        }
        let mut out = Vec::new();
        for (name, p) in &self.parameters {
            let v = match given.get(name).or(p.default.as_ref()) {
                Some(v) => v,
                None if p.is_required() => {
                    return Err(invalid(format!(
                        "missing parameter '{name}' ({})",
                        p.kind.name()
                    )));
                }
                None => continue,
            };
            let t = term(name, p, v, prefixes)
                .map_err(|e| invalid(format!("parameter '{name}': {e}")))?;
            if let Some(allowed) = &p.allowed {
                let ok = allowed
                    .iter()
                    .any(|a| term(name, p, a, prefixes).is_ok_and(|x| x == t));
                if !ok {
                    return Err(invalid(format!(
                        "parameter '{name}': {t} is not one of the allowed values"
                    )));
                }
            }
            out.push((name.clone(), t));
        }
        Ok(out)
    }
}

/// The text of a request value: a string as is, a number or boolean in JSON syntax.
fn text_of(v: &Value) -> std::result::Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err("expected a string, a number or a boolean".into()),
    }
}

/// One term in SPARQL syntax: an IRI, a prefixed name or a literal. It is parsed as the
/// only value of a `VALUES` block, and anything but exactly one term is refused.
fn sparql_term(
    text: &str,
    prefixes: &BTreeMap<String, String>,
) -> std::result::Result<Term, String> {
    let t = text.trim();
    if t.is_empty() || t.contains(['{', '}', '\n', '\r']) && !t.starts_with('"') {
        return Err(format!("'{text}' is not a term"));
    }
    let mut parser = SparqlParser::new();
    for (p, ns) in prefixes {
        parser = parser.with_prefix(p, ns).map_err(|e| e.to_string())?;
    }
    let q = parser
        .parse_query(&format!("SELECT * {{ VALUES ?v {{ {t} }} }}"))
        .map_err(|_| format!("'{text}' is not an IRI or literal in SPARQL syntax"))?;
    let bad = || format!("'{text}' is not a single IRI or literal");
    let Query::Select { pattern, .. } = q else {
        return Err(bad());
    };
    let GraphPattern::Project { inner, variables } = pattern else {
        return Err(bad());
    };
    let GraphPattern::Values {
        variables: vars,
        bindings,
    } = *inner
    else {
        return Err(bad());
    };
    if variables.len() != 1 || vars.len() != 1 || bindings.len() != 1 {
        return Err(bad());
    }
    match bindings
        .into_iter()
        .next()
        .and_then(|r| r.into_iter().next())
    {
        Some(Some(spargebra::term::GroundTerm::NamedNode(n))) => Ok(Term::NamedNode(n)),
        Some(Some(spargebra::term::GroundTerm::Literal(l))) => Ok(Term::Literal(l)),
        _ => Err(bad()),
    }
}

fn checked(l: Literal) -> std::result::Result<Term, String> {
    if crate::xsd::is_valid(&l) {
        Ok(Term::Literal(l))
    } else {
        Err(format!(
            "'{}' is not a valid {}",
            l.value(),
            l.datatype().as_str()
        ))
    }
}

/// The term of one value of parameter `p`.
fn term(
    name: &str,
    p: &Parameter,
    v: &Value,
    prefixes: &BTreeMap<String, String>,
) -> std::result::Result<Term, String> {
    let _ = name;
    let text = text_of(v)?;
    match p.kind {
        ParamType::Iri => {
            let t = text.trim();
            let iri = if let Some(i) = t.strip_prefix('<').and_then(|i| i.strip_suffix('>')) {
                i.to_string()
            } else if let Some((pfx, local)) = t.split_once(':')
                && !local.starts_with("//")
                && let Some(ns) = prefixes.get(pfx)
            {
                format!("{ns}{local}")
            } else {
                t.to_string()
            };
            NamedNode::new(iri.as_str())
                .map(Term::NamedNode)
                .map_err(|e| format!("'{text}' is not an IRI: {e}"))
        }
        ParamType::String => Ok(Term::Literal(match &p.language {
            Some(l) => Literal::new_language_tagged_literal(text, l)
                .map_err(|e| format!("language: {e}"))?,
            None => Literal::new_simple_literal(text),
        })),
        ParamType::Literal if p.datatype.is_some() => {
            let dt = NamedNode::new(p.datatype.as_deref().unwrap_or_default())
                .map_err(|e| e.to_string())?;
            checked(Literal::new_typed_literal(text, dt))
        }
        ParamType::Literal => match sparql_term(&text, prefixes)? {
            Term::Literal(l) => checked(l),
            _ => Err(format!("'{text}' is not a literal")),
        },
        ParamType::Term => match sparql_term(&text, prefixes)? {
            Term::Literal(l) => checked(l),
            t => Ok(t),
        },
        k => {
            let dt = NamedNode::new_unchecked(k.xsd().unwrap_or_default());
            checked(Literal::new_typed_literal(text.trim(), dt))
        }
    }
}

// ----------------------------------------------------------------- versions ------

/// The metadata of one version of a query.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    /// 1 for the first version, then one more per change.
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    /// RFC 3339 UTC.
    pub created: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The dataset's head commit when the version was saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_commit: Option<u64>,
    /// Hex SHA-256 over the parent's digest and the definition's JSON.
    pub digest: String,
}

/// One stored version: its metadata and its definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stored {
    #[serde(flatten)]
    pub version: Version,
    pub definition: Definition,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Entry {
    /// oldest first; the last one is current
    versions: Vec<Stored>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct FileDoc {
    format: u32,
    queries: BTreeMap<String, Entry>,
}

/// Who saves a version, and the version a client expects to replace.
#[derive(Clone, Debug, Default)]
pub struct Change {
    pub author: Option<String>,
    pub message: Option<String>,
    pub dataset_commit: Option<u64>,
    /// Fail with [`Error::PreconditionFailed`] unless the current version is this one
    /// (`Some(0)`: the query must not exist yet).
    pub if_version: Option<u64>,
}

/// The outcome of a [`Catalog::put`].
#[derive(Clone, Debug)]
pub struct Saved {
    pub stored: Stored,
    /// `false`: the definition equals the current one and no version was added.
    pub changed: bool,
    pub created: bool,
}

/// The stored queries of one dataset, kept in memory and written to `<root>/queries.json`
/// (in-memory datasets keep them in memory only).
pub struct Catalog {
    root: Option<PathBuf>,
    doc: RwLock<FileDoc>,
    /// why the file could not be read ([`Catalog::open_or_broken`]); changes are refused
    broken: Option<String>,
}

fn digest(parent: Option<&str>, def: &Definition) -> String {
    let mut h = Sha256::new();
    h.update(parent.unwrap_or("").as_bytes());
    h.update([0u8]);
    h.update(serde_json::to_vec(def).unwrap_or_default());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

impl Catalog {
    /// The catalog of a database directory (`None`: in memory). A missing file is an
    /// empty catalog; a malformed one is an error.
    pub fn open(root: Option<&Path>) -> Result<Catalog> {
        let doc = match root {
            Some(r) => match std::fs::read(r.join(FILE)) {
                Ok(b) => {
                    let d: FileDoc =
                        serde_json::from_slice(&b).map_err(|e| invalid(format!("{FILE}: {e}")))?;
                    if d.format != FORMAT {
                        return Err(invalid(format!(
                            "{FILE}: unknown format {} (this version reads {FORMAT})",
                            d.format
                        )));
                    }
                    d
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileDoc {
                    format: FORMAT,
                    ..Default::default()
                },
                Err(e) => return Err(e.into()),
            },
            None => FileDoc {
                format: FORMAT,
                ..Default::default()
            },
        };
        Ok(Catalog {
            root: root.map(Path::to_path_buf),
            doc: RwLock::new(doc),
            broken: None,
        })
    }

    /// [`Catalog::open`], or, when the file cannot be read, an empty catalog that refuses
    /// changes (so a malformed file is never overwritten) and names the problem.
    pub fn open_or_broken(root: Option<&Path>) -> Catalog {
        Catalog::open(root).unwrap_or_else(|e| Catalog {
            root: root.map(Path::to_path_buf),
            doc: RwLock::new(FileDoc {
                format: FORMAT,
                ..Default::default()
            }),
            broken: Some(e.to_string()),
        })
    }

    /// Why the file could not be read, if it could not.
    pub fn broken(&self) -> Option<&str> {
        self.broken.as_deref()
    }

    fn writable(&self) -> Result<()> {
        match &self.broken {
            Some(e) => Err(Error::Conflict(format!(
                "the stored queries cannot be changed until {FILE} is fixed or removed: {e}"
            ))),
            None => Ok(()),
        }
    }

    /// The current version of every query, by name.
    pub fn list(&self) -> Vec<(String, Stored)> {
        self.doc
            .read()
            .queries
            .iter()
            .filter_map(|(n, e)| Some((n.clone(), e.versions.last()?.clone())))
            .collect()
    }

    /// The current version of a query, or the given version while it is kept.
    pub fn get(&self, name: &str, version: Option<u64>) -> Option<Stored> {
        let doc = self.doc.read();
        let e = doc.queries.get(name)?;
        match version {
            None => e.versions.last().cloned(),
            Some(v) => e.versions.iter().find(|s| s.version.version == v).cloned(),
        }
    }

    /// The kept versions of a query, newest first.
    pub fn versions(&self, name: &str) -> Option<Vec<Version>> {
        let doc = self.doc.read();
        let e = doc.queries.get(name)?;
        Some(e.versions.iter().rev().map(|s| s.version.clone()).collect())
    }

    fn save(&self, doc: &FileDoc) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let path = root.join(FILE);
        if doc.queries.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            };
        }
        let bytes = serde_json::to_vec_pretty(doc).map_err(|e| Error::Io(e.into()))?;
        crate::store::write_atomic(&path, &bytes)?;
        Ok(())
    }

    /// Store a checked definition as the query's next version. An unchanged definition
    /// adds no version.
    pub fn put(&self, name: &str, def: Definition, change: Change) -> Result<Saved> {
        if !valid_name(name) {
            return Err(invalid(format!(
                "invalid query name '{name}': use letters, digits, _ and -, at most {MAX_NAME} characters"
            )));
        }
        self.writable()?;
        def.check()?;
        let mut doc = self.doc.write();
        let current = doc
            .queries
            .get(name)
            .and_then(|e| e.versions.last())
            .cloned();
        let have = current.as_ref().map_or(0, |c| c.version.version);
        if let Some(want) = change.if_version
            && want != have
        {
            return Err(Error::PreconditionFailed(format!(
                "query '{name}' is at version {have}, not {want}"
            )));
        }
        if let Some(c) = &current
            && c.definition == def
        {
            return Ok(Saved {
                stored: c.clone(),
                changed: false,
                created: false,
            });
        }
        let version = Version {
            version: have + 1,
            parent: (have > 0).then_some(have),
            created: crate::builder::now_rfc3339(),
            author: change.author,
            message: change.message,
            dataset_commit: change.dataset_commit,
            digest: digest(current.as_ref().map(|c| c.version.digest.as_str()), &def),
        };
        let stored = Stored {
            version,
            definition: def,
        };
        let mut next = doc.clone();
        let entry = next.queries.entry(name.to_string()).or_default();
        entry.versions.push(stored.clone());
        if entry.versions.len() > MAX_VERSIONS {
            let drop = entry.versions.len() - MAX_VERSIONS;
            entry.versions.drain(..drop);
        }
        self.save(&next)?;
        *doc = next;
        Ok(Saved {
            stored,
            changed: true,
            created: have == 0,
        })
    }

    /// Remove a query and its versions; `false` when there was none.
    pub fn delete(&self, name: &str, if_version: Option<u64>) -> Result<bool> {
        self.writable()?;
        let mut doc = self.doc.write();
        let have = doc
            .queries
            .get(name)
            .and_then(|e| e.versions.last())
            .map_or(0, |c| c.version.version);
        if let Some(want) = if_version
            && want != have
        {
            return Err(Error::PreconditionFailed(format!(
                "query '{name}' is at version {have}, not {want}"
            )));
        }
        if have == 0 {
            return Ok(false);
        }
        let mut next = doc.clone();
        next.queries.remove(name);
        self.save(&next)?;
        *doc = next;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
