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
    /// The table group's `@id`, notes and common properties, for standard mode.
    pub group: Described,
    /// The `@language` that the metadata's `@context` sets: the language of the plain
    /// strings of notes and common properties.
    pub language: Option<String>,
}

/// What standard mode writes about a table group or a table besides its rows: its
/// `@id`, its `notes` and its common properties (spec F19 §6.1).
#[derive(Clone, Debug, Default)]
pub struct Described {
    pub id: Option<String>,
    pub notes: Vec<J>,
    /// common properties, by their name as written (`dc:title`, or an absolute IRI)
    pub common: Vec<(String, J)>,
}

/// A schema's foreign key (spec F19 §6.3).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ForeignKey {
    pub columns: Vec<String>,
    /// the referenced table's `url`, as written
    pub resource: Option<String>,
    /// the referenced schema's URL, as written
    pub schema_reference: Option<String>,
    pub ref_columns: Vec<String>,
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
    /// The schema's `primaryKey` column names.
    pub primary_key: Vec<String>,
    pub foreign_keys: Vec<ForeignKey>,
    /// the schema's `@id`, which a foreign key's `schemaReference` names
    pub schema_id: Option<String>,
    /// whether the table has a `tableSchema`, even an empty one. CSVW names the columns
    /// of a declared schema without columns by position, where the header's titles
    /// name the columns of a table without a schema.
    pub schema_declared: bool,
    /// The columns whose cells title the rows (`rowTitles`).
    pub row_titles: Vec<String>,
    pub described: Described,
}

#[derive(Clone, Debug, Default)]
pub struct ColumnDesc {
    pub name: Option<String>,
    pub titles: Vec<String>,
    /// The titles in the language `und`, which name a column without a `name`.
    pub und_titles: Vec<String>,
    pub is_virtual: bool,
    pub suppress: bool,
    /// The column's own inherited properties (not yet merged with the schema's).
    pub props: Props,
    /// The Sparkles lookup of §3.6: cell values and the IRIs that replace the `valueUrl`
    /// expansion for them. It is never inherited.
    pub lookup: Option<std::collections::HashMap<String, oxrdf::NamedNode>>,
}

/// The key of the lookup extension on a column description.
pub const LOOKUP_KEY: &str = "urn:x-sparkles:lookup";
/// The most entries of a table's lookups together.
pub const MAX_LOOKUP: usize = 10_000;

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
#[derive(Clone, Debug, Default, PartialEq)]
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

/// The `@id`, `notes` and common properties of an object.
fn described(o: &Map<String, J>, cx: &Ctx) -> Described {
    // an `@id` resolves against the metadata's base; one that is not a string is
    // treated as the empty string by CSVW's processing rules, which is the base itself
    let id = match o.get("@id") {
        Some(J::String(s)) => Some(s.as_str()),
        Some(_) if cx.lenient => Some(""),
        _ => None,
    }
    .map(|s| {
        let s = expand_prefixed(s).unwrap_or_else(|| s.to_string());
        match &cx.base {
            Some(b) => b.resolve(&s).map(|i| i.into_inner()).unwrap_or(s),
            None => s,
        }
    });
    Described {
        id,
        notes: match o.get("notes") {
            Some(J::Array(a)) => a.clone(),
            Some(v) => vec![v.clone()],
            None => Vec::new(),
        },
        common: o
            .iter()
            .filter(|(k, _)| {
                k.contains(':') && !k.starts_with("urn:x-sparkles:") && !k.starts_with('@')
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    }
}

/// A column reference: one name or an array of names.
fn column_names(v: &J, what: &str) -> Result<Vec<String>, String> {
    match v {
        J::String(s) => Ok(vec![s.clone()]),
        J::Array(a) if !a.is_empty() => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{what} must name columns by strings"))
            })
            .collect(),
        _ => Err(format!("{what} must be a column name or an array of them")),
    }
}

