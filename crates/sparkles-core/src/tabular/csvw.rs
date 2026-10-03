//! CSVW metadata documents (the Metadata Vocabulary for Tabular Data): table groups,
//! tables, schemas, columns, inherited properties and dialects. Only the subset that
//! the RDF conversion needs is read; see spec C05 §3.

use super::datatype::Datatype;
use super::uritemplate::UriTemplate;
use serde_json::{Map, Value as J};

pub const CSVW_CONTEXT: &str = "http://www.w3.org/ns/csvw";

/// A metadata document: one table description, or the tables of a table group.
#[derive(Clone, Debug, Default)]
pub struct Metadata {
    pub tables: Vec<TableDesc>,
    /// Properties that were accepted and ignored, or not checked.
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct TableDesc {
    /// `url` resolved against the metadata's base, when it can be
    pub url: Option<String>,
    /// `url` as written
    pub url_text: Option<String>,
    /// The schema's columns; empty when the columns come from the header.
    pub columns: Vec<ColumnDesc>,
    /// The inherited properties in effect at the schema (group ← table ← schema).
    pub props: Props,
    pub dialect: DialectDesc,
    pub suppress: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ColumnDesc {
    pub name: Option<String>,
    pub titles: Vec<String>,
    pub is_virtual: bool,
    pub suppress: bool,
    /// The column's own inherited properties (not yet merged with the schema's).
    pub props: Props,
}

/// The inherited properties. `None` is "not set here".
#[derive(Clone, Debug, Default)]
pub struct Props {
    pub about_url: Option<UriTemplate>,
    pub property_url: Option<UriTemplate>,
    pub value_url: Option<UriTemplate>,
    pub datatype: Option<Datatype>,
    pub default: Option<String>,
    pub lang: Option<Option<String>>,
    pub null: Option<Vec<String>>,
    pub ordered: Option<bool>,
    pub required: Option<bool>,
    pub separator: Option<Option<String>>,
}

impl Props {
    /// These properties over `parent`'s: the nearest one wins.
    pub fn over(&self, parent: &Props) -> Props {
        Props {
            about_url: self.about_url.clone().or_else(|| parent.about_url.clone()),
            property_url: self
                .property_url
                .clone()
                .or_else(|| parent.property_url.clone()),
            value_url: self.value_url.clone().or_else(|| parent.value_url.clone()),
            datatype: self.datatype.clone().or_else(|| parent.datatype.clone()),
            default: self.default.clone().or_else(|| parent.default.clone()),
            lang: self.lang.clone().or_else(|| parent.lang.clone()),
            null: self.null.clone().or_else(|| parent.null.clone()),
            ordered: self.ordered.or(parent.ordered),
            required: self.required.or(parent.required),
            separator: self.separator.clone().or_else(|| parent.separator.clone()),
        }
    }
}

/// Dialect properties as given. `None` is "not set here".
#[derive(Clone, Debug, Default)]
pub struct DialectDesc {
    pub delimiter: Option<u8>,
    pub quote: Option<Option<u8>>,
    pub double_quote: Option<bool>,
    pub header: Option<bool>,
    pub header_rows: Option<usize>,
    pub skip_rows: Option<usize>,
    pub skip_columns: Option<usize>,
    pub skip_blank_rows: Option<bool>,
    pub skip_initial_space: Option<bool>,
    pub comment: Option<Option<u8>>,
    /// (start, end)
    pub trim: Option<(bool, bool)>,
}

impl DialectDesc {
    fn over(&self, parent: &DialectDesc) -> DialectDesc {
        DialectDesc {
            delimiter: self.delimiter.or(parent.delimiter),
            quote: self.quote.or(parent.quote),
            double_quote: self.double_quote.or(parent.double_quote),
            header: self.header.or(parent.header),
            header_rows: self.header_rows.or(parent.header_rows),
            skip_rows: self.skip_rows.or(parent.skip_rows),
            skip_columns: self.skip_columns.or(parent.skip_columns),
            skip_blank_rows: self.skip_blank_rows.or(parent.skip_blank_rows),
            skip_initial_space: self.skip_initial_space.or(parent.skip_initial_space),
            comment: self.comment.or(parent.comment),
            trim: self.trim.or(parent.trim),
        }
    }
}

const INHERITED: &[&str] = &[
    "aboutUrl",
    "propertyUrl",
    "valueUrl",
    "datatype",
    "default",
    "lang",
    "null",
    "ordered",
    "required",
    "separator",
    "textDirection",
];

/// Keys that are accepted without effect and without a warning: descriptive metadata.
const QUIET: &[&str] = &[
    "@id",
    "@type",
    "notes",
    "tableDirection",
    "rowTitles",
    "primaryKey",
    "foreignKeys",
];

struct Ctx {
    base: Option<oxiri::Iri<String>>,
    warnings: Vec<String>,
}

impl Ctx {
    fn warn(&mut self, w: String) {
        if !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }

    fn unknown(&mut self, o: &Map<String, J>, what: &str, known: &[&str]) {
        for k in o.keys() {
            if known.contains(&k.as_str())
                || QUIET.contains(&k.as_str())
                || INHERITED.contains(&k.as_str())
            {
                continue;
            }
            // common properties (`dc:title`, absolute IRIs) describe the table
            if k.contains(':') {
                continue;
            }
            self.warn(format!("{what} property {k:?} is ignored"));
        }
    }
}

/// Parse a metadata document. `location` is its URL (a `file:` URL on the CLI), against
/// which `url` resolves when the document has no `@base`.
pub fn parse(text: &str, location: Option<&str>) -> Result<Metadata, String> {
    let v: J = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let o = v.as_object().ok_or("the metadata must be a JSON object")?;
    let mut base = match location {
        Some(l) => Some(oxiri::Iri::parse(l.to_string()).map_err(|e| format!("{l}: {e}"))?),
        None => None,
    };
    match o.get("@context") {
        Some(J::String(s)) if is_csvw(s) => {}
        Some(J::Array(a)) if a.first().and_then(J::as_str).is_some_and(is_csvw) => {
            for extra in &a[1..] {
                let Some(e) = extra.as_object() else {
                    return Err("@context may only add an object with @base and @language".into());
                };
                for (k, v) in e {
                    match (k.as_str(), v) {
                        ("@base", J::String(b)) => {
                            base = Some(match &base {
                                Some(l) => l.resolve(b).map_err(|e| format!("@base {b:?}: {e}"))?,
                                None => oxiri::Iri::parse(b.clone())
                                    .map_err(|e| format!("@base {b:?}: {e}"))?,
                            });
                        }
                        ("@language", J::String(_)) => {}
                        _ => return Err(format!("@context key {k:?} is not supported")),
                    }
                }
            }
        }
        Some(_) => {
            return Err(format!(
                "@context must be {CSVW_CONTEXT:?}, or an array of it and an object with @base"
            ));
        }
        None => {
            return Err(format!(
                "the metadata has no @context (use {CSVW_CONTEXT:?})"
            ));
        }
    }
    let mut cx = Ctx {
        base,
        warnings: Vec::new(),
    };
    let mut tables = Vec::new();
    if let Some(ts) = o.get("tables") {
        let ts = ts.as_array().ok_or("tables must be an array")?;
        if ts.is_empty() {
            return Err("the table group has no tables".into());
        }
        cx.unknown(
            o,
            "table group",
            &["@context", "tables", "dialect", "transformations"],
        );
        let props = props(o, &mut cx)?;
        let dialect = match o.get("dialect") {
            Some(d) => dialect(d, &mut cx)?,
            None => DialectDesc::default(),
        };
        for t in ts {
            let t = t.as_object().ok_or("each table must be an object")?;
            tables.push(table(t, &props, &dialect, &mut cx)?);
        }
    } else {
        tables.push(table(
            o,
            &Props::default(),
            &DialectDesc::default(),
            &mut cx,
        )?);
    }
    if o.contains_key("transformations") {
        cx.warn("transformations are ignored".into());
    }
    Ok(Metadata {
        tables,
        warnings: cx.warnings,
    })
}

fn is_csvw(s: &str) -> bool {
    s == CSVW_CONTEXT || s == "http://www.w3.org/ns/csvw#"
}

fn table(
    o: &Map<String, J>,
    group: &Props,
    group_dialect: &DialectDesc,
    cx: &mut Ctx,
) -> Result<TableDesc, String> {
    cx.unknown(
        o,
        "table",
        &[
            "@context",
            "url",
            "tableSchema",
            "dialect",
            "suppressOutput",
            "transformations",
        ],
    );
    if o.contains_key("transformations") {
        cx.warn("transformations are ignored".into());
    }
    let (url, url_text) = match o.get("url") {
        None => (None, None),
        Some(J::String(u)) => {
            let abs = match &cx.base {
                Some(b) => Some(
                    b.resolve(u)
                        .map_err(|e| format!("table url {u:?}: {e}"))?
                        .into_inner(),
                ),
                None => oxiri::Iri::parse(u.clone()).ok().map(|i| i.into_inner()),
            };
            (abs, Some(u.clone()))
        }
        Some(_) => return Err("table url must be a string".into()),
    };
    let table_props = props(o, cx)?.over(group);
    let dialect = match o.get("dialect") {
        Some(d) => dialect(d, cx)?.over(group_dialect),
        None => group_dialect.clone(),
    };
    let suppress = bool_prop(o, "suppressOutput")?.unwrap_or(false);
    let mut columns = Vec::new();
    let mut schema_props = Props::default();
    match o.get("tableSchema") {
        None => {}
        Some(J::Object(s)) => {
            cx.unknown(s, "schema", &["columns", "@context"]);
            schema_props = props(s, cx)?;
            if let Some(cs) = s.get("columns") {
                let cs = cs.as_array().ok_or("columns must be an array")?;
                let mut seen_virtual = false;
                for c in cs {
                    let c = c.as_object().ok_or("each column must be an object")?;
                    let col = column(c, cx)?;
                    if col.is_virtual {
                        seen_virtual = true;
                    } else if seen_virtual {
                        return Err("virtual columns must come after the other columns".into());
                    }
                    if let Some(n) = &col.name
                        && columns
                            .iter()
                            .any(|x: &ColumnDesc| x.name.as_ref() == Some(n))
                    {
                        return Err(format!("two columns are named {n:?}"));
                    }
                    columns.push(col);
                }
            }
        }
        Some(J::String(u)) => {
            return Err(format!(
                "tableSchema {u:?} is a reference: give the schema inline in the metadata"
            ));
        }
        Some(_) => return Err("tableSchema must be an object".into()),
    }
    Ok(TableDesc {
        url,
        url_text,
        columns,
        props: schema_props.over(&table_props),
        dialect,
        suppress,
    })
}

fn column(o: &Map<String, J>, cx: &mut Ctx) -> Result<ColumnDesc, String> {
    cx.unknown(
        o,
        "column",
        &["name", "titles", "virtual", "suppressOutput"],
    );
    let name = match o.get("name") {
        None => None,
        Some(J::String(n)) if n.starts_with('_') => {
            return Err(format!(
                "column name {n:?} starts with '_', which CSVW reserves for template variables"
            ));
        }
        Some(J::String(n)) if n.is_empty() => return Err("a column name is empty".into()),
        Some(J::String(n)) => Some(n.clone()),
        Some(_) => return Err("a column name must be a string".into()),
    };
    let mut titles = Vec::new();
    match o.get("titles") {
        None => {}
        Some(J::String(t)) => titles.push(t.clone()),
        Some(J::Array(a)) => {
            for t in a {
                titles.push(t.as_str().ok_or("titles must be strings")?.to_string());
            }
        }
        Some(J::Object(m)) => {
            for v in m.values() {
                match v {
                    J::String(t) => titles.push(t.clone()),
                    J::Array(a) => {
                        for t in a {
                            titles.push(t.as_str().ok_or("titles must be strings")?.to_string());
                        }
                    }
                    _ => return Err("titles must be strings".into()),
                }
            }
        }
        Some(_) => return Err("titles must be a string, an array or a language map".into()),
    }
    let is_virtual = bool_prop(o, "virtual")?.unwrap_or(false);
    let props = props(o, cx)?;
    if is_virtual && props.value_url.is_none() {
        let n = name.as_deref().unwrap_or("(unnamed)");
        return Err(format!("virtual column {n} has no valueUrl"));
    }
    Ok(ColumnDesc {
        name,
        titles,
        is_virtual,
        suppress: bool_prop(o, "suppressOutput")?.unwrap_or(false),
        props,
    })
}

fn bool_prop(o: &Map<String, J>, k: &str) -> Result<Option<bool>, String> {
    match o.get(k) {
        None => Ok(None),
        Some(J::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{k} must be true or false")),
    }
}

fn template(o: &Map<String, J>, k: &str) -> Result<Option<UriTemplate>, String> {
    match o.get(k) {
        None => Ok(None),
        Some(J::String(t)) => UriTemplate::parse(t)
            .map(Some)
            .map_err(|e| format!("{k}: {e}")),
        Some(_) => Err(format!("{k} must be a string")),
    }
}

fn props(o: &Map<String, J>, cx: &mut Ctx) -> Result<Props, String> {
    let mut warn = |w: String| cx.warn(w);
    Ok(Props {
        about_url: template(o, "aboutUrl")?,
        property_url: template(o, "propertyUrl")?,
        value_url: template(o, "valueUrl")?,
        datatype: match o.get("datatype") {
            None => None,
            Some(d) => {
                Some(Datatype::from_json(d, &mut warn).map_err(|e| format!("datatype: {e}"))?)
            }
        },
        default: match o.get("default") {
            None => None,
            Some(J::String(s)) => Some(s.clone()),
            Some(_) => return Err("default must be a string".into()),
        },
        lang: match o.get("lang") {
            None => None,
            Some(J::String(l)) if l == "und" => Some(None),
            Some(J::String(l)) => {
                oxrdf::Literal::new_language_tagged_literal("", l.as_str())
                    .map_err(|e| format!("lang {l:?}: {e}"))?;
                Some(Some(l.clone()))
            }
            Some(_) => return Err("lang must be a string".into()),
        },
        null: match o.get("null") {
            None => None,
            Some(J::String(s)) => Some(vec![s.clone()]),
            Some(J::Array(a)) => Some(
                a.iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .ok_or("null values must be strings")
                    })
                    .collect::<Result<_, _>>()?,
            ),
            Some(_) => return Err("null must be a string or an array of strings".into()),
        },
        ordered: bool_prop(o, "ordered")?,
        required: bool_prop(o, "required")?,
        separator: match o.get("separator") {
            None => None,
            Some(J::Null) => Some(None),
            Some(J::String(s)) if !s.is_empty() => Some(Some(s.clone())),
            Some(_) => return Err("separator must be a non-empty string or null".into()),
        },
    })
}

fn one_char(v: &J, k: &str) -> Result<u8, String> {
    match v.as_str() {
        Some(s) if s.len() == 1 && s.is_ascii() => Ok(s.as_bytes()[0]),
        _ => Err(format!("dialect {k} must be one ASCII character")),
    }
}

fn dialect(v: &J, cx: &mut Ctx) -> Result<DialectDesc, String> {
    let o = match v {
        J::Object(o) => o,
        J::String(u) => {
            return Err(format!(
                "dialect {u:?} is a reference: give the dialect inline in the metadata"
            ));
        }
        _ => return Err("dialect must be an object".into()),
    };
    let mut d = DialectDesc::default();
    let num = |k: &str| -> Result<Option<usize>, String> {
        match o.get(k) {
            None => Ok(None),
            Some(v) => v
                .as_u64()
                .map(|n| Some(n as usize))
                .ok_or_else(|| format!("dialect {k} must be a non-negative integer")),
        }
    };
    for (k, v) in o {
        match k.as_str() {
            "delimiter" => d.delimiter = Some(one_char(v, k)?),
            "quoteChar" => {
                d.quote = Some(match v {
                    J::Null => None,
                    v => Some(one_char(v, k)?),
                })
            }
            "commentPrefix" => {
                d.comment = Some(match v {
                    J::Null => None,
                    v => Some(one_char(v, k)?),
                })
            }
            "doubleQuote" => d.double_quote = bool_prop(o, k)?,
            "header" => d.header = bool_prop(o, k)?,
            "headerRowCount" => d.header_rows = num(k)?,
            "skipRows" => d.skip_rows = num(k)?,
            "skipColumns" => d.skip_columns = num(k)?,
            "skipBlankRows" => d.skip_blank_rows = bool_prop(o, k)?,
            "skipInitialSpace" => d.skip_initial_space = bool_prop(o, k)?,
            "trim" => {
                d.trim = Some(match v {
                    J::Bool(true) => (true, true),
                    J::Bool(false) => (false, false),
                    J::String(s) => match s.as_str() {
                        "true" => (true, true),
                        "false" => (false, false),
                        "start" => (true, false),
                        "end" => (false, true),
                        _ => return Err("dialect trim must be true, false, start or end".into()),
                    },
                    _ => return Err("dialect trim must be true, false, start or end".into()),
                })
            }
            "encoding" => match v.as_str() {
                Some(e) if e.eq_ignore_ascii_case("utf-8") || e.eq_ignore_ascii_case("utf8") => {}
                _ => return Err(format!("dialect encoding {v} is not supported: use utf-8")),
            },
            "lineTerminators" | "@id" | "@type" => {}
            _ => cx.warn(format!("dialect property {k:?} is ignored")),
        }
    }
    Ok(d)
}

/// The prefixes of the CSVW initial context (the RDFa initial context), which expand
/// prefixed names in `propertyUrl` and `valueUrl`.
pub const PREFIXES: &[(&str, &str)] = &[
    ("as", "https://www.w3.org/ns/activitystreams#"),
    ("cc", "http://creativecommons.org/ns#"),
    ("csvw", "http://www.w3.org/ns/csvw#"),
    ("ctag", "http://commontag.org/ns#"),
    ("dc", "http://purl.org/dc/terms/"),
    ("dc11", "http://purl.org/dc/elements/1.1/"),
    ("dcat", "http://www.w3.org/ns/dcat#"),
    ("dcterms", "http://purl.org/dc/terms/"),
    ("dqv", "http://www.w3.org/ns/dqv#"),
    ("duv", "https://www.w3.org/ns/duv#"),
    ("foaf", "http://xmlns.com/foaf/0.1/"),
    ("gr", "http://purl.org/goodrelations/v1#"),
    ("grddl", "http://www.w3.org/2003/g/data-view#"),
    ("ical", "http://www.w3.org/2002/12/cal/icaltzd#"),
    ("jsonld", "http://www.w3.org/ns/json-ld#"),
    ("ldp", "http://www.w3.org/ns/ldp#"),
    ("ma", "http://www.w3.org/ns/ma-ont#"),
    ("oa", "http://www.w3.org/ns/oa#"),
    ("odrl", "http://www.w3.org/ns/odrl/2/"),
    ("og", "http://ogp.me/ns#"),
    ("org", "http://www.w3.org/ns/org#"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("prov", "http://www.w3.org/ns/prov#"),
    ("qb", "http://purl.org/linked-data/cube#"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfa", "http://www.w3.org/ns/rdfa#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("rev", "http://purl.org/stuff/rev#"),
    ("rif", "http://www.w3.org/2007/rif#"),
    ("rr", "http://www.w3.org/ns/r2rml#"),
    ("schema", "http://schema.org/"),
    ("sd", "http://www.w3.org/ns/sparql-service-description#"),
    ("sioc", "http://rdfs.org/sioc/ns#"),
    ("skos", "http://www.w3.org/2004/02/skos/core#"),
    ("skosxl", "http://www.w3.org/2008/05/skos-xl#"),
    ("sosa", "http://www.w3.org/ns/sosa/"),
    ("ssn", "http://www.w3.org/ns/ssn/"),
    ("time", "http://www.w3.org/2006/time#"),
    ("v", "http://rdf.data-vocabulary.org/#"),
    ("vcard", "http://www.w3.org/2006/vcard/ns#"),
    ("void", "http://rdfs.org/ns/void#"),
    ("wdr", "http://www.w3.org/2007/05/powder#"),
    ("wdrs", "http://www.w3.org/2007/05/powder-s#"),
    ("xhv", "http://www.w3.org/1999/xhtml/vocab#"),
    ("xml", "http://www.w3.org/XML/1998/namespace"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
];

/// `schema:name` → `http://schema.org/name`, for the prefixes of [`PREFIXES`].
pub fn expand_prefixed(s: &str) -> Option<String> {
    let (p, local) = s.split_once(':')?;
    if local.starts_with("//") {
        return None;
    }
    let (_, ns) = PREFIXES.iter().find(|(q, _)| *q == p)?;
    Some(format!("{ns}{local}"))
}

/// The prefixed names a metadata document uses in `propertyUrl` and `valueUrl`, as
/// `(prefix, namespace)` pairs, for Turtle output.
pub fn used_prefixes(m: &Metadata) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut add = |t: &Option<UriTemplate>| {
        if let Some(t) = t
            && let Some((p, _)) = t.as_str().split_once(':')
            && let Some((_, ns)) = PREFIXES.iter().find(|(q, _)| *q == p)
            && !out.iter().any(|(q, _)| q == p)
        {
            out.push((p.to_string(), ns.to_string()));
        }
    };
    for t in &m.tables {
        add(&t.props.property_url);
        add(&t.props.value_url);
        for c in &t.columns {
            add(&c.props.property_url);
            add(&c.props.value_url);
        }
    }
    out
}
