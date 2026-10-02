//! CSV and TSV to RDF (spec C05): a default mapping, CSVW metadata mappings and
//! Tarql-style CONSTRUCT templates.
//!
//! [`convert`] reads a table as a stream and hands each triple to a sink. [`write`]
//! serializes the triples, and [`to_ntriples_file`] writes them into a temporary
//! N-Triples file that the loader reads as an ordinary [`Source`](crate::io::Source).

pub mod csvw;
pub mod datatype;
pub mod template;
#[cfg(test)]
mod tests;
pub mod uritemplate;

pub use csvw::Metadata;
pub use template::Template;

use crate::error::{Budget, BudgetKind, Error, Result};
use crate::sparql::QueryOptions;
use csvw::{ColumnDesc, Props, TableDesc};
use datatype::Datatype;
use oxrdf::vocab::rdf;
use oxrdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple, Variable};
use oxrdfio::{RdfFormat, RdfSerializer};
use spargebra::term::GroundTerm;
use std::borrow::Cow;
use std::cell::Cell as StdCell;
use std::io::{Read, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use uritemplate::{UriTemplate, Value};

/// How a table becomes triples.
#[derive(Clone)]
pub enum Mapping {
    /// One subject per row (from `key`, or the row's position), one predicate per
    /// column under the namespace [`Options::base`] (spec C05 §4).
    Default { key: Option<String> },
    /// A CSVW metadata document (§3).
    Csvw(Arc<Metadata>),
    /// A CONSTRUCT template (§5), with a metadata document for the dialect, names and
    /// datatypes of the columns.
    Template {
        template: Arc<Template>,
        metadata: Option<Arc<Metadata>>,
    },
}

/// Limits of one conversion (§2.3).
#[derive(Clone, Debug)]
pub struct Limits {
    /// Bytes in one record. A longer one (an unclosed quote, usually) fails.
    pub max_record_bytes: u64,
    pub max_columns: usize,
    /// Bytes written by [`write`] and [`to_ntriples_file`].
    pub max_output_bytes: Option<u64>,
    /// Rows per evaluation of a template.
    pub batch_rows: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_record_bytes: 16 << 20,
            max_columns: 4096,
            max_output_bytes: None,
            batch_rows: 10_000,
        }
    }
}

/// A timeout or cancellation check, called every 65,536 rows.
pub type Check = Arc<dyn Fn() -> Result<()> + Send + Sync>;

#[derive(Clone)]
pub struct Options {
    pub mapping: Mapping,
    /// The default mapping's namespace, and the table URL of a mapping whose table has
    /// no `url`.
    pub base: Option<String>,
    /// The `file:` URL of the input, the last fallback for both.
    pub file_url: Option<String>,
    /// The input's name in messages, and the file name a table group's tables are
    /// matched by.
    pub name: String,
    /// Tab-separated (tabs, no quoting) unless the dialect says otherwise.
    pub tsv: bool,
    /// The table of the mapping to use (otherwise matched by file name).
    pub table: Option<usize>,
    pub limits: Limits,
    /// Budgets and timeout of template evaluation.
    pub query: QueryOptions,
    pub check: Option<Check>,
}

impl Options {
    pub fn new(mapping: Mapping, name: impl Into<String>) -> Options {
        Options {
            mapping,
            base: None,
            file_url: None,
            name: name.into(),
            tsv: false,
            table: None,
            limits: Limits::default(),
            query: QueryOptions::default(),
            check: None,
        }
    }

    /// Options for a file: its name, `file:` URL and whether it is TSV.
    pub fn for_file(mapping: Mapping, path: &Path) -> Options {
        let mut o = Options::new(mapping, path.display().to_string());
        o.file_url = Some(file_url(path));
        o.tsv = tabular_kind(path) == Some(TabularKind::Tsv);
        o
    }
}

/// What a conversion did.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub rows: u64,
    pub triples: u64,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabularKind {
    Csv,
    Tsv,
}

/// Whether a file name names a table: `.csv`, `.tsv` or `.tab`, before any compression
/// extension.
pub fn tabular_kind(path: &Path) -> Option<TabularKind> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let name = crate::codec::Codec::strip_extension(&name);
    if name.ends_with(".csv") {
        Some(TabularKind::Csv)
    } else if name.ends_with(".tsv") || name.ends_with(".tab") {
        Some(TabularKind::Tsv)
    } else {
        None
    }
}

/// Whether a media type is a table's: `text/csv` or `text/tab-separated-values`.
pub fn tabular_media_type(mt: &str) -> Option<TabularKind> {
    let base = mt.split(';').next()?.trim().to_ascii_lowercase();
    match base.as_str() {
        "text/csv" | "application/csv" => Some(TabularKind::Csv),
        "text/tab-separated-values" => Some(TabularKind::Tsv),
        _ => None,
    }
}

/// The `file:` URL of a path, made absolute, with the characters an IRI cannot hold
/// percent-encoded.
pub fn file_url(path: &Path) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|d| d.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    });
    let mut s = String::from("file://");
    for c in abs.to_string_lossy().chars() {
        if c.is_control()
            || matches!(
                c,
                ' ' | '"' | '<' | '>' | '\\' | '^' | '`' | '{' | '|' | '}' | '#' | '?' | '%'
            )
        {
            let mut b = [0u8; 4];
            for x in c.encode_utf8(&mut b).bytes() {
                s.push_str(&format!("%{x:02X}"));
            }
        } else {
            s.push(c);
        }
    }
    s
}