fn foreign_keys(v: &J, cx: &mut Ctx) -> Result<Vec<ForeignKey>, String> {
    let Some(a) = v.as_array() else {
        cx.soft::<()>(Err("foreignKeys must be an array".into()))?;
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for fk in a {
        let Some(o) = fk.as_object() else {
            cx.soft::<()>(Err("each foreign key must be an object".into()))?;
            continue;
        };
        if let Some(k) = o
            .keys()
            .find(|k| !matches!(k.as_str(), "columnReference" | "reference"))
        {
            return Err(format!(
                "a foreign key has the property {k:?}, which it cannot have"
            ));
        }
        let columns = column_names(
            o.get("columnReference")
                .ok_or("a foreign key needs a columnReference")?,
            "a foreign key's columnReference",
        )?;
        let r = o
            .get("reference")
            .and_then(J::as_object)
            .ok_or("a foreign key needs a reference object")?;
        if let Some(k) = r.keys().find(|k| {
            !matches!(
                k.as_str(),
                "resource" | "schemaReference" | "columnReference"
            )
        }) {
            return Err(format!(
                "a foreign key's reference has the property {k:?}, which it cannot have"
            ));
        }
        let ref_columns = column_names(
            r.get("columnReference")
                .ok_or("a foreign key's reference needs a columnReference")?,
            "a reference's columnReference",
        )?;
        if ref_columns.len() != columns.len() {
            return Err("a foreign key and its reference name different numbers of columns".into());
        }
        let resource = r.get("resource").and_then(J::as_str).map(str::to_string);
        let schema_reference = r
            .get("schemaReference")
            .and_then(J::as_str)
            .map(str::to_string);
        if resource.is_some() == schema_reference.is_some() {
            return Err(
                "a foreign key's reference needs one of resource and schemaReference".into(),
            );
        }
        out.push(ForeignKey {
            columns,
            resource,
            schema_reference,
            ref_columns,
        });
    }
    Ok(out)
}

/// Reads the document at a URL for a reference in the metadata, or gives `None`.
pub type Resolver<'a> = &'a dyn Fn(&str) -> Option<String>;

struct Ctx<'a> {
    resolve: Resolver<'a>,
    base: Option<oxiri::Iri<String>>,
    warnings: Vec<String>,
    /// CSVW's processing rules: a property with an invalid value is a warning and takes
    /// its default, where the strict reading fails
    lenient: bool,
}

