//! YARRRML (spec F19 §3.4): a YAML document translated into the mapping model the way
//! its specification describes the translation to RML. Unknown keys fail with their path
//! in the document, because YARRRML has no extension mechanism to ignore safely.

use super::CONNECTION_PREFIX;
use super::model::*;
use super::template::{Part, Template};
use crate::error::{Error, Result};
use crate::tabular::csvw::DialectDesc;
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{Literal, NamedNode, Term};
use yaml_rust2::{Yaml, YamlLoader};

/// Prefixes every YARRRML document has without declaring them.
const PREDEFINED: &[(&str, &str)] = &[
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("schema", "http://schema.org/"),
    ("foaf", "http://xmlns.com/foaf/0.1/"),
    ("dc", "http://purl.org/dc/elements/1.1/"),
    ("dcterms", "http://purl.org/dc/terms/"),
    ("grel", "http://users.ugent.be/~bjdmeest/function/grel.ttl#"),
    ("idlab-fn", "https://w3id.org/imec/idlab/function#"),
];

struct Cx {
    prefixes: Vec<(String, String)>,
    /// named sources: name and declaration
    sources: Vec<(String, SourceDecl)>,
}

fn err<T>(path: &str, m: impl std::fmt::Display) -> Result<T> {
    Err(Error::invalid(format!("YARRRML {path}: {m}")))
}

fn key(y: &Yaml) -> Option<&str> {
    match y {
        Yaml::String(s) => Some(s),
        _ => None,
    }
}

/// A scalar as text.
fn scalar(y: &Yaml) -> Option<String> {
    Some(match y {
        Yaml::String(s) => s.clone(),
        Yaml::Integer(i) => i.to_string(),
        Yaml::Real(r) => r.clone(),
        Yaml::Boolean(b) => b.to_string(),
        _ => return None,
    })
}

fn as_list(y: &Yaml) -> Vec<&Yaml> {
    match y {
        Yaml::Array(a) => a.iter().collect(),
        Yaml::Null | Yaml::BadValue => Vec::new(),
        other => vec![other],
    }
}

/// The entries of a hash, refusing keys outside `known` (aliases included).
fn entries<'a>(y: &'a Yaml, path: &str, known: &[&str]) -> Result<Vec<(&'a str, &'a Yaml)>> {
    let Yaml::Hash(h) = y else {
        return err(path, "expected a mapping of keys to values");
    };
    let mut out = Vec::new();
    for (k, v) in h {
        let Some(k) = key(k) else {
            return err(path, "a key is not a string");
        };
        if !known.contains(&k) {
            return err(
                &format!("{path}.{k}"),
                format!("unknown key; the keys here are {}", known.join(", ")),
            );
        }
        out.push((k, v));
    }
    Ok(out)
}

impl Cx {
    fn expand(&self, s: &str, path: &str) -> Result<String> {
        if s == "a" {
            return Ok(rdf::TYPE.as_str().to_string());
        }
        if let Some((p, local)) = s.split_once(':')
            && !local.starts_with("//")
            && let Some((_, ns)) = self.prefixes.iter().find(|(q, _)| q == p)
        {
            return Ok(format!("{ns}{local}"));
        }
        if oxiri::Iri::parse(s).is_ok() {
            return Ok(s.to_string());
        }
        err(
            path,
            format!("{s:?} is not an absolute IRI or a prefixed name of a declared prefix"),
        )
    }

    fn iri(&self, s: &str, path: &str) -> Result<NamedNode> {
        NamedNode::new(self.expand(s, path)?)
            .map_err(|e| Error::invalid(format!("YARRRML {path}: {e}")))
    }

    /// A value with `$(reference)` parts: a reference, a template or a constant string.
    /// In an IRI position, the constant part may be a prefixed name.
    fn value(&self, s: &str, iri: bool, path: &str) -> Result<Value> {
        let mut parts = Vec::new();
        let mut cur = String::new();
        let mut rest = s;
        while let Some(i) = rest.find("$(") {
            // `\$(` keeps the text
            if i > 0 && rest.as_bytes()[i - 1] == b'\\' {
                cur.push_str(&rest[..i - 1]);
                cur.push_str("$(");
                rest = &rest[i + 2..];
                continue;
            }
            cur.push_str(&rest[..i]);
            let after = &rest[i + 2..];
            // references may hold parentheses, as in JSONPath filters: match them
            let mut depth = 1;
            let mut end = None;
            for (k, c) in after.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(k);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(end) = end else {
                return err(path, format!("{s:?} has an unclosed $("));
            };
            if !cur.is_empty() {
                parts.push(Part::Text(std::mem::take(&mut cur)));
            }
            parts.push(Part::Ref(after[..end].to_string()));
            rest = &after[end + 1..];
        }
        cur.push_str(rest);
        if !cur.is_empty() {
            parts.push(Part::Text(cur));
        }
        match parts.as_slice() {
            [Part::Ref(r)] => Ok(Value::Reference(r.clone())),
            [] => Ok(Value::Constant(Term::Literal(Literal::new_simple_literal(
                "",
            )))),
            [Part::Text(t)] => Ok(Value::Constant(if iri {
                Term::NamedNode(self.iri(t, path)?)
            } else {
                Term::Literal(Literal::new_simple_literal(t.clone()))
            })),
            _ => {
                // a prefixed name at the start of an IRI template
                if iri
                    && let Some(Part::Text(first)) = parts.first_mut()
                    && let Some((p, local)) = first.clone().split_once(':')
                    && !local.starts_with("//")
                    && let Some((_, ns)) = self.prefixes.iter().find(|(q, _)| q == p)
                {
                    *first = format!("{ns}{local}");
                }
                let text: String = parts
                    .iter()
                    .map(|p| match p {
                        Part::Text(t) => Template::escape_text(t),
                        Part::Ref(r) => format!("{{{r}}}"),
                    })
                    .collect();
                Ok(Value::Template(Template::parse(&text).map_err(|e| {
                    Error::invalid(format!("YARRRML {path}: {e}"))
                })?))
            }
        }
    }