/// Open a (possibly compressed) table file, decompressing past `max_decompressed`
/// bytes as an error.
pub fn open(path: &Path, max_decompressed: Option<u64>) -> Result<Box<dyn Read + Send>> {
    let mut f = std::fs::File::open(path).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {e}", path.display()),
        ))
    })?;
    let (codec, head) = crate::io::sniff_codec(&mut f, Some(path), &path.display().to_string())?;
    let r = std::io::Cursor::new(head).chain(f);
    codec.reader_send(r, max_decompressed)
}

/// Read a mapping file: CSVW metadata located at `path`.
pub fn read_metadata(path: &Path) -> Result<Metadata> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {e}", path.display()),
        ))
    })?;
    csvw::parse(&text, Some(&file_url(path)))
        .map_err(|e| Error::invalid(format!("{}: {e}", path.display())))
}

/// Read a template file.
pub fn read_template(path: &Path) -> Result<Template> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {e}", path.display()),
        ))
    })?;
    Template::parse(&text, Some(&file_url(path))).map_err(|e| match e {
        Error::Invalid(m) => Error::invalid(format!("{}: {m}", path.display())),
        Error::SparqlSyntax(s) => Error::invalid(format!("{}: {s}", path.display())),
        e => e,
    })
}

// ----------------------------------------------------------------------- reading ----

/// The dialect in effect.
#[derive(Clone, Debug)]
struct Dialect {
    delimiter: u8,
    quote: Option<u8>,
    double_quote: bool,
    header_rows: usize,
    skip_rows: usize,
    skip_columns: usize,
    skip_blank_rows: bool,
    comment: Option<u8>,
    trim: (bool, bool),
}

impl Dialect {
    fn new(tsv: bool, d: &csvw::DialectDesc) -> Dialect {
        let header_rows = match (d.header, d.header_rows) {
            (Some(false), _) => 0,
            (_, Some(n)) => n,
            _ => 1,
        };
        let mut trim = d.trim.unwrap_or((true, true));
        if d.skip_initial_space == Some(true) {
            trim.0 = true;
        }
        Dialect {
            delimiter: d.delimiter.unwrap_or(if tsv { b'\t' } else { b',' }),
            quote: d.quote.unwrap_or(if tsv { None } else { Some(b'"') }),
            double_quote: d.double_quote.unwrap_or(true),
            header_rows,
            skip_rows: d.skip_rows.unwrap_or(0),
            skip_columns: d.skip_columns.unwrap_or(0),
            skip_blank_rows: d.skip_blank_rows.unwrap_or(false),
            comment: d.comment.unwrap_or(None),
            trim,
        }
    }

    fn trim<'a>(&self, s: &'a str) -> &'a str {
        let s = if self.trim.0 { s.trim_start() } else { s };
        if self.trim.1 { s.trim_end() } else { s }
    }
}

/// Counts the bytes the CSV reader takes, and fails a read once the current record has
/// grown past its limit, so that an unclosed quote cannot read a whole file into one
/// field.
struct Guard<R> {
    inner: R,
    read: Rc<StdCell<u64>>,
    stop_at: Rc<StdCell<u64>>,
    tripped: Rc<StdCell<bool>>,
}

impl<R: Read> Read for Guard<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.read.get() > self.stop_at.get() {
            self.tripped.set(true);
            return Err(std::io::Error::other("record too long"));
        }
        let n = self.inner.read(buf)?;
        self.read.set(self.read.get() + n as u64);
        Ok(n)
    }
}

/// Slack for the CSV reader's own buffer when checking a record's length.
const READ_SLACK: u64 = 256 << 10;

struct Records<R: Read> {
    rdr: csv::Reader<Guard<R>>,
    stop_at: Rc<StdCell<u64>>,
    tripped: Rc<StdCell<bool>>,
    max_record: u64,
    name: String,
    /// records read, including the header and skipped rows
    count: u64,
    first: bool,
}

impl<R: Read> Records<R> {
    fn new(input: R, d: &Dialect, opts: &Options) -> Records<R> {
        let read = Rc::new(StdCell::new(0));
        let stop_at = Rc::new(StdCell::new(opts.limits.max_record_bytes + READ_SLACK));
        let tripped = Rc::new(StdCell::new(false));
        let guard = Guard {
            inner: input,
            read,
            stop_at: stop_at.clone(),
            tripped: tripped.clone(),
        };
        let mut b = csv::ReaderBuilder::new();
        b.has_headers(false)
            .flexible(true)
            .delimiter(d.delimiter)
            .quoting(d.quote.is_some())
            .quote(d.quote.unwrap_or(b'"'))
            .double_quote(d.double_quote)
            .escape(if d.double_quote { None } else { Some(b'\\') })
            .comment(d.comment);
        Records {
            rdr: b.from_reader(guard),
            stop_at,
            tripped,
            max_record: opts.limits.max_record_bytes,
            name: opts.name.clone(),
            count: 0,
            first: true,
        }
    }