impl Ctx<'_> {
    /// A property read: an invalid value is a warning and the type's default (nothing)
    /// in lenient mode, and an error otherwise.
    fn soft<T: Default>(&mut self, r: Result<T, String>) -> Result<T, String> {
        match r {
            Err(e) if self.lenient => {
                self.warn(e);
                Ok(T::default())
            }
            r => r,
        }
    }

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
            if k.starts_with("urn:x-sparkles:") {
                self.warn(format!(
                    "{what} property {k:?} is not a Sparkles extension that this version reads, and is ignored"
                ));
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

/// The `@id` and `@type` of a description object: an `@id` is not a blank node, and an
/// `@type` is the one of the kind of object.
fn check_id_type(o: &Map<String, J>, kind: &str, cx: &mut Ctx) -> Result<(), String> {
    match o.get("@id") {
        Some(J::String(s)) if s.starts_with("_:") => {
            return Err(format!("a {kind}'s @id {s:?} is a blank node"));
        }
        Some(J::String(_)) | None => {}
        Some(_) => cx.soft::<()>(Err(format!("a {kind}'s @id must be a string")))?,
    }
    match o.get("@type") {
        None => Ok(()),
        Some(J::String(t)) if t == kind => Ok(()),
        Some(t) => Err(format!("a {kind}'s @type is {t}, not {kind:?}")),
    }
}

/// Parse a metadata document. `location` is its URL (a `file:` URL on the CLI), against
/// which `url` resolves when the document has no `@base`.
pub fn parse(text: &str, location: Option<&str>) -> Result<Metadata, String> {
    parse_with(text, location, false)
}

/// Parse a metadata document by CSVW's processing rules when `lenient`: a property with
/// an invalid value becomes a warning and takes its default, as the CSVW specifications
/// ask, where [`parse`] fails.
pub fn parse_with(text: &str, location: Option<&str>, lenient: bool) -> Result<Metadata, String> {
    parse_resolving(text, location, lenient, &|_| None)
}

/// Like [`parse_with`], reading the schemas that the metadata names (a `tableSchema`
/// given as a URL) through `resolve`, which gets the absolute URL of each.
pub fn parse_resolving(
    text: &str,
    location: Option<&str>,
    lenient: bool,
    resolve: Resolver<'_>,
) -> Result<Metadata, String> {
    let v: J = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let o = v.as_object().ok_or("the metadata must be a JSON object")?;
    let mut base = match location {
        Some(l) => Some(oxiri::Iri::parse(l.to_string()).map_err(|e| format!("{l}: {e}"))?),
        None => None,
    };
    let mut language = None;
    let mut warnings = Vec::new();
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
                        ("@language", J::String(l)) => {
                            match oxrdf::Literal::new_language_tagged_literal("", l.as_str()) {
                                Ok(_) => language = Some(l.clone()),
                                Err(e) if lenient => {
                                    warnings.push(format!("@language {l:?} is ignored: {e}"))
                                }
                                Err(e) => return Err(format!("@language {l:?}: {e}")),
                            }
                        }
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
        resolve,
        base,
        warnings,
        lenient,
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
        check_id_type(o, "TableGroup", &mut cx)?;
        transformations(o, &mut cx)?;
        let props = props(o, &mut cx)?;
        let dialect = match o.get("dialect") {
            Some(d) => dialect(d, &mut cx)?,
            None => DialectDesc::default(),
        };
        for t in ts {
            let Some(t) = t.as_object() else {
                let e = Err("each table must be an object".to_string());
                cx.soft::<()>(e)?;
                continue;
            };
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
    for t in &tables {
        let n: usize = t
            .columns
            .iter()
            .filter_map(|c| c.lookup.as_ref())
            .map(|l| l.len())
            .sum();
        if n > MAX_LOOKUP {
            return Err(format!(
                "the lookups of a table hold {n} values; the limit is {MAX_LOOKUP}"
            ));
        }
    }
    let group = if o.contains_key("tables") {
        described(o, &cx)
    } else {
        Described::default()
    };
    Ok(Metadata {
        tables,
        warnings: cx.warnings,
        group,
        language,
    })
}

/// The transformations are not run, and their `@id` and `@type` are still checked.
fn transformations(o: &Map<String, J>, cx: &mut Ctx) -> Result<(), String> {
    if let Some(J::Array(a)) = o.get("transformations") {
        for t in a.iter().filter_map(J::as_object) {
            check_id_type(t, "Template", cx)?;
        }
    }
    Ok(())
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
    check_id_type(o, "Table", cx)?;
    transformations(o, cx)?;
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
    let suppress = cx.soft(bool_prop(o, "suppressOutput"))?.unwrap_or(false);
    let mut columns = Vec::new();
    let mut schema_props = Props::default();
    let mut primary_key = Vec::new();
    let mut fks = Vec::new();
    let mut schema_id = None;
    let mut row_titles = Vec::new();
    let schema_declared = o.contains_key("tableSchema");
    // a schema that the metadata names is read through the caller's resolver
    let referenced: Option<Map<String, J>> = match o.get("tableSchema") {
        Some(J::String(u)) => {
            let abs = match &cx.base {
                Some(b) => b.resolve(u).map(|i| i.into_inner()).ok(),
                None => Some(u.clone()),
            };
            let text = abs.as_deref().and_then(|a| (cx.resolve)(a));
            match (abs, text) {
                (Some(abs), Some(text)) => {
                    let v: J = serde_json::from_str(&text)
                        .map_err(|e| format!("the schema {u:?} is not JSON: {e}"))?;
                    let mut obj = match v {
                        J::Object(o) => o,
                        _ => return Err(format!("the schema {u:?} must be a JSON object")),
                    };
                    obj.entry("@id").or_insert(J::String(abs));
                    Some(obj)
                }
                _ => None,
            }
        }
        _ => None,
    };
    let schema_value: Option<J> = match referenced {
        Some(obj) => Some(J::Object(obj)),
        None => o.get("tableSchema").cloned(),
    };
    match schema_value.as_ref() {
        None => {}
        Some(J::Object(s)) => {
            cx.unknown(s, "schema", &["columns", "@context"]);
            check_id_type(s, "Schema", cx)?;
            schema_props = props(s, cx)?;
            if let Some(pk) = s.get("primaryKey") {
                primary_key = cx.soft(column_names(pk, "primaryKey"))?;
            }
            if let Some(f) = s.get("foreignKeys") {
                fks = foreign_keys(f, cx)?;
            }
            if let Some(r) = s.get("rowTitles") {
                row_titles = cx.soft(column_names(r, "rowTitles"))?;
            }
            schema_id = s.get("@id").and_then(J::as_str).map(str::to_string);
            if let Some(cs) = s.get("columns") {
                let cs = match cs.as_array() {
                    Some(a) => a.as_slice(),
                    None => {
                        cx.soft::<()>(Err("columns must be an array".into()))?;
                        &[]
                    }
                };
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
                "tableSchema {u:?} is a reference that could not be read: give the schema inline in the metadata"
            ));
        }
        Some(_) => cx.soft::<()>(Err("tableSchema must be an object".into()))?,
    }
    // a key names columns of the table by their `name` property
    if !columns.is_empty() {
        let named = |n: &String| columns.iter().any(|c| c.name.as_ref() == Some(n));
        let missing = |n: &String| {
            format!(
                "a key refers to the column {n:?}, which no column of the table has as its name"
            )
        };
        if let Some(n) = primary_key.iter().find(|n| !named(n)) {
            // a primary key is dropped in lenient mode, and a foreign key is an error
            cx.soft::<()>(Err(missing(n)))?;
            primary_key.clear();
        }
        if let Some(n) = fks
            .iter()
            .flat_map(|f| f.columns.iter())
            .find(|n| !named(n))
        {
            return Err(missing(n));
        }
    }
    Ok(TableDesc {
        url,
        url_text,
        columns,
        props: schema_props.over(&table_props),
        dialect,
        suppress,
        primary_key,
        foreign_keys: fks,
        schema_id,
        schema_declared,
        row_titles,
        described: described(o, cx),
    })
}

fn column(o: &Map<String, J>, cx: &mut Ctx) -> Result<ColumnDesc, String> {
    cx.unknown(
        o,
        "column",
        &["name", "titles", "virtual", "suppressOutput", LOOKUP_KEY],
    );
    check_id_type(o, "Column", cx)?;
    let name = cx.soft(match o.get("name") {
        None => Ok(None),
        Some(J::String(n)) if n.starts_with('_') => Err(format!(
            "column name {n:?} starts with '_', which CSVW reserves for template variables"
        )),
        Some(J::String(n)) if n.is_empty() => Err("a column name is empty".into()),
        Some(J::String(n)) if cx.lenient && !valid_variable(n) => Err(format!(
            "column name {n:?} is not a valid variable name of a URI template"
        )),
        Some(J::String(n)) => Ok(Some(n.clone())),
        Some(_) => Err("a column name must be a string".into()),
    })?;
    let mut titles = Vec::new();
    let mut und_titles = Vec::new();
    let mut title = |cx: &mut Ctx, v: &J, und: bool| -> Result<(), String> {
        match v.as_str() {
            Some(t) => {
                titles.push(t.to_string());
                if und {
                    und_titles.push(t.to_string());
                }
            }
            None => cx.soft::<()>(Err("titles must be strings".into()))?,
        }
        Ok(())
    };
    match o.get("titles") {
        None => {}
        Some(J::Array(a)) => {
            for t in a {
                title(cx, t, true)?;
            }
        }
        Some(J::Object(m)) => {
            for (lang, v) in m {
                match v {
                    J::Array(a) => {
                        for t in a {
                            title(cx, t, lang == "und")?;
                        }
                    }
                    v => title(cx, v, lang == "und")?,
                }
            }
        }
        Some(v @ J::String(_)) => title(cx, v, true)?,
        Some(_) => cx.soft::<()>(Err(
            "titles must be a string, an array or a language map".into()
        ))?,
    }
    let is_virtual = cx.soft(bool_prop(o, "virtual"))?.unwrap_or(false);
    let props = props(o, cx)?;
    if is_virtual && props.value_url.is_none() {
        let n = name.as_deref().unwrap_or("(unnamed)");
        return Err(format!("virtual column {n} has no valueUrl"));
    }
    let lookup = match o.get(LOOKUP_KEY) {
        None => None,
        Some(v) => {
            let n = name.as_deref().unwrap_or("(unnamed)");
            if is_virtual {
                return Err(format!("virtual column {n} cannot have a lookup"));
            }
            Some(lookup(v).map_err(|e| format!("column {n}: {LOOKUP_KEY}: {e}"))?)
        }
    };
    Ok(ColumnDesc {
        name,
        titles,
        und_titles,
        is_virtual,
        suppress: cx.soft(bool_prop(o, "suppressOutput"))?.unwrap_or(false),
        props,
        lookup,
    })
}

/// The entries of a lookup: `{"rdf:value": "text" or ["text", ...], "@id": "IRI"}`.
fn lookup(v: &J) -> Result<std::collections::HashMap<String, oxrdf::NamedNode>, String> {
    let entries = v
        .as_array()
        .ok_or("the lookup must be an array of entries")?;
    let mut out: std::collections::HashMap<String, oxrdf::NamedNode> =
        std::collections::HashMap::new();
    for e in entries {
        let e = e.as_object().ok_or("a lookup entry must be an object")?;
        let id = e
            .get("@id")
            .and_then(J::as_str)
            .ok_or("a lookup entry needs an @id")?;
        let iri = oxrdf::NamedNode::new(id)
            .map_err(|err| format!("the @id {id:?} is not an absolute IRI: {err}"))?;
        let value = e
            .get("rdf:value")
            .or_else(|| e.get("http://www.w3.org/1999/02/22-rdf-syntax-ns#value"))
            .ok_or("a lookup entry needs an rdf:value")?;
        let keys: Vec<&str> = match value {
            J::String(s) => vec![s.as_str()],
            J::Array(a) => a
                .iter()
                .map(|k| k.as_str().ok_or("an rdf:value must be a string or strings"))
                .collect::<Result<_, _>>()?,
            _ => return Err("an rdf:value must be a string or strings".into()),
        };
        for k in keys {
            match out.get(k) {
                Some(prev) if prev != &iri => {
                    return Err(format!("the value {k:?} names two IRIs"));
                }
                _ => {
                    out.insert(k.to_string(), iri.clone());
                }
            }
        }
    }
    Ok(out)
}

fn bool_prop(o: &Map<String, J>, k: &str) -> Result<Option<bool>, String> {
    match o.get(k) {
        None => Ok(None),
        Some(J::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{k} must be true or false")),
    }
}

fn template(o: &Map<String, J>, k: &str, cx: &mut Ctx) -> Result<Option<UriTemplate>, String> {
    match o.get(k) {
        None => Ok(None),
        Some(J::String(t)) => cx.soft(
            UriTemplate::parse(t)
                .map(Some)
                .map_err(|e| format!("{k}: {e}")),
        ),
        // the empty template, which expands to the table's own URL
        Some(_) => Ok(cx
            .soft::<Option<UriTemplate>>(Err(format!("{k} must be a string")))?
            .or_else(|| {
                cx.lenient
                    .then(|| UriTemplate::parse("").expect("the empty template parses"))
            })),
    }
}

fn props(o: &Map<String, J>, cx: &mut Ctx) -> Result<Props, String> {
    let about_url = template(o, "aboutUrl", cx)?;
    let property_url = template(o, "propertyUrl", cx)?;
    let value_url = template(o, "valueUrl", cx)?;
    let datatype = match o.get("datatype") {
        None => None,
        Some(d) if !d.is_string() && !d.is_object() => {
            cx.soft::<()>(Err("datatype must be a string or an object".into()))?;
            None
        }
        Some(d) => {
            // a description that is invalid beyond repair is an error in either mode
            let mut warnings = Vec::new();
            let r = Datatype::from_json_with(d, cx.lenient, &mut |w| warnings.push(w))
                .map_err(|e| format!("datatype: {e}"));
            for w in warnings {
                cx.warn(w);
            }
            Some(r?)
        }
    };
    let default = cx.soft(match o.get("default") {
        None => Ok(None),
        Some(J::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err("default must be a string".to_string()),
    })?;
    let lang = cx.soft(match o.get("lang") {
        None => Ok(None),
        Some(J::String(l)) if l == "und" => Ok(Some(None)),
        Some(J::String(l)) => oxrdf::Literal::new_language_tagged_literal("", l.as_str())
            .map(|_| Some(Some(l.clone())))
            .map_err(|e| format!("lang {l:?}: {e}")),
        Some(_) => Err("lang must be a string".to_string()),
    })?;
    let null = match o.get("null") {
        Some(J::Array(a)) => {
            // a value that is not a string is a warning, and the strings still count
            let out: Vec<String> = a.iter().filter_map(J::as_str).map(str::to_string).collect();
            if out.len() != a.len() {
                cx.soft::<()>(Err("null values must be strings".into()))?;
            }
            Some(out)
        }
        v => cx.soft(match v {
            None => Ok(None),
            Some(J::String(s)) => Ok(Some(vec![s.clone()])),
            Some(_) => Err("null must be a string or an array of strings".to_string()),
        })?,
    };
    let ordered = cx.soft(bool_prop(o, "ordered"))?;
    let required = cx.soft(bool_prop(o, "required"))?;
    let separator = cx.soft(match o.get("separator") {
        None => Ok(None),
        Some(J::Null) => Ok(Some(None)),
        Some(J::String(s)) if !s.is_empty() => Ok(Some(Some(s.clone()))),
        Some(_) => Err("separator must be a non-empty string or null".to_string()),
    })?;
    Ok(Props {
        about_url,
        property_url,
        value_url,
        datatype,
        default,
        lang,
        null,
        ordered,
        required,
        separator,
    })
}

/// Whether a column name is a variable of a URI template: letters, digits, underscores,
/// percent-encoded octets and dots.
fn valid_variable(n: &str) -> bool {
    let b = n.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            c if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c >= 0x80 => i += 1,
            b'%' if i + 2 < b.len()
                && b[i + 1].is_ascii_hexdigit()
                && b[i + 2].is_ascii_hexdigit() =>
            {
                i += 3
            }
            _ => return false,
        }
    }
    true
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
            cx.soft::<()>(Err(format!(
                "dialect {u:?} is a reference: give the dialect inline in the metadata"
            )))?;
            return Ok(DialectDesc::default());
        }
        _ => {
            cx.soft::<()>(Err("dialect must be an object".into()))?;
            return Ok(DialectDesc::default());
        }
    };
    check_id_type(o, "Dialect", cx)?;
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
            "delimiter" => d.delimiter = cx.soft(one_char(v, k).map(Some))?,
            "quoteChar" => {
                d.quote = cx.soft(match v {
                    J::Null => Ok(Some(None)),
                    v => one_char(v, k).map(|c| Some(Some(c))),
                })?
            }
            "commentPrefix" => {
                d.comment = cx.soft(match v {
                    J::Null => Ok(Some(None)),
                    v => one_char(v, k).map(|c| Some(Some(c))),
                })?
            }
            "doubleQuote" => d.double_quote = cx.soft(bool_prop(o, k))?,
            "header" => d.header = cx.soft(bool_prop(o, k))?,
            "headerRowCount" => d.header_rows = cx.soft(num(k))?,
            "skipRows" => d.skip_rows = cx.soft(num(k))?,
            "skipColumns" => d.skip_columns = cx.soft(num(k))?,
            "skipBlankRows" => d.skip_blank_rows = cx.soft(bool_prop(o, k))?,
            "skipInitialSpace" => d.skip_initial_space = cx.soft(bool_prop(o, k))?,
            "trim" => {
                d.trim = cx.soft(match v {
                    J::Bool(true) => Ok(Some((true, true))),
                    J::Bool(false) => Ok(Some((false, false))),
                    J::String(s) => match s.as_str() {
                        "true" => Ok(Some((true, true))),
                        "false" => Ok(Some((false, false))),
                        "start" => Ok(Some((true, false))),
                        "end" => Ok(Some((false, true))),
                        _ => Err("dialect trim must be true, false, start or end".to_string()),
                    },
                    _ => Err("dialect trim must be true, false, start or end".to_string()),
                })?
            }
            "encoding" => match v.as_str() {
                Some(e) if e.eq_ignore_ascii_case("utf-8") || e.eq_ignore_ascii_case("utf8") => {}
                _ => cx.soft::<()>(Err(format!(
                    "dialect encoding {v} is not supported: use utf-8"
                )))?,
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