    /// A source: the `[access~formulation, iterator]` shorthand, a long form, or the name
    /// of a declared source.
    fn source(&self, y: &Yaml, path: &str) -> Result<SourceDecl> {
        match y {
            Yaml::String(name) if !name.contains('~') => {
                if let Some((_, d)) = self.sources.iter().find(|(n, _)| n == name) {
                    return Ok(d.clone());
                }
                file_decl(name, None, None, path)
            }
            Yaml::String(s) => {
                let (access, f) = s.rsplit_once('~').expect("has ~");
                file_decl(access, Some(f), None, path)
            }
            Yaml::Array(a) => {
                let first = a.first().and_then(scalar).ok_or_else(|| {
                    Error::invalid(format!("YARRRML {path}: a source needs its access"))
                })?;
                let iterator = a.get(1).and_then(scalar);
                if a.len() > 2 {
                    return err(
                        path,
                        "a source shorthand has at most an access and an iterator",
                    );
                }
                let (access, f) = match first.rsplit_once('~') {
                    Some((a, f)) => (a.to_string(), Some(f.to_string())),
                    None => (first.clone(), None),
                };
                if let Some(conn) = access.strip_prefix(CONNECTION_PREFIX) {
                    let q = iterator.ok_or_else(|| {
                        Error::invalid(format!(
                            "YARRRML {path}: a database source needs a query or table"
                        ))
                    })?;
                    return Ok(sql(conn, &q, f.as_deref()));
                }
                file_decl(&access, f.as_deref(), iterator, path)
            }
            Yaml::Hash(_) => {
                let mut access = None;
                let mut formulation = None;
                let mut iterator = None;
                let mut query = None;
                let mut delimiter = None;
                for (k, v) in entries(
                    y,
                    path,
                    &[
                        "access",
                        "referenceFormulation",
                        "iterator",
                        "query",
                        "queryFormulation",
                        "type",
                        "credentials",
                        "delimiter",
                        "encoding",
                    ],
                )? {
                    let p = format!("{path}.{k}");
                    match k {
                        "access" => access = scalar(v),
                        "referenceFormulation" => formulation = scalar(v),
                        "iterator" => iterator = scalar(v),
                        "query" => query = scalar(v),
                        "queryFormulation" | "type" => {}
                        "delimiter" => delimiter = scalar(v),
                        "encoding" => {
                            if !scalar(v).is_some_and(|e| e.eq_ignore_ascii_case("utf-8")) {
                                return err(&p, "sources must be UTF-8");
                            }
                        }
                        "credentials" => {
                            return err(
                                &p,
                                "credentials are refused: declare a connection on the server or the command line, and name it with access: urn:x-sparkles:connection:NAME",
                            );
                        }
                        _ => unreachable!("checked"),
                    }
                }
                let access = access.ok_or_else(|| {
                    Error::invalid(format!("YARRRML {path}: a source needs access"))
                })?;
                if let Some(name) = access.strip_prefix(CONNECTION_PREFIX) {
                    if let Some(q) = query {
                        return Ok(sql(name, &q, Some("query")));
                    }
                    let t = iterator.ok_or_else(|| {
                        Error::invalid(format!("YARRRML {path}: a database source needs a query, or a table as its iterator"))
                    })?;
                    return Ok(sql(name, &t, Some("table")));
                }
                if query.is_some() {
                    return err(
                        path,
                        "a query needs access to a database connection (urn:x-sparkles:connection:NAME)",
                    );
                }
                let mut d = file_decl(&access, formulation.as_deref(), iterator, path)?;
                if let (Some(del), SourceKind::Csv { dialect, .. }) = (delimiter, &mut d.kind) {
                    match del.as_bytes() {
                        [b] => dialect.delimiter = Some(*b),
                        _ => return err(path, "the delimiter must be one ASCII character"),
                    }
                }
                Ok(d)
            }
            _ => err(path, "a source must be a string, a list or a mapping"),
        }
    }
}