    /// The next record, or `None` at the end. Returns its source row and line.
    fn next(&mut self, rec: &mut csv::StringRecord) -> Result<Option<(u64, u64)>> {
        let start = self.rdr.position().byte();
        match self.rdr.read_record(rec) {
            Ok(false) => Ok(None),
            Ok(true) => {
                self.count += 1;
                let end = self.rdr.position().byte();
                if end - start > self.max_record {
                    return Err(self.too_long());
                }
                self.stop_at.set(end + self.max_record + READ_SLACK);
                if self.first {
                    self.first = false;
                    if rec.get(0).is_some_and(|f| f.starts_with('\u{feff}')) {
                        let fields: Vec<String> = rec
                            .iter()
                            .enumerate()
                            .map(|(i, f)| {
                                if i == 0 {
                                    f.trim_start_matches('\u{feff}').to_string()
                                } else {
                                    f.to_string()
                                }
                            })
                            .collect();
                        *rec = csv::StringRecord::from(fields);
                    }
                }
                let line = rec.position().map_or(self.count, |p| p.line());
                Ok(Some((self.count, line)))
            }
            Err(e) => {
                if self.tripped.get() {
                    return Err(self.too_long());
                }
                let row = self.count + 1;
                Err(match e.kind() {
                    csv::ErrorKind::Utf8 { pos, err } => Error::invalid(format!(
                        "{}: row {row}{}, field {}: the table is not UTF-8",
                        self.name,
                        line_note(row, pos.as_ref().map(|p| p.line())),
                        err.field() + 1
                    )),
                    csv::ErrorKind::Io(io) => Error::invalid(format!(
                        "{}: row {row}: cannot read the table: {io}",
                        self.name
                    )),
                    _ => Error::invalid(format!("{}: row {row}: {e}", self.name)),
                })
            }
        }
    }

    fn too_long(&self) -> Error {
        Error::invalid(format!(
            "{}: row {}: a record is longer than {}; an unclosed quote is the usual cause",
            self.name,
            self.count + 1,
            crate::error::human_bytes(self.max_record)
        ))
    }
}

fn line_note(row: u64, line: Option<u64>) -> String {
    match line {
        Some(l) if l != row => format!(" (line {l})"),
        _ => String::new(),
    }
}

// ----------------------------------------------------------------------- columns ----

/// A variable of a URI template, resolved against the table.
#[derive(Clone, Copy, Debug)]
enum Slot {
    Col(usize),
    Row,
    SourceRow,
    Column,
    SourceColumn,
    Name,
    Undef,
}

#[derive(Clone, Debug)]
struct Tpl {
    t: UriTemplate,
    slots: Vec<Slot>,
}

/// A column with its properties resolved.
#[derive(Clone, Debug)]
struct Column {
    name: String,
    /// the name, percent-decoded (`_name`)
    decoded: String,
    var: String,
    is_virtual: bool,
    suppress: bool,
    about: Option<Tpl>,
    property: Option<Tpl>,
    value: Option<Tpl>,
    datatype: Datatype,
    default: String,
    lang: Option<String>,
    null: Vec<String>,
    ordered: bool,
    required: bool,
    separator: Option<String>,
}

/// A parsed cell.
#[derive(Clone, Debug)]
enum Cell {
    Null,
    One(String),
    /// the items and the value before it was split
    List(Vec<String>, String),
}

/// A table ready to read rows: its columns and URL.
struct Table {
    columns: Vec<Column>,
    url: Option<oxiri::Iri<String>>,
    index: usize,
    skip_columns: usize,
}

fn compile(t: &UriTemplate, columns: &[ColumnDesc2]) -> Tpl {
    let slots = t
        .vars()
        .iter()
        .map(|v| match v.as_str() {
            "_row" => Slot::Row,
            "_sourceRow" => Slot::SourceRow,
            "_column" => Slot::Column,
            "_sourceColumn" => Slot::SourceColumn,
            "_name" => Slot::Name,
            v => columns
                .iter()
                .position(|c| !c.is_virtual && c.name == v)
                .map_or(Slot::Undef, Slot::Col),
        })
        .collect();
    Tpl {
        t: t.clone(),
        slots,
    }
}

/// The name and kind of a column, before its templates are compiled.
struct ColumnDesc2 {
    name: String,
    is_virtual: bool,
}

