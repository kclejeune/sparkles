//! Imports and EXTERNAL shapes: where their definitions come from ([`Resolver`]), and
//! the closure of a schema over them ([`close`]).
//!
//! [`FileResolver`] looks an `IMPORT` IRI up in the bodies given with the request, then
//! reads `file:` IRIs (under the [`FileLoads`] rules) and relative ones (against its
//! directories), then fetches http(s) IRIs through the server's [`OutboundPolicy`]. An
//! IRI that does not resolve as given is tried with `.shex`, then `.json` appended,
//! since shexTest imports name schemas without an extension. The text is ShExJ when it
//! starts with `{`, ShExC otherwise, and its own relative imports resolve against the
//! place it came from. [`ImportLimits`] caps how many schemas and bytes one closure
//! reads, and how long one fetch may take.

use crate::ast::{Label, Schema, ShapeDecl, ShapeExpr, TripleExpr};
use crate::error::SchemaError;
use anyhow::{anyhow, bail};
use sparkles::outbound::{OutboundPolicy, RequestBudget};
use sparkles::sparql::FileLoads;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

/// Supplies imported schemas and the definitions of EXTERNAL shapes.
pub trait Resolver: Send + Sync {
    /// The schema behind an `IMPORT` IRI; `None` if this resolver does not know it.
    fn import(&self, iri: &str) -> anyhow::Result<Option<Schema>>;

    /// The definition of an EXTERNAL shape, by its label's ShExJ form (the IRI, or
    /// `_:label`); `None` if this resolver does not know it.
    fn external(&self, label: &str) -> anyhow::Result<Option<ShapeExpr>> {
        let _ = label;
        Ok(None)
    }
}

/// A resolver that knows no imports and no external shapes.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoImports;

impl Resolver for NoImports {
    fn import(&self, _iri: &str) -> anyhow::Result<Option<Schema>> {
        Ok(None)
    }
}

/// The default of [`ImportLimits::max_schemas`].
pub const DEFAULT_MAX_IMPORTS: usize = 64;
/// The default of [`ImportLimits::max_bytes`]: 16 MiB.
pub const DEFAULT_MAX_IMPORT_BYTES: u64 = 16 << 20;
/// The default of [`ImportLimits::timeout`].
pub const DEFAULT_IMPORT_TIMEOUT: Duration = Duration::from_secs(10);

/// What one import closure may read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportLimits {
    /// imported schemas, counted once per IRI (inline bodies included)
    pub max_schemas: usize,
    /// bytes of schema text read from files and the network, together
    pub max_bytes: u64,
    /// time one http(s) fetch may take (also held to the outbound policy's timeout and
    /// the request's budget)
    pub timeout: Duration,
}

impl Default for ImportLimits {
    fn default() -> ImportLimits {
        ImportLimits {
            max_schemas: DEFAULT_MAX_IMPORTS,
            max_bytes: DEFAULT_MAX_IMPORT_BYTES,
            timeout: DEFAULT_IMPORT_TIMEOUT,
        }
    }
}

/// What a [`FileResolver`] has read so far, against its [`ImportLimits`].
#[derive(Debug, Default)]
pub struct ImportUsage {
    schemas: AtomicUsize,
    bytes: AtomicU64,
}

impl ImportUsage {
    pub fn schemas(&self) -> usize {
        self.schemas.load(Ordering::Relaxed)
    }

    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
}

/// Imports from inline bodies, then files, then http(s); external shapes from an externs
/// schema. An IRI that does not resolve as given is tried with `.shex`, then `.json`
/// appended. One resolver serves one request (or one command): its limits count
/// everything it reads.
#[derive(Default)]
pub struct FileResolver {
    /// directories relative IRIs resolve against (the importing schema's, on the
    /// command line)
    pub dirs: Vec<PathBuf>,
    /// import bodies given with the request, by IRI
    pub inline: HashMap<String, Schema>,
    /// which `file:` IRIs may be read
    pub files: FileLoads,
    /// http(s) imports, through this policy and request budget (`None`: no network)
    pub outbound: Option<(OutboundPolicy, Arc<RequestBudget>)>,
    /// the schema whose shapes define the EXTERNAL labels (`--externs`, the envelope's
    /// `externs`)
    pub externs: Option<Schema>,
    /// caps on what the imports may read
    pub limits: ImportLimits,
    /// what has been read so far
    pub used: ImportUsage,
}

/// The `Accept` header of http(s) imports.
const ACCEPT: &str = "text/shex, application/shex+json;q=0.9, application/json;q=0.5, */*;q=0.1";