fn sql(connection: &str, q: &str, how: Option<&str>) -> SourceDecl {
    let looks_like_query = q.trim_start().to_ascii_uppercase().starts_with("SELECT")
        || q.trim_start().to_ascii_uppercase().starts_with("WITH");
    let query = match how {
        Some("table") => SqlQuery::Table(q.to_string()),
        Some("query") => SqlQuery::Query(q.to_string()),
        _ if looks_like_query => SqlQuery::Query(q.to_string()),
        _ => SqlQuery::Table(q.to_string()),
    };
    SourceDecl {
        kind: SourceKind::Sql {
            connection: connection.to_string(),
            query,
        },
        iterator: None,
    }
}

fn file_decl(
    access: &str,
    formulation: Option<&str>,
    iterator: Option<String>,
    path: &str,
) -> Result<SourceDecl> {
    let lower = access.to_ascii_lowercase();
    let lower = crate::codec::Codec::strip_extension(&lower).to_string();
    let file = access.to_string();
    let kind = match formulation.map(|f| f.to_ascii_lowercase()) {
        Some(f) if f == "csv" => SourceKind::Csv {
            tsv: lower.ends_with(".tsv") || lower.ends_with(".tab"),
            file,
            dialect: DialectDesc::default(),
            null: Vec::new(),
        },
        Some(f) if f == "tsv" => SourceKind::Csv {
            tsv: true,
            file,
            dialect: DialectDesc::default(),
            null: Vec::new(),
        },
        Some(f) if f == "jsonpath" || f == "json" => SourceKind::Json {
            lines: lower.ends_with(".jsonl") || lower.ends_with(".ndjson"),
            file,
        },
        Some(f) if f == "jsonl" || f == "jsonlines" => SourceKind::Json { lines: true, file },
        Some(f) if f == "parquet" => SourceKind::Parquet { file },
        Some(f) if f == "xpath" || f == "xml" => {
            return err(
                path,
                "XML sources and XPath iterators are not supported (spec F19, open question 1)",
            );
        }
        Some(f) => {
            return err(
                path,
                format!("the reference formulation {f:?} is not csv, jsonpath, jsonl or parquet"),
            );
        }
        None if lower.ends_with(".csv") => SourceKind::Csv {
            tsv: false,
            file,
            dialect: DialectDesc::default(),
            null: Vec::new(),
        },
        None if lower.ends_with(".tsv") || lower.ends_with(".tab") => SourceKind::Csv {
            tsv: true,
            file,
            dialect: DialectDesc::default(),
            null: Vec::new(),
        },
        None if lower.ends_with(".jsonl") || lower.ends_with(".ndjson") => {
            SourceKind::Json { lines: true, file }
        }
        None if lower.ends_with(".json") => SourceKind::Json { lines: false, file },
        None if lower.ends_with(".parquet") => SourceKind::Parquet { file },
        None => {
            return err(
                path,
                format!(
                    "the source {access:?} names no reference formulation (~csv, ~jsonpath) and its extension says none"
                ),
            );
        }
    };
    let iterator = match &kind {
        SourceKind::Json { .. } => Some(iterator.unwrap_or_else(|| "$".into())),
        _ => None,
    };
    Ok(SourceDecl { kind, iterator })
}

/// One translated mapping before its joins are resolved: the triples maps of each
/// (source, subject) pair, and the joins of its objects by mapping name.
/// A join by name: the parent mapping and the (child, parent) references.
type NamedJoin = (String, Vec<(String, String)>);

struct Pending {
    name: String,
    maps: Vec<TriplesMap>,
    /// for each map, for each predicate-object, for each object: a join by name
    joins: Vec<(usize, usize, NamedJoin)>,
}