/// Build the columns of `desc` for a table whose header gave `titles` (one list per
/// cell) and whose rows have `width` cells.
fn columns(
    desc: &TableDesc,
    titles: &[Vec<String>],
    width: usize,
    header: bool,
    template_vars: bool,
    warnings: &mut Vec<String>,
) -> Result<Vec<Column>, String> {
    let from_header = desc.columns.is_empty();
    let descs: Vec<ColumnDesc> = if from_header {
        (0..width)
            .map(|i| ColumnDesc {
                name: None,
                titles: titles.get(i).cloned().unwrap_or_default(),
                ..Default::default()
            })
            .collect()
    } else {
        let real = desc.columns.iter().filter(|c| !c.is_virtual).count();
        if header && width != real {
            return Err(format!(
                "the header has {width} columns and the metadata describes {real}"
            ));
        }
        for (i, c) in desc.columns.iter().filter(|c| !c.is_virtual).enumerate() {
            if let Some(h) = titles.get(i).and_then(|t| t.first())
                && !c.titles.is_empty()
                && !c.titles.contains(h)
            {
                warnings.push(format!(
                    "column {}: the header {h:?} matches none of the titles of column {:?}",
                    i + 1,
                    c.name.as_deref().unwrap_or(&c.titles[0])
                ));
            }
        }
        desc.columns.clone()
    };
    let mut names: Vec<ColumnDesc2> = Vec::new();
    for (i, c) in descs.iter().enumerate() {
        let mut name = match (&c.name, c.titles.first()) {
            (Some(n), _) => n.clone(),
            (None, Some(t)) if !t.is_empty() => uritemplate::encode_name(t),
            _ => format!("_col.{}", i + 1),
        };
        // two header cells with the same title
        if names.iter().any(|n| n.name == name) {
            let mut k = 2;
            while names.iter().any(|n| n.name == format!("{name}_{k}")) {
                k += 1;
            }
            name = format!("{name}_{k}");
        }
        names.push(ColumnDesc2 {
            name,
            is_virtual: c.is_virtual,
        });
    }
    let mut out = Vec::new();
    let mut vars: Vec<String> = Vec::new();
    for (i, (c, n)) in descs.iter().zip(&names).enumerate() {
        let p: Props = c.props.over(&desc.props);
        let decoded = uritemplate::decode(&n.name);
        let mut var = if template_vars && !header && c.name.is_none() {
            template::letter_name(i)
        } else {
            template::var_name(
                c.titles
                    .first()
                    .filter(|_| c.name.is_none())
                    .unwrap_or(&decoded),
            )
        };
        if vars.contains(&var) || var == "ROWNUM" {
            let mut k = 2;
            while vars.contains(&format!("{var}_{k}")) {
                k += 1;
            }
            var = format!("{var}_{k}");
        }
        vars.push(var.clone());
        out.push(Column {
            name: n.name.clone(),
            decoded,
            var,
            is_virtual: c.is_virtual,
            suppress: c.suppress,
            about: p.about_url.as_ref().map(|t| compile(t, &names)),
            property: p.property_url.as_ref().map(|t| compile(t, &names)),
            value: p.value_url.as_ref().map(|t| compile(t, &names)),
            datatype: p.datatype.clone().unwrap_or_default(),
            default: p.default.clone().unwrap_or_default(),
            lang: p.lang.clone().flatten(),
            null: p.null.clone().unwrap_or_else(|| vec![String::new()]),
            ordered: p.ordered.unwrap_or(false),
            required: p.required.unwrap_or(false),
            separator: p.separator.clone().flatten(),
        });
    }
    Ok(out)
}

/// Collapse runs of whitespace into one space.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c == ' ' {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

impl Column {
    /// CSVW's cell parsing (§3.3).
    fn parse(&self, raw: &str) -> Result<Cell, String> {
        let dt = &self.datatype;
        let mut s: Cow<'_, str> = Cow::Borrowed(raw);
        if !dt.keeps_line_breaks() && s.contains(['\r', '\n', '\t']) {
            s = Cow::Owned(s.replace(['\r', '\n', '\t'], " "));
        }
        if !dt.keeps_spaces() {
            let t = s.trim_matches(' ');
            s = if t.contains("  ") {
                Cow::Owned(collapse(t))
            } else {
                Cow::Owned(t.to_string())
            };
        }
        if s.is_empty() {
            s = Cow::Owned(self.default.clone());
        }
        if let Some(sep) = &self.separator {
            if s.is_empty() || self.null.iter().any(|n| n == &*s) {
                if self.required {
                    return Err("the value is required".into());
                }
                return Ok(if s.is_empty() {
                    Cell::List(Vec::new(), String::new())
                } else {
                    Cell::Null
                });
            }
            let mut items = Vec::new();
            for item in s.split(sep.as_str()) {
                let item = if matches!(dt.kind, datatype::Kind::String | datatype::Kind::Any) {
                    item
                } else {
                    item.trim()
                };
                if self.null.iter().any(|n| n == item) {
                    continue;
                }
                items.push(dt.parse(item)?);
            }
            return Ok(Cell::List(items, s.into_owned()));
        }
        if self.null.iter().any(|n| n == &*s) {
            if self.required {
                return Err("the value is required".into());
            }
            return Ok(Cell::Null);
        }
        Ok(Cell::One(dt.parse(&s)?))
    }
}

// ------------------------------------------------------------------- conversion ----

/// The row being converted, for template expansion and messages.
struct RowCtx<'a> {
    cells: &'a [Cell],
    row: u64,
    source_row: u64,
    line: u64,
}