impl Resolver for FileResolver {
    fn import(&self, iri: &str) -> anyhow::Result<Option<Schema>> {
        let n = self.used.schemas.fetch_add(1, Ordering::Relaxed) + 1;
        if n > self.limits.max_schemas {
            bail!(
                "import <{iri}>: the imports exceed the limit of {} schemas",
                self.limits.max_schemas
            );
        }
        let candidates = [
            iri.to_string(),
            format!("{iri}.shex"),
            format!("{iri}.json"),
        ];
        if let Some(s) = candidates.iter().find_map(|c| self.inline.get(c)) {
            return Ok(Some(s.clone()));
        }
        let scheme = oxiri::IriRef::parse(iri)
            .ok()
            .and_then(|i| i.scheme().map(str::to_ascii_lowercase));
        match scheme.as_deref() {
            Some("file") => self.read_files(iri, &candidates),
            Some("http" | "https") => self.fetch_web(iri, &candidates),
            Some(_) => Ok(None),
            None => {
                for dir in &self.dirs {
                    let urls = candidates.clone().map(|c| file_url(&dir.join(c)));
                    if let Some(s) = self.read_files(iri, &urls)? {
                        return Ok(Some(s));
                    }
                }
                Ok(None)
            }
        }
    }

    fn external(&self, label: &str) -> anyhow::Result<Option<ShapeExpr>> {
        Ok(self.externs.as_ref().and_then(|x| {
            x.shapes
                .iter()
                .find(|d| d.label.to_shexj() == label && !matches!(d.expr, ShapeExpr::External))
                .map(|d| d.expr.clone())
        }))
    }
}

impl FileResolver {
    /// The first of `urls` (`file:` URLs) that is a readable file.
    fn read_files(&self, iri: &str, urls: &[String]) -> anyhow::Result<Option<Schema>> {
        for url in urls {
            let path = match self.files.check(url) {
                Ok(p) => p,
                Err(sparkles::Error::NotPermitted(_)) => bail!(self.file_refusal(url)),
                // missing inside the load directory: try the next form
                Err(_) => continue,
            };
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            self.charge(iri, meta.len())?;
            let text = std::fs::read_to_string(&path)
                .map_err(|e| anyhow!("import <{iri}>: {}: {e}", path.display()))?;
            return parse(&text, url).map(Some);
        }
        Ok(None)
    }

    fn file_refusal(&self, url: &str) -> String {
        match &self.files {
            FileLoads::Disabled => {
                "import not allowed: file imports are not enabled (no load directory is \
                 configured)"
                    .to_string()
            }
            _ => format!("import not allowed: <{url}> is not a file in the load directory"),
        }
    }

    /// The first of `urls` that the outbound policy fetches without an error.
    fn fetch_web(&self, iri: &str, urls: &[String]) -> anyhow::Result<Option<Schema>> {
        let Some((policy, budget)) = &self.outbound else {
            bail!("import not allowed: <{iri}>: http(s) imports are not enabled");
        };
        let mut first_error = None;
        for url in urls {
            let mut policy = policy.clone();
            policy.timeout = policy.timeout.min(self.limits.timeout);
            let left = self.limits.max_bytes.saturating_sub(self.used.bytes());
            policy.max_response_bytes = policy.max_response_bytes.min(left);
            match sparkles::outbound::fetch_text(&policy, budget, url, ACCEPT) {
                Ok(text) => {
                    self.charge(iri, text.len() as u64)?;
                    return parse(&text, url).map(Some);
                }
                Err(sparkles::Error::NotPermitted(m)) => bail!("import not allowed: {m}"),
                Err(e @ sparkles::Error::BudgetExceeded(_)) => {
                    bail!("import <{iri}>: {e}")
                }
                // not there (an HTTP error, most likely 404): try the next form
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        match first_error {
            Some(e) => bail!("import <{iri}>: {e}"),
            None => Ok(None),
        }
    }

    /// Count `n` more bytes read for `iri`, or fail past the limit.
    fn charge(&self, iri: &str, n: u64) -> anyhow::Result<()> {
        let total = self.used.bytes.fetch_add(n, Ordering::Relaxed) + n;
        if total > self.limits.max_bytes {
            bail!(
                "import <{iri}>: the imports exceed the limit of {} bytes",
                self.limits.max_bytes
            );
        }
        Ok(())
    }
}

/// An imported schema's text: ShExJ when it starts with `{`, ShExC (with `url` as its
/// base) otherwise.
fn parse(text: &str, url: &str) -> anyhow::Result<Schema> {
    crate::parse_schema(text, Some(url), None).map_err(|e| anyhow!("import <{url}>: {e}"))
}

/// The `file:` URL of a path (made absolute against the working directory), with the
/// bytes an IRI path cannot hold percent-encoded.
pub fn file_url(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let mut url = String::from("file://");
    let s = abs.to_string_lossy();
    if !s.starts_with('/') {
        url.push('/');
    }
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                url.push(b as char)
            }
            b'\\' => url.push('/'),
            b':' | b'@' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';'
            | b'=' => url.push(b as char),
            _ => url.push_str(&format!("%{b:02X}")),
        }
    }
    url
}