/// Parse a YARRRML document.
pub fn parse(text: &str, _base: Option<&str>) -> Result<Mapping> {
    let docs = YamlLoader::load_from_str(text)
        .map_err(|e| Error::invalid(format!("YARRRML is not valid YAML: {e}")))?;
    let [doc] = docs.as_slice() else {
        return Err(Error::invalid("YARRRML must be one YAML document"));
    };
    let top = entries(
        doc,
        "$",
        &[
            "prefixes", "base", "sources", "mappings", "authors", "targets", "external",
        ],
    )?;
    let mut cx = Cx {
        prefixes: PREDEFINED
            .iter()
            .map(|(p, n)| (p.to_string(), n.to_string()))
            .collect(),
        sources: Vec::new(),
    };
    let mut base = None;
    let mut warnings = Vec::new();
    let mut declared: Vec<(String, String)> = Vec::new();
    for (k, v) in &top {
        match *k {
            "prefixes" => {
                for (p, ns) in entries(v, "$.prefixes", &[]).or_else(|_| {
                    // every key is allowed here
                    match v {
                        Yaml::Hash(h) => {
                            Ok(h.iter().filter_map(|(k, v)| Some((key(k)?, v))).collect())
                        }
                        _ => err("$.prefixes", "expected a mapping of prefixes to namespaces"),
                    }
                })? {
                    let ns = scalar(ns).ok_or_else(|| {
                        Error::invalid(format!("YARRRML $.prefixes.{p}: not a string"))
                    })?;
                    cx.prefixes.retain(|(q, _)| q != p);
                    cx.prefixes.push((p.to_string(), ns.clone()));
                    declared.push((p.to_string(), ns));
                }
            }
            "base" => base = scalar(v),
            "authors" => {}
            "targets" => warnings.push(
                "YARRRML targets are ignored; the target is the dataset or the output file"
                    .to_string(),
            ),
            "external" => return err("$.external", "external references are not supported"),
            _ => {}
        }
    }
    if let Some((_, v)) = top.iter().find(|(k, _)| *k == "sources") {
        let Yaml::Hash(h) = v else {
            return err("$.sources", "expected a mapping of source names to sources");
        };
        for (k, v) in h {
            let Some(name) = key(k) else {
                return err("$.sources", "a source name is not a string");
            };
            let d = cx.source(v, &format!("$.sources.{name}"))?;
            cx.sources.push((name.to_string(), d));
        }
    }
    let Some((_, mappings)) = top.iter().find(|(k, _)| *k == "mappings") else {
        return err("$", "the document has no mappings");
    };
    let Yaml::Hash(mh) = mappings else {
        return err("$.mappings", "expected a mapping of names to mappings");
    };
    let mut sources: Vec<SourceDecl> = Vec::new();
    let mut pending: Vec<Pending> = Vec::new();
    for (k, v) in mh {
        let Some(name) = key(k) else {
            return err("$.mappings", "a mapping name is not a string");
        };
        pending.push(mapping(&cx, name, v, &mut sources)?);
    }
    // resolve the joins by mapping name: one referencing object map per parent map
    let mut offsets = Vec::new();
    let mut total = 0;
    for p in &pending {
        offsets.push(total);
        total += p.maps.len();
    }
    let mut maps: Vec<TriplesMap> = Vec::new();
    for (pi, p) in pending.iter().enumerate() {
        for (mi, m) in p.maps.iter().enumerate() {
            let mut m = m.clone();
            for (jm, jpo, (target, conds)) in &p.joins {
                if *jm != mi {
                    continue;
                }
                let Some(ti) = pending.iter().position(|q| q.name == *target) else {
                    return err(
                        &format!("$.mappings.{}", p.name),
                        format!("the object names the mapping {target:?}, which does not exist"),
                    );
                };
                for k in 0..pending[ti].maps.len() {
                    m.predicate_objects[*jpo].objects.push(ObjectMap::Join {
                        parent: offsets[ti] + k,
                        conditions: conds.clone(),
                    });
                }
            }
            let _ = pi;
            maps.push(m);
        }
    }
    Ok(Mapping {
        maps,
        sources,
        base,
        vocabulary: Vocabulary::Yarrrml,
        warnings,
        prefixes: declared,
    })
}

fn mapping(cx: &Cx, name: &str, y: &Yaml, sources: &mut Vec<SourceDecl>) -> Result<Pending> {
    let path = format!("$.mappings.{name}");
    let es = entries(
        y,
        &path,
        &[
            "sources",
            "source",
            "s",
            "subject",
            "subjects",
            "po",
            "predicateobjects",
            "graphs",
            "graph",
            "g",
        ],
    )?;
    let mut decls = Vec::new();
    let mut subjects: Vec<TermMap> = Vec::new();
    let mut graphs = Vec::new();
    let mut pos: Vec<PredicateObject> = Vec::new();
    let mut joins: Vec<(usize, NamedJoin)> = Vec::new();
    for (k, v) in &es {
        let p = format!("{path}.{k}");
        match *k {
            "sources" | "source" => {
                // one shorthand list is one source, not a list of sources
                let items: Vec<&Yaml> = match v {
                    Yaml::Array(a)
                        if a.iter().all(|x| matches!(x, Yaml::String(_)))
                            && a.len() <= 2
                            && a.first().and_then(key).is_some_and(|s| s.contains('~')) =>
                    {
                        vec![v]
                    }
                    other => as_list(other),
                };
                for (i, s) in items.into_iter().enumerate() {
                    decls.push(cx.source(s, &format!("{p}[{i}]"))?);
                }
            }
            "s" | "subject" | "subjects" => {
                for (i, s) in as_list(v).into_iter().enumerate() {
                    subjects.push(subject(cx, s, &format!("{p}[{i}]"))?);
                }
            }
            "graphs" | "graph" | "g" => {
                for (i, g) in as_list(v).into_iter().enumerate() {
                    graphs.push(graph(cx, g, &format!("{p}[{i}]"))?);
                }
            }
            "po" | "predicateobjects" => {
                for (i, po) in as_list(v).into_iter().enumerate() {
                    let (po, js) = predicate_object(cx, po, &format!("{p}[{i}]"))?;
                    let at = pos.len();
                    for (target, conds) in js {
                        joins.push((at, (target, conds)));
                    }
                    pos.push(po);
                }
            }
            _ => unreachable!("checked"),
        }
    }
    if decls.is_empty() {
        return err(&path, "the mapping has no sources");
    }
    if subjects.is_empty() {
        subjects.push(TermMap {
            value: Value::Fresh,
            term_type: TermType::BlankNode,
            datatype: None,
            language: None,
        });
    }
    let mut maps = Vec::new();
    let mut out_joins = Vec::new();
    for d in &decls {
        let source = Mapping::source_index(sources, d.clone());
        for s in &subjects {
            let i = maps.len();
            // the classes of `a` with a constant object live on the subject map in RML;
            // they stay predicate-object maps here, which generate the same triples
            maps.push(TriplesMap {
                id: if decls.len() * subjects.len() > 1 {
                    format!("{name}#{}", i + 1)
                } else {
                    name.to_string()
                },
                source,
                subject: s.clone(),
                classes: Vec::new(),
                graphs: graphs.clone(),
                predicate_objects: pos.clone(),
                base: None,
            });
            for (po, join) in &joins {
                out_joins.push((i, *po, join.clone()));
            }
        }
    }
    Ok(Pending {
        name: name.to_string(),
        maps,
        joins: out_joins,
    })
}