impl Table {
    fn value<'a>(&'a self, slot: Slot, r: &'a RowCtx<'a>, col: usize) -> Value<'a> {
        match slot {
            Slot::Col(i) => match &r.cells[i] {
                Cell::Null => Value::Undef,
                Cell::One(s) => Value::Str(Cow::Borrowed(s)),
                Cell::List(items, _) => Value::List(items),
            },
            Slot::Row => Value::Str(Cow::Owned(r.row.to_string())),
            Slot::SourceRow => Value::Str(Cow::Owned(r.source_row.to_string())),
            Slot::Column => Value::Str(Cow::Owned((col + 1).to_string())),
            Slot::SourceColumn => Value::Str(Cow::Owned((col + 1 + self.skip_columns).to_string())),
            Slot::Name => Value::Str(Cow::Borrowed(&self.columns[col].decoded)),
            Slot::Undef => Value::Undef,
        }
    }

    /// Expand a template into an absolute IRI.
    fn iri(&self, t: &Tpl, r: &RowCtx<'_>, col: usize) -> Result<NamedNode, String> {
        let s = t.t.expand(&|i| self.value(t.slots[i], r, col));
        let s = csvw::expand_prefixed(&s).unwrap_or(s);
        let abs = match oxiri::Iri::parse(s.as_str()) {
            Ok(_) => s,
            Err(_) => match &self.url {
                Some(base) => base
                    .resolve(&s)
                    .map_err(|e| format!("{s:?} from {:?} is not a valid IRI: {e}", t.t.as_str()))?
                    .into_inner(),
                None => {
                    return Err(format!(
                        "{s:?} from {:?} is relative and the table has no URL: give the table a url, or a base",
                        t.t.as_str()
                    ));
                }
            },
        };
        NamedNode::new(abs).map_err(|e| format!("{:?} gives an invalid IRI: {e}", t.t.as_str()))
    }
}