/// The schema with its import closure merged in (each IRI fetched once, cycles ended,
/// imported `start`s ignored) and EXTERNAL shapes replaced by their definitions where
/// `resolver` has one. Overlapping labels and imported start actions are errors; the
/// same declaration reached twice (a schema importing itself, or the importing schema
/// imported back) is merged once.
pub fn close(schema: &Schema, resolver: &dyn Resolver) -> Result<Schema, SchemaError> {
    let external = schema
        .shapes
        .iter()
        .any(|d| matches!(d.expr, ShapeExpr::External));
    if schema.imports.is_empty() && !external {
        return Ok(schema.clone());
    }
    let mut out = schema.clone();
    // where each label was declared (`None`: the importing schema)
    let mut owner: HashMap<Label, (usize, Option<String>)> = out
        .shapes
        .iter()
        .enumerate()
        .map(|(i, d)| (d.label.clone(), (i, None)))
        .collect();
    let root = schema.base.as_deref().map(without_extension);
    let mut seen = HashSet::new();
    let mut queue: VecDeque<String> = schema.imports.iter().cloned().collect();
    while let Some(iri) = queue.pop_front() {
        if !seen.insert(iri.clone()) || root == Some(without_extension(&iri)) {
            continue;
        }
        let imported = resolver
            .import(&iri)
            .map_err(|e| {
                let m = format!("{e:#}");
                SchemaError::new(if m.starts_with("import") {
                    m
                } else {
                    format!("import <{iri}>: {m}")
                })
            })?
            .ok_or_else(|| SchemaError::new(format!("import <{iri}> could not be resolved")))?;
        if imported.shapes == schema.shapes && imported.start_acts == schema.start_acts {
            // the importing schema itself, under another name
            continue;
        }
        if !imported.start_acts.is_empty() {
            return Err(SchemaError::new(format!(
                "the imported schema <{iri}> has start actions"
            )));
        }
        for d in imported.shapes {
            match owner.get(&d.label) {
                Some((i, _)) if out.shapes[*i].expr == d.expr => {}
                Some((_, from)) => {
                    let first = from
                        .as_ref()
                        .map_or("the importing schema".to_string(), |f| format!("<{f}>"));
                    return Err(SchemaError::new(format!(
                        "{} is declared both in {first} and in the imported schema <{iri}>",
                        d.label
                    )));
                }
                None => {
                    owner.insert(d.label.clone(), (out.shapes.len(), Some(iri.clone())));
                    out.shapes.push(d);
                }
            }
        }
        queue.extend(imported.imports);
    }
    fill_externals(&mut out, resolver)?;
    Ok(out)
}

/// `iri` without a `.shex` or `.json` extension.
fn without_extension(iri: &str) -> &str {
    iri.strip_suffix(".shex")
        .or_else(|| iri.strip_suffix(".json"))
        .unwrap_or(iri)
}

/// Replace EXTERNAL declarations by the resolver's definitions, and declare the labels
/// those definitions reference that the schema lacks, when the resolver defines them
/// too. A label the resolver does not know stays EXTERNAL (the checks report it).
fn fill_externals(out: &mut Schema, resolver: &dyn Resolver) -> Result<(), SchemaError> {
    let lookup = |label: &Label| {
        resolver
            .external(&label.to_shexj())
            .map_err(|e| SchemaError::new(format!("external shape {label}: {e:#}")))
    };
    let mut filled = Vec::new();
    for (i, d) in out.shapes.iter_mut().enumerate() {
        if matches!(d.expr, ShapeExpr::External)
            && let Some(e) = lookup(&d.label)?
        {
            d.expr = e;
            filled.push(i);
        }
    }
    let mut declared: HashSet<Label> = out.shapes.iter().map(|d| d.label.clone()).collect();
    while let Some(i) = filled.pop() {
        let mut refs = Vec::new();
        shape_refs(&out.shapes[i].expr, &mut refs);
        for label in refs {
            if declared.contains(&label) {
                continue;
            }
            if let Some(e) = lookup(&label)? {
                declared.insert(label.clone());
                filled.push(out.shapes.len());
                out.shapes.push(ShapeDecl { label, expr: e });
            }
        }
    }
    Ok(())
}

/// The shape labels `e` references, value expressions of triple constraints included.
fn shape_refs(e: &ShapeExpr, out: &mut Vec<Label>) {
    match e {
        ShapeExpr::Or(v) | ShapeExpr::And(v) => v.iter().for_each(|x| shape_refs(x, out)),
        ShapeExpr::Not(x) => shape_refs(x, out),
        ShapeExpr::Ref(l) => out.push(l.clone()),
        ShapeExpr::Shape(s) => {
            if let Some(t) = &s.expression {
                triple_refs(t, out);
            }
        }
        ShapeExpr::Nc(_) | ShapeExpr::External => {}
    }
}

fn triple_refs(t: &TripleExpr, out: &mut Vec<Label>) {
    match t {
        TripleExpr::EachOf(g) | TripleExpr::OneOf(g) => {
            g.exprs.iter().for_each(|x| triple_refs(x, out))
        }
        TripleExpr::Tc(tc) => {
            if let Some(v) = &tc.value_expr {
                shape_refs(v, out);
            }
        }
        TripleExpr::Include(_) => {}
    }
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