fn subject(cx: &Cx, y: &Yaml, path: &str) -> Result<TermMap> {
    match y {
        Yaml::Hash(_) => {
            let mut value = None;
            let mut tt = TermType::Iri;
            for (k, v) in entries(
                y,
                path,
                &["value", "v", "type", "function", "fn", "parameters", "pms"],
            )? {
                match k {
                    "value" | "v" => value = scalar(v),
                    "type" => {
                        tt = match scalar(v).as_deref() {
                            Some("iri") => TermType::Iri,
                            Some("blank") | Some("blanknode") => TermType::BlankNode,
                            _ => {
                                return err(
                                    &format!("{path}.type"),
                                    "a subject's type is iri or blank",
                                );
                            }
                        }
                    }
                    "function" | "fn" => {
                        return Ok(TermMap {
                            value: Value::Function(Box::new(function(cx, y, path)?)),
                            term_type: tt,
                            datatype: None,
                            language: None,
                        });
                    }
                    _ => {}
                }
            }
            let v = value.ok_or_else(|| {
                Error::invalid(format!("YARRRML {path}: a subject needs a value"))
            })?;
            let (v, tt) = match v.strip_suffix("~blank") {
                Some(rest) => (rest.to_string(), TermType::BlankNode),
                None => (v, tt),
            };
            Ok(TermMap {
                value: cx.value(&v, tt == TermType::Iri, path)?,
                term_type: tt,
                datatype: None,
                language: None,
            })
        }
        _ => {
            let s = scalar(y).ok_or_else(|| {
                Error::invalid(format!("YARRRML {path}: a subject must be a string"))
            })?;
            let (s, tt) = match s.strip_suffix("~blank") {
                Some(rest) => (rest.to_string(), TermType::BlankNode),
                None => (
                    s.strip_suffix("~iri").unwrap_or(&s).to_string(),
                    TermType::Iri,
                ),
            };
            Ok(TermMap {
                value: cx.value(&s, tt == TermType::Iri, path)?,
                term_type: tt,
                datatype: None,
                language: None,
            })
        }
    }
}

fn graph(cx: &Cx, y: &Yaml, path: &str) -> Result<TermMap> {
    let s = scalar(y)
        .ok_or_else(|| Error::invalid(format!("YARRRML {path}: a graph must be a string")))?;
    if s == "rr:defaultGraph" || s == "defaultGraph" {
        return Ok(TermMap::constant(NamedNode::new_unchecked(
            super::DEFAULT_GRAPH_RR,
        )));
    }
    let s = s.strip_suffix("~iri").unwrap_or(&s).to_string();
    Ok(TermMap {
        value: cx.value(&s, true, path)?,
        term_type: TermType::Iri,
        datatype: None,
        language: None,
    })
}

fn predicate(cx: &Cx, y: &Yaml, path: &str) -> Result<TermMap> {
    let s = scalar(y)
        .ok_or_else(|| Error::invalid(format!("YARRRML {path}: a predicate must be a string")))?;
    let s = s.strip_suffix("~iri").unwrap_or(&s).to_string();
    Ok(TermMap {
        value: cx.value(&s, true, path)?,
        term_type: TermType::Iri,
        datatype: None,
        language: None,
    })
}