/// Convert a table, handing each triple to `sink`.
pub fn convert(
    input: impl Read,
    opts: &Options,
    sink: &mut dyn FnMut(Triple) -> Result<()>,
) -> Result<Stats> {
    let mut stats = Stats::default();
    let name = opts.name.clone();
    let fail = |m: String| Error::invalid(format!("{name}: {m}"));
    // the table's description
    let (desc, metadata, template, key) = match &opts.mapping {
        Mapping::Default { key } => {
            let ns = match (&opts.base, &opts.file_url) {
                (Some(b), _) => b.clone(),
                (None, Some(f)) => format!("{f}#"),
                (None, None) => {
                    return Err(fail(
                        "a table without a mapping needs a base IRI for its subjects and predicates".into(),
                    ));
                }
            };
            oxiri::Iri::parse(ns.as_str())
                .map_err(|e| fail(format!("the base {ns:?} is not an absolute IRI: {e}")))?;
            let props = Props {
                about_url: Some(
                    UriTemplate::parse(&format!("{ns}row={{_sourceRow}}")).map_err(fail)?,
                ),
                property_url: Some(UriTemplate::parse(&format!("{ns}{{_name}}")).map_err(fail)?),
                ..Default::default()
            };
            (
                TableDesc {
                    props,
                    ..Default::default()
                },
                None,
                None,
                key.clone().map(|k| (k, ns)),
            )
        }
        Mapping::Csvw(m) => (pick_table(m, opts)?.clone(), Some(m.clone()), None, None),
        Mapping::Template { template, metadata } => (
            match metadata {
                Some(m) => pick_table(m, opts)?.clone(),
                None => TableDesc::default(),
            },
            metadata.clone(),
            Some(template.clone()),
            None,
        ),
    };
    if let Some(m) = &metadata {
        stats.warnings.extend(m.warnings.iter().cloned());
    }
    if desc.suppress && template.is_none() {
        return Ok(stats);
    }
    let table_url = match desc
        .url
        .clone()
        .or_else(|| opts.base.clone())
        .or_else(|| opts.file_url.clone())
    {
        Some(u) => Some(
            oxiri::Iri::parse(u.clone())
                .map_err(|e| fail(format!("the table URL {u:?} is not an absolute IRI: {e}")))?,
        ),
        None => None,
    };
    let dialect = Dialect::new(opts.tsv, &desc.dialect);
    let mut recs = Records::new(input, &dialect, opts);
    let mut rec = csv::StringRecord::new();
    for _ in 0..dialect.skip_rows {
        if recs.next(&mut rec)?.is_none() {
            break;
        }
    }
    let mut titles: Vec<Vec<String>> = Vec::new();
    for _ in 0..dialect.header_rows {
        if recs.next(&mut rec)?.is_none() {
            break;
        }
        for (i, f) in rec.iter().skip(dialect.skip_columns).enumerate() {
            if titles.len() <= i {
                titles.push(Vec::new());
            }
            let t = dialect.trim(f);
            if !t.is_empty() {
                titles[i].push(t.to_string());
            }
        }
    }
    let header = dialect.header_rows > 0;
    // the first data row, to size a table without header or columns
    let mut pending = recs.next(&mut rec)?;
    let width = if header {
        titles.len()
    } else if !desc.columns.is_empty() {
        desc.columns.iter().filter(|c| !c.is_virtual).count()
    } else {
        pending.map_or(0, |_| rec.len().saturating_sub(dialect.skip_columns))
    };
    if width > opts.limits.max_columns {
        return Err(fail(format!(
            "the table has {width} columns, more than the limit of {}",
            opts.limits.max_columns
        )));
    }
    let mut cols = columns(
        &desc,
        &titles,
        width,
        header,
        template.is_some(),
        &mut stats.warnings,
    )
    .map_err(fail)?;
    if let Some((key, ns)) = &key {
        let i = cols
            .iter()
            .position(|c| &c.decoded == key || &c.name == key)
            .ok_or_else(|| {
                fail(format!(
                    "there is no column {key:?}; the columns are {}",
                    cols.iter()
                        .map(|c| format!("{:?}", c.decoded))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
        let names: Vec<ColumnDesc2> = cols
            .iter()
            .map(|c| ColumnDesc2 {
                name: c.name.clone(),
                is_virtual: c.is_virtual,
            })
            .collect();
        let about = compile(
            &UriTemplate::parse(&format!("{ns}{{{}}}", cols[i].name)).map_err(fail)?,
            &names,
        );
        for c in cols.iter_mut() {
            c.about = Some(about.clone());
        }
        cols[i].required = true;
    }
    let table = Table {
        columns: cols,
        url: table_url,
        index: opts.table.unwrap_or(0),
        skip_columns: dialect.skip_columns,
    };
    let mut runner = template
        .as_ref()
        .map(|t| template::Runner::new(t, &opts.query));
    let vars: Vec<Variable> = table
        .columns
        .iter()
        .filter(|c| !c.is_virtual)
        .map(|c| Variable::new_unchecked(c.var.clone()))
        .chain(std::iter::once(Variable::new_unchecked("ROWNUM")))
        .collect();
    let mut batch: Vec<Vec<Option<GroundTerm>>> = Vec::new();
    let mut batch_start = 1u64;
    let mut cells: Vec<Cell> = Vec::with_capacity(width);
    let mut row = 0u64;
    let mut emit = |t: Triple, stats: &mut Stats| -> Result<()> {
        stats.triples += 1;
        sink(t)
    };
    while let Some((source_row, line)) = pending {
        let fields = rec.iter().skip(dialect.skip_columns);
        if dialect.skip_blank_rows && rec.iter().all(|f| dialect.trim(f).is_empty()) {
            pending = recs.next(&mut rec)?;
            continue;
        }
        row += 1;
        if row.is_multiple_of(65_536)
            && let Some(c) = &opts.check
        {
            c()?;
        }
        let n = rec.len().saturating_sub(dialect.skip_columns);
        if n != width {
            return Err(fail(format!(
                "row {source_row}{} has {n} cells and the table has {width} columns",
                line_note(source_row, Some(line))
            )));
        }
        cells.clear();
        for (i, f) in fields.enumerate() {
            let col = &table.columns[i];
            let cell = col.parse(dialect.trim(f)).map_err(|m| {
                fail(format!(
                    "row {source_row}{}, column {} ({}): {m}",
                    line_note(source_row, Some(line)),
                    i + 1,
                    col.decoded
                ))
            })?;
            cells.push(cell);
        }
        let r = RowCtx {
            cells: &cells,
            row,
            source_row,
            line,
        };
        match &mut runner {
            None => emit_row(&table, &r, &name, &mut |t| emit(t, &mut stats))?,
            Some(run) => {
                let mut values = Vec::with_capacity(vars.len());
                for (i, c) in table
                    .columns
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| !c.is_virtual)
                {
                    values.push(bind(&table, c, i, &r).map_err(&fail)?);
                }
                values.push(Some(GroundTerm::Literal(Literal::from(row as i64))));
                batch.push(values);
                if batch.len() >= opts.limits.batch_rows.max(1) {
                    let rows = std::mem::take(&mut batch);
                    run_batch(run, &vars, rows, batch_start, row, &name, &mut |t| {
                        emit(t, &mut stats)
                    })?;
                    batch_start = row + 1;
                }
            }
        }
        pending = recs.next(&mut rec)?;
    }
    if let Some(run) = &mut runner
        && !batch.is_empty()
    {
        run_batch(run, &vars, batch, batch_start, row, &name, &mut |t| {
            emit(t, &mut stats)
        })?;
    }
    stats.rows = row;
    Ok(stats)
}

fn run_batch(
    run: &mut template::Runner<'_>,
    vars: &[Variable],
    rows: Vec<Vec<Option<GroundTerm>>>,
    first: u64,
    last: u64,
    name: &str,
    sink: &mut dyn FnMut(Triple) -> Result<()>,
) -> Result<()> {
    run.run(vars, rows, sink).map_err(|e| match e {
        Error::Invalid(m) => Error::invalid(format!("{name}: data rows {first} to {last}: {m}")),
        e => e,
    })?;
    Ok(())
}

/// The template binding of a cell.
fn bind(table: &Table, c: &Column, i: usize, r: &RowCtx<'_>) -> Result<Option<GroundTerm>, String> {
    Ok(match &r.cells[i] {
        Cell::Null => None,
        Cell::List(_, raw) => Some(GroundTerm::Literal(Literal::new_simple_literal(raw))),
        Cell::One(lex) => Some(match &c.value {
            Some(t) => GroundTerm::NamedNode(table.iri(t, r, i).map_err(|m| at(r, i, c, m))?),
            None => GroundTerm::Literal(c.datatype.literal(lex.clone(), c.lang.as_deref())),
        }),
    })
}

fn at(r: &RowCtx<'_>, i: usize, c: &Column, m: String) -> String {
    format!(
        "row {}{}, column {} ({}): {m}",
        r.source_row,
        line_note(r.source_row, Some(r.line)),
        i + 1,
        c.decoded
    )
}

/// The triples of one row in CSVW's minimal mode (§3.5).
fn emit_row(
    table: &Table,
    r: &RowCtx<'_>,
    name: &str,
    sink: &mut dyn FnMut(Triple) -> Result<()>,
) -> Result<()> {
    let row_node = || {
        NamedOrBlankNode::BlankNode(BlankNode::new_unchecked(format!(
            "t{}r{}",
            table.index, r.source_row
        )))
    };
    let out = |t: Triple, sink: &mut dyn FnMut(Triple) -> Result<()>| sink(t);
    for (i, c) in table.columns.iter().enumerate() {
        if c.suppress {
            continue;
        }
        let cell = if c.is_virtual {
            None
        } else {
            Some(&r.cells[i])
        };
        if matches!(cell, Some(Cell::Null))
            || matches!(cell, Some(Cell::List(l, _)) if l.is_empty())
        {
            continue;
        }
        let err = |m: String| Error::invalid(format!("{name}: {}", at(r, i, c, m)));
        let subject = match &c.about {
            Some(t) => NamedOrBlankNode::NamedNode(table.iri(t, r, i).map_err(err)?),
            None => row_node(),
        };
        let predicate = match &c.property {
            Some(t) => table.iri(t, r, i).map_err(err)?,
            None => {
                let Some(base) = &table.url else {
                    return Err(err(
                        "the column has no propertyUrl and the table has no URL to name its predicate".into(),
                    ));
                };
                let mut frag = String::new();
                for ch in c.decoded.chars() {
                    if ch.is_ascii_alphanumeric() || "-._~!$&'()*+,;=:@/?".contains(ch) {
                        frag.push(ch);
                    } else {
                        let mut b = [0u8; 4];
                        for x in ch.encode_utf8(&mut b).bytes() {
                            frag.push_str(&format!("%{x:02X}"));
                        }
                    }
                }
                let s = base.as_str();
                let s = s.split('#').next().unwrap_or(s);
                NamedNode::new(format!("{s}#{frag}")).map_err(|e| err(e.to_string()))?
            }
        };
        if let Some(t) = &c.value {
            let o = table.iri(t, r, i).map_err(err)?;
            out(Triple::new(subject, predicate, o), sink)?;
            continue;
        }
        match cell {
            Some(Cell::One(lex)) => {
                let o = c.datatype.literal(lex.clone(), c.lang.as_deref());
                out(Triple::new(subject, predicate, o), sink)?;
            }
            Some(Cell::List(items, _)) if c.ordered => {
                let node = |k: usize| {
                    BlankNode::new_unchecked(format!(
                        "t{}r{}c{}l{k}",
                        table.index,
                        r.source_row,
                        i + 1
                    ))
                };
                out(Triple::new(subject, predicate, node(0)), sink)?;
                for (k, item) in items.iter().enumerate() {
                    let o = c.datatype.literal(item.clone(), c.lang.as_deref());
                    out(Triple::new(node(k), rdf::FIRST, o), sink)?;
                    let rest: Term = if k + 1 == items.len() {
                        rdf::NIL.into_owned().into()
                    } else {
                        node(k + 1).into()
                    };
                    out(Triple::new(node(k), rdf::REST, rest), sink)?;
                }
            }
            Some(Cell::List(items, _)) => {
                for item in items {
                    let o = c.datatype.literal(item.clone(), c.lang.as_deref());
                    out(Triple::new(subject.clone(), predicate.clone(), o), sink)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn pick_table<'a>(m: &'a Metadata, opts: &Options) -> Result<&'a TableDesc> {
    if let Some(i) = opts.table {
        return m
            .tables
            .get(i)
            .ok_or_else(|| Error::invalid(format!("the mapping has no table {}", i + 1)));
    }
    if m.tables.len() == 1 {
        return Ok(&m.tables[0]);
    }
    let file = |s: &str| {
        let s = s.split(['?', '#']).next().unwrap_or(s);
        uritemplate::decode(s.rsplit('/').next().unwrap_or(s))
    };
    let mine = file(&opts.name.replace('\\', "/"));
    let mine = crate::codec::Codec::strip_extension(&mine).to_string();
    m.tables
        .iter()
        .find(|t| t.url_text.as_deref().is_some_and(|u| file(u) == mine))
        .ok_or_else(|| {
            Error::invalid(format!(
                "{}: the mapping has {} tables and none has the url {mine:?}",
                opts.name,
                m.tables.len()
            ))
        })
}

// ----------------------------------------------------------------------- output ----

/// A writer that fails past a byte limit.
struct Counting<W> {
    inner: W,
    n: u64,
    limit: Option<u64>,
}

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

const OUTPUT_LIMIT: &str = "sparkles: output limit";

/// The prefixes worth declaring in Turtle output of `mapping`.
pub fn prefixes(mapping: &Mapping) -> Vec<(String, String)> {
    let mut out = vec![
        (
            "rdf".to_string(),
            rdf::NIL.as_str().trim_end_matches("nil").to_string(),
        ),
        (
            "xsd".to_string(),
            "http://www.w3.org/2001/XMLSchema#".to_string(),
        ),
    ];
    let extra = match mapping {
        Mapping::Default { .. } => Vec::new(),
        Mapping::Csvw(m) => csvw::used_prefixes(m),
        Mapping::Template { template, metadata } => {
            let mut v = template.prefixes().to_vec();
            if let Some(m) = metadata {
                v.extend(csvw::used_prefixes(m));
            }
            v
        }
    };
    for (p, ns) in extra {
        if !out.iter().any(|(q, _)| *q == p) {
            out.push((p, ns));
        }
    }
    out
}

/// Convert a table and serialize the triples in `format`, into `graph` for a quad
/// format. [`Limits::max_output_bytes`] bounds the bytes written.
pub fn write(
    input: impl Read,
    opts: &Options,
    out: impl Write,
    format: RdfFormat,
    graph: Option<&NamedNode>,
) -> Result<Stats> {
    let limit = opts.limits.max_output_bytes;
    let ser = crate::io::with_prefixes(RdfSerializer::from_format(format), prefixes(&opts.mapping));
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
    let stats = convert(input, opts, &mut |t: Triple| {
        if quads && let Some(g) = graph {
            w.serialize_quad(t.as_ref().in_graph(g.as_ref()))
        } else {
            w.serialize_triple(&t)
        }
        .map_err(map_io)
    })?;
    let mut inner = w.finish().map_err(map_io)?;
    inner.flush().map_err(map_io)?;
    Ok(stats)
}

/// Convert a table into a temporary N-Triples file in `dir` (the system's temporary
/// directory by default), for the loader. The file is removed when the path is
/// dropped.
pub fn to_ntriples_file(
    input: impl Read,
    opts: &Options,
    dir: Option<&Path>,
) -> Result<(tempfile::TempPath, Stats)> {
    let mut b = tempfile::Builder::new();
    b.prefix("sparkles-csv-").suffix(".nt");
    let f = match dir {
        Some(d) => b.tempfile_in(d)?,
        None => b.tempfile()?,
    };
    let (file, path) = f.into_parts();
    let stats = write(
        input,
        opts,
        std::io::BufWriter::with_capacity(1 << 20, file),
        RdfFormat::NTriples,
        None,
    )?;
    Ok((path, stats))
}

/// An N-Triples file written by [`to_ntriples_file`] as a loader source, named `name`
/// in messages and loaded into `graph`.
pub fn source(path: &Path, graph: Option<NamedNode>, name: &str) -> crate::io::Source {
    let mut s = crate::io::Source::from_bytes(Vec::new(), RdfFormat::NTriples, graph);
    s.data = crate::io::SourceData::File(path.to_path_buf());
    s.name = name.to_string();
    s
}

/// The CSVW metadata of the default mapping for a table: its header, the namespace and
/// the key. A user starts a mapping file from it.
pub fn default_metadata(
    input: impl Read,
    opts: &Options,
    key: Option<&str>,
) -> Result<serde_json::Value> {
    let ns = match (&opts.base, &opts.file_url) {
        (Some(b), _) => b.clone(),
        (None, Some(f)) => format!("{f}#"),
        (None, None) => {
            return Err(Error::invalid(
                "a base IRI is needed for the default mapping",
            ));
        }
    };
    let dialect = Dialect::new(opts.tsv, &Default::default());
    let mut recs = Records::new(input, &dialect, opts);
    let mut rec = csv::StringRecord::new();
    if recs.next(&mut rec)?.is_none() {
        return Err(Error::invalid(format!("{}: the table is empty", opts.name)));
    }
    let mut names: Vec<String> = Vec::new();
    let mut cols = Vec::new();
    for (i, t) in rec.iter().enumerate() {
        let t = dialect.trim(t);
        let mut name = if t.is_empty() {
            format!("_col.{}", i + 1)
        } else {
            uritemplate::encode_name(t)
        };
        if names.contains(&name) {
            let mut k = 2;
            while names.contains(&format!("{name}_{k}")) {
                k += 1;
            }
            name = format!("{name}_{k}");
        }
        let mut c = serde_json::json!({
            "name": name,
            "titles": t,
            "datatype": "string",
            "propertyUrl": format!("{ns}{name}"),
        });
        if key.is_some_and(|k| k == t || k == name) {
            c["required"] = true.into();
        }
        names.push(name);
        cols.push(c);
    }
    let about = match key {
        Some(k) => {
            let name = names
                .iter()
                .zip(rec.iter())
                .find(|(n, t)| *n == k || dialect.trim(t) == k)
                .map(|(n, _)| n.clone())
                .ok_or_else(|| {
                    Error::invalid(format!("{}: there is no column {k:?}", opts.name))
                })?;
            format!("{ns}{{{name}}}")
        }
        None => format!("{ns}row={{_sourceRow}}"),
    };
    let mut m = serde_json::json!({
        "@context": csvw::CSVW_CONTEXT,
        "tableSchema": { "aboutUrl": about, "columns": cols },
    });
    if let Some(u) = &opts.file_url {
        m["url"] = u.clone().into();
    }
    if opts.tsv {
        m["dialect"] = serde_json::json!({ "delimiter": "\t", "quoteChar": null });
    }
    Ok(m)
}