/// An object from its value and the optional third element of the list form: a
/// datatype, or a language with `~lang`.
fn object_value(
    cx: &Cx,
    v: &str,
    extra: Option<&str>,
    is_type: bool,
    path: &str,
) -> Result<TermMap> {
    let (v, mut tt) = if let Some(r) = v.strip_suffix("~iri") {
        (r.to_string(), TermType::Iri)
    } else if let Some(r) = v.strip_suffix("~blank") {
        (r.to_string(), TermType::BlankNode)
    } else if is_type {
        (v.to_string(), TermType::Iri)
    } else {
        (v.to_string(), TermType::Literal)
    };
    let mut datatype = None;
    let mut language = None;
    if let Some(x) = extra {
        if let Some(l) = x.strip_suffix("~lang") {
            language = Some(lang(cx, l, path)?);
        } else {
            datatype = Some(Dt::Constant(cx.iri(x, path)?));
        }
        tt = TermType::Literal;
    }
    Ok(TermMap {
        value: cx.value(&v, tt == TermType::Iri, path)?,
        term_type: tt,
        datatype,
        language,
    })
}

fn lang(cx: &Cx, l: &str, path: &str) -> Result<Lang> {
    match cx.value(l, false, path)? {
        Value::Constant(Term::Literal(t)) => {
            if Literal::new_language_tagged_literal("", t.value()).is_err() {
                return err(
                    path,
                    format!("{:?} is not a well-formed language tag", t.value()),
                );
            }
            Ok(Lang::Constant(t.value().to_string()))
        }
        v => Ok(Lang::Map(Box::new(TermMap {
            value: v,
            term_type: TermType::Literal,
            datatype: None,
            language: None,
        }))),
    }
}

type Joins = Vec<(String, Vec<(String, String)>)>;

fn predicate_object(cx: &Cx, y: &Yaml, path: &str) -> Result<(PredicateObject, Joins)> {
    let mut joins = Vec::new();
    match y {
        Yaml::Array(a) => {
            if a.len() < 2 || a.len() > 3 {
                return err(
                    path,
                    "the list form is [predicate, object] or [predicate, object, datatype or language~lang]",
                );
            }
            let preds: Vec<TermMap> = as_list(&a[0])
                .into_iter()
                .enumerate()
                .map(|(i, p)| predicate(cx, p, &format!("{path}[0][{i}]")))
                .collect::<Result<_>>()?;
            let is_type = preds.iter().all(
                |p| matches!(&p.value, Value::Constant(Term::NamedNode(n)) if *n == rdf::TYPE),
            );
            let extra = a.get(2).and_then(scalar);
            let mut objects = Vec::new();
            for (i, o) in as_list(&a[1]).into_iter().enumerate() {
                let op = format!("{path}[1][{i}]");
                match o {
                    Yaml::Hash(_) => {
                        let (om, js) = object(cx, o, is_type, &op)?;
                        objects.extend(om);
                        joins.extend(js);
                    }
                    _ => {
                        let s = scalar(o).ok_or_else(|| {
                            Error::invalid(format!("YARRRML {op}: an object must be a string"))
                        })?;
                        objects.push(ObjectMap::Term(object_value(
                            cx,
                            &s,
                            extra.as_deref(),
                            is_type,
                            &op,
                        )?));
                    }
                }
            }
            Ok((
                PredicateObject {
                    predicates: preds,
                    objects,
                    graphs: Vec::new(),
                },
                joins,
            ))
        }
        Yaml::Hash(_) => {
            let mut preds = Vec::new();
            let mut objects = Vec::new();
            let mut graphs = Vec::new();
            let es = entries(
                y,
                path,
                &[
                    "p",
                    "predicates",
                    "predicate",
                    "o",
                    "objects",
                    "object",
                    "graphs",
                    "graph",
                    "g",
                ],
            )?;
            for (k, v) in &es {
                if matches!(*k, "p" | "predicates" | "predicate") {
                    for (i, p) in as_list(v).into_iter().enumerate() {
                        preds.push(predicate(cx, p, &format!("{path}.{k}[{i}]"))?);
                    }
                }
            }
            let is_type = !preds.is_empty()
                && preds.iter().all(
                    |p| matches!(&p.value, Value::Constant(Term::NamedNode(n)) if *n == rdf::TYPE),
                );
            for (k, v) in &es {
                let p = format!("{path}.{k}");
                match *k {
                    "o" | "objects" | "object" => {
                        for (i, o) in as_list(v).into_iter().enumerate() {
                            let op = format!("{p}[{i}]");
                            match o {
                                Yaml::Hash(_) => {
                                    let (om, js) = object(cx, o, is_type, &op)?;
                                    objects.extend(om);
                                    joins.extend(js);
                                }
                                Yaml::Array(pair) => {
                                    // [value, datatype or language~lang]
                                    let s = pair.first().and_then(scalar).ok_or_else(|| {
                                        Error::invalid(format!(
                                            "YARRRML {op}: an object must be a string"
                                        ))
                                    })?;
                                    let extra = pair.get(1).and_then(scalar);
                                    objects.push(ObjectMap::Term(object_value(
                                        cx,
                                        &s,
                                        extra.as_deref(),
                                        is_type,
                                        &op,
                                    )?));
                                }
                                _ => {
                                    let s = scalar(o).ok_or_else(|| {
                                        Error::invalid(format!(
                                            "YARRRML {op}: an object must be a string"
                                        ))
                                    })?;
                                    objects.push(ObjectMap::Term(object_value(
                                        cx, &s, None, is_type, &op,
                                    )?));
                                }
                            }
                        }
                    }
                    "graphs" | "graph" | "g" => {
                        for (i, g) in as_list(v).into_iter().enumerate() {
                            graphs.push(graph(cx, g, &format!("{p}[{i}]"))?);
                        }
                    }
                    _ => {}
                }
            }
            if preds.is_empty() {
                return err(path, "the predicate-object has no predicate");
            }
            Ok((
                PredicateObject {
                    predicates: preds,
                    objects,
                    graphs,
                },
                joins,
            ))
        }
        _ => err(path, "a predicate-object must be a list or a mapping"),
    }
}

/// An object in the long form: a value with a type, datatype or language, a function,
/// or a join with another mapping.
fn object(cx: &Cx, y: &Yaml, is_type: bool, path: &str) -> Result<(Vec<ObjectMap>, Joins)> {
    let es = entries(
        y,
        path,
        &[
            "value",
            "v",
            "type",
            "datatype",
            "language",
            "mapping",
            "condition",
            "function",
            "fn",
            "parameters",
            "pms",
        ],
    )?;
    if let Some((_, m)) = es.iter().find(|(k, _)| *k == "mapping") {
        let target = scalar(m)
            .ok_or_else(|| Error::invalid(format!("YARRRML {path}.mapping: not a string")))?;
        let mut conds = Vec::new();
        if let Some((_, c)) = es.iter().find(|(k, _)| *k == "condition") {
            for (i, c) in as_list(c).into_iter().enumerate() {
                conds.push(condition(cx, c, &format!("{path}.condition[{i}]"))?);
            }
        }
        return Ok((Vec::new(), vec![(target, conds)]));
    }
    let mut tt = if is_type {
        TermType::Iri
    } else {
        TermType::Literal
    };
    let mut explicit_type = false;
    let mut datatype = None;
    let mut language = None;
    let mut value = None;
    for (k, v) in &es {
        let p = format!("{path}.{k}");
        match *k {
            "value" | "v" => value = scalar(v),
            "type" => {
                explicit_type = true;
                tt = match scalar(v).as_deref() {
                    Some("iri") => TermType::Iri,
                    Some("literal") => TermType::Literal,
                    Some("blank") | Some("blanknode") => TermType::BlankNode,
                    _ => return err(&p, "an object's type is iri, literal or blank"),
                }
            }
            "datatype" => {
                let d = scalar(v)
                    .ok_or_else(|| Error::invalid(format!("YARRRML {p}: not a string")))?;
                datatype = Some(match cx.value(&d, true, &p)? {
                    Value::Constant(Term::NamedNode(n)) => Dt::Constant(n),
                    v => Dt::Map(Box::new(TermMap {
                        value: v,
                        term_type: TermType::Iri,
                        datatype: None,
                        language: None,
                    })),
                });
            }
            "language" => {
                let l = scalar(v)
                    .ok_or_else(|| Error::invalid(format!("YARRRML {p}: not a string")))?;
                language = Some(lang(cx, &l, &p)?);
            }
            _ => {}
        }
    }
    if datatype.is_some() || language.is_some() {
        if explicit_type && tt != TermType::Literal {
            return err(path, "a datatype or language needs a literal");
        }
        tt = TermType::Literal;
    }
    if es.iter().any(|(k, _)| *k == "function" || *k == "fn") {
        let f = function(cx, y, path)?;
        return Ok((
            vec![ObjectMap::Term(TermMap {
                value: Value::Function(Box::new(f)),
                term_type: tt,
                datatype,
                language,
            })],
            Vec::new(),
        ));
    }
    let v = value.ok_or_else(|| {
        Error::invalid(format!(
            "YARRRML {path}: an object needs a value, a function or a mapping"
        ))
    })?;
    let (v, tt) = if let Some(r) = v.strip_suffix("~iri") {
        (r.to_string(), TermType::Iri)
    } else if let Some(r) = v.strip_suffix("~lang") {
        (r.to_string(), tt)
    } else {
        (v, tt)
    };
    Ok((
        vec![ObjectMap::Term(TermMap {
            value: cx.value(&v, tt == TermType::Iri, path)?,
            term_type: tt,
            datatype,
            language,
        })],
        Vec::new(),
    ))
}

/// A join condition: the `equal` function with two parameters, the first from this
/// mapping and the second from the parent (or as their third element says, `s` for
/// this mapping and `o` for the parent).
fn condition(cx: &Cx, y: &Yaml, path: &str) -> Result<(String, String)> {
    let es = entries(y, path, &["function", "fn", "parameters", "pms"])?;
    let f = es
        .iter()
        .find(|(k, _)| matches!(*k, "function" | "fn"))
        .and_then(|(_, v)| scalar(v))
        .ok_or_else(|| Error::invalid(format!("YARRRML {path}: a condition needs a function")))?;
    let fi = if f.contains(':') {
        cx.expand(&f, path)?
    } else {
        f.clone()
    };
    if fi != "equal" && fi != format!("{}equal", super::functions::IDLAB) {
        return err(path, format!("a join condition must use equal, not {f:?}"));
    }
    let params = es
        .iter()
        .find(|(k, _)| matches!(*k, "parameters" | "pms"))
        .map(|(_, v)| as_list(v))
        .unwrap_or_default();
    let mut child = None;
    let mut parent = None;
    for (i, p) in params.iter().enumerate() {
        let pp = format!("{path}.parameters[{i}]");
        let (name, value, side) = match p {
            Yaml::Array(a) => (
                a.first().and_then(scalar),
                a.get(1).and_then(scalar),
                a.get(2).and_then(scalar),
            ),
            Yaml::Hash(_) => {
                let mut n = None;
                let mut v = None;
                let mut s = None;
                for (k, x) in entries(p, &pp, &["parameter", "value", "from"])? {
                    match k {
                        "parameter" => n = scalar(x),
                        "value" => v = scalar(x),
                        _ => s = scalar(x),
                    }
                }
                (n, v, s)
            }
            _ => return err(&pp, "a parameter is [name, value] or a mapping"),
        };
        let value = value
            .ok_or_else(|| Error::invalid(format!("YARRRML {pp}: a parameter needs a value")))?;
        let reference = match cx.value(&value, false, &pp)? {
            Value::Reference(r) => r,
            _ => return err(&pp, "a join condition compares references, $(name)"),
        };
        let name = name.unwrap_or_default();
        let to_parent = match side.as_deref() {
            Some("o") | Some("object") => true,
            Some("s") | Some("subject") => false,
            _ => {
                name.ends_with("str2")
                    || name.ends_with("valueParameter2")
                    || name.ends_with("otherStr")
            }
        };
        if to_parent {
            parent = Some(reference);
        } else {
            child = Some(reference);
        }
    }
    match (child, parent) {
        (Some(c), Some(p)) => Ok((c, p)),
        _ => err(path, "a join condition needs one reference of each side"),
    }
}

fn function(cx: &Cx, y: &Yaml, path: &str) -> Result<FunctionCall> {
    let Yaml::Hash(h) = y else {
        return err(path, "a function must be a mapping");
    };
    let mut name = None;
    let mut params: Vec<&Yaml> = Vec::new();
    for (k, v) in h {
        match key(k) {
            Some("function" | "fn") => name = scalar(v),
            Some("parameters" | "pms") => params = as_list(v),
            _ => {}
        }
    }
    let name = name.ok_or_else(|| Error::invalid(format!("YARRRML {path}: no function name")))?;
    if name.contains('(') {
        return err(
            path,
            "the inline function syntax is not supported; use function and parameters",
        );
    }
    let function = cx.iri(&name, path)?;
    let mut args = Vec::new();
    for (i, p) in params.into_iter().enumerate() {
        let pp = format!("{path}.parameters[{i}]");
        let (pname, value): (String, &Yaml) = match p {
            Yaml::Array(a) if a.len() == 2 => (
                a.first()
                    .and_then(scalar)
                    .ok_or_else(|| Error::invalid(format!("YARRRML {pp}: no parameter name")))?,
                &a[1],
            ),
            Yaml::Hash(_) => {
                let mut n = None;
                let mut v = None;
                for (k, x) in entries(p, &pp, &["parameter", "value"])? {
                    if k == "parameter" {
                        n = scalar(x);
                    } else {
                        v = Some(x);
                    }
                }
                (
                    n.ok_or_else(|| Error::invalid(format!("YARRRML {pp}: no parameter name")))?,
                    v.ok_or_else(|| Error::invalid(format!("YARRRML {pp}: no value")))?,
                )
            }
            _ => {
                return err(
                    &pp,
                    "a parameter is [name, value] or a mapping with parameter and value",
                );
            }
        };
        let param = cx.iri(&pname, &pp)?;
        let value = match value {
            Yaml::Hash(_) => TermMap {
                value: Value::Function(Box::new(function_nested(cx, value, &pp)?)),
                term_type: TermType::Literal,
                datatype: None,
                language: None,
            },
            other => {
                let s = scalar(other)
                    .ok_or_else(|| Error::invalid(format!("YARRRML {pp}: not a string")))?;
                TermMap {
                    value: cx.value(&s, false, &pp)?,
                    term_type: TermType::Literal,
                    datatype: None,
                    language: None,
                }
            }
        };
        args.push((param, value));
    }
    Ok(FunctionCall { function, args })
}

fn function_nested(cx: &Cx, y: &Yaml, path: &str) -> Result<FunctionCall> {
    function(cx, y, path)
}

#[allow(dead_code)]
fn xsd_string() -> NamedNode {
    xsd::STRING.into_owned()
}
