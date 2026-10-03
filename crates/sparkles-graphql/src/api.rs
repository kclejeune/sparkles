//! The API schema a client sees (§3.4), derived from the mapping: `Node`, `Resource`,
//! the scalars with `@specifiedBy`, per mapped type `TFilter`, `TOrderBy`,
//! `TConnection` and `TEdge`, the generated arguments, and `Query`.

use crate::mapping::{FieldMap, Mapping, RootKind, Target};
use crate::names::upper_snake;
use crate::scalars::{CUSTOM, Scalar};
use std::collections::BTreeSet;
use std::fmt::Write;

fn quoted(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn desc(out: &mut String, indent: &str, d: Option<&str>) {
    if let Some(d) = d {
        let _ = writeln!(out, "{indent}{}", quoted(d));
    }
}

/// The `TOrderBy` values of a type's orderable fields: `(value, field)`.
pub fn order_values(t: &crate::mapping::TypeMap) -> Vec<(String, String)> {
    t.fields
        .iter()
        .filter(|f| !f.list && orderable(f))
        .map(|f| (upper_snake(&f.name), f.name.clone()))
        .collect()
}

fn orderable(f: &FieldMap) -> bool {
    match &f.target {
        Target::Scalar(s) => s.orderable(),
        Target::Enum(_) => true,
        Target::Object(_) => false,
    }
}

/// The input type of filters on a field, if it has one.
fn filter_type(m: &Mapping, f: &FieldMap) -> Option<String> {
    match &f.target {
        Target::Scalar(s) => s.filter_type().map(str::to_string),
        Target::Enum(e) => Some(format!("{e}Filter")),
        Target::Object(o) if m.ty(o).is_some() => Some(format!("{o}Filter")),
        Target::Object(_) => None,
    }
}

const ORDER_OPS: &str = "eq ne in notIn lt lte gt gte";

/// Write the API schema as SDL.
pub fn sdl(m: &Mapping) -> String {
    let mut out = String::new();
    let mut used: BTreeSet<Scalar> = BTreeSet::new();
    used.insert(Scalar::Id);
    for t in &m.types {
        for f in &t.fields {
            if let Target::Scalar(s) = f.target {
                used.insert(s);
            }
        }
    }
    out.push_str("\"An RDF node, identified by its IRI or by a stored blank node label (_:b…).\"\ninterface Node {\n  id: ID!\n}\n\n");
    out.push_str("\"A node that belongs to no mapped type, with its direct rdf:type IRIs.\"\ntype Resource implements Node {\n  id: ID!\n  _types: [ID!]!\n}\n\n");
    out.push_str("type PageInfo {\n  hasNextPage: Boolean!\n  hasPreviousPage: Boolean!\n  startCursor: String\n  endCursor: String\n}\n\n");
    out.push_str("\"The order of the values of a multi-valued field.\"\nenum ValueOrder {\n  ASC\n  DESC\n}\n\n");
    for (name, url) in CUSTOM {
        if m.scalars.iter().any(|s| s == name) || used.iter().any(|s| s.name() == name) {
            let _ = writeln!(out, "scalar {name} @specifiedBy(url: {})\n", quoted(url));
        }
    }
    if used.contains(&Scalar::LangString) {
        out.push_str("\"A literal with its language tag and base direction.\"\n");
        out.push_str(crate::mapping::LANG_STRING);
        out.push_str("\n\n");
    }
    if used.contains(&Scalar::RdfTerm) {
        out.push_str("\"Any RDF term: kind is IRI, BLANK_NODE or LITERAL.\"\n");
        out.push_str(crate::mapping::RDF_TERM);
        out.push_str("\n\n");
    }
    // scalar filters
    let mut filters: BTreeSet<(String, String)> = BTreeSet::new();
    filters.insert(("IDFilter".into(), "ID".into()));
    for s in &used {
        if let Some(ft) = s.filter_type() {
            let vt = match s {
                Scalar::LangString => "String",
                s => s.name(),
            };
            filters.insert((ft.to_string(), vt.to_string()));
        }
    }
    for (ft, vt) in &filters {
        let _ = writeln!(out, "input {ft} {{");
        match ft.as_str() {
            "IDFilter" => {
                let _ = writeln!(out, "  eq: ID\n  in: [ID!]\n  notIn: [ID!]");
            }
            "IRIFilter" => {
                let _ = writeln!(
                    out,
                    "  eq: IRI\n  in: [IRI!]\n  notIn: [IRI!]\n  exists: Boolean"
                );
            }
            "BooleanFilter" => {
                let _ = writeln!(out, "  eq: Boolean\n  ne: Boolean\n  exists: Boolean");
            }
            "StringFilter" => {
                for op in ORDER_OPS.split(' ') {
                    let t = if op.ends_with("In") || op == "in" {
                        "[String!]"
                    } else {
                        "String"
                    };
                    let _ = writeln!(out, "  {op}: {t}");
                }
                let _ = writeln!(
                    out,
                    "  startsWith: String\n  contains: String\n  \"A SPARQL REGEX pattern\"\n  regex: String\n  \"The flags of regex\"\n  flags: String\n  \"Compare only values whose language tag matches this range\"\n  lang: String\n  exists: Boolean"
                );
            }
            _ => {
                for op in ORDER_OPS.split(' ') {
                    let t = if op == "in" || op == "notIn" {
                        format!("[{vt}!]")
                    } else {
                        vt.clone()
                    };
                    let _ = writeln!(out, "  {op}: {t}");
                }
                let _ = writeln!(out, "  exists: Boolean");
            }
        }
        out.push_str("}\n\n");
    }
    // enums and their filters
    for e in &m.enums {
        desc(&mut out, "", e.description.as_deref());
        let _ = writeln!(out, "enum {} {{", e.name);
        for (v, iri, d) in &e.values {
            desc(&mut out, "  ", Some(d.as_deref().unwrap_or(iri.as_str())));
            let _ = writeln!(out, "  {v}");
        }
        out.push_str("}\n\n");
        let n = &e.name;
        let _ = writeln!(
            out,
            "input {n}Filter {{\n  eq: {n}\n  ne: {n}\n  in: [{n}!]\n  notIn: [{n}!]\n  exists: Boolean\n}}\n"
        );
    }
    for (name, members, d) in &m.unions {
        desc(&mut out, "", d.as_deref());
        let _ = writeln!(out, "union {name} = {}\n", members.join(" | "));
    }
    for t in &m.types {
        let mut implements = vec!["Node".to_string()];
        implements.extend(t.implements.iter().cloned());
        desc(
            &mut out,
            "",
            Some(
                t.description
                    .as_deref()
                    .unwrap_or(&format!("Members of the class <{}>.", t.class)),
            ),
        );
        let kw = if t.interface { "interface" } else { "type" };
        let _ = writeln!(
            out,
            "{kw} {} implements {} {{\n  \"The node's IRI, or its blank node label\"\n  id: ID!",
            t.name,
            implements.join(" & ")
        );
        for f in &t.fields {
            desc(
                &mut out,
                "  ",
                Some(f.description.as_deref().unwrap_or(&format!(
                    "{}<{}>",
                    if f.inverse { "^" } else { "" },
                    f.predicate
                ))),
            );
            let args = field_args(m, f);
            let _ = writeln!(out, "  {}{args}: {}", f.name, f.ty);
        }
        out.push_str("}\n\n");
        let n = &t.name;
        // TFilter
        let _ = writeln!(
            out,
            "input {n}Filter {{\n  and: [{n}Filter!]\n  or: [{n}Filter!]\n  not: {n}Filter\n  id: IDFilter"
        );
        for f in &t.fields {
            if let Some(ft) = filter_type(m, f) {
                let _ = writeln!(out, "  {}: {ft}", f.name);
            }
        }
        out.push_str("}\n\n");
        // TOrderBy
        let _ = writeln!(out, "enum {n}OrderBy {{\n  ID_ASC\n  ID_DESC");
        for (v, _) in order_values(t) {
            let _ = writeln!(out, "  {v}_ASC\n  {v}_DESC");
        }
        out.push_str("}\n\n");
        let _ = writeln!(
            out,
            "type {n}Connection {{\n  edges: [{n}Edge!]!\n  nodes: [{n}!]!\n  pageInfo: PageInfo!\n  \"The number of members that match the filter\"\n  totalCount: Int!\n}}\n\ntype {n}Edge {{\n  cursor: String!\n  node: {n}!\n}}\n"
        );
    }
    desc(&mut out, "", m.query_description.as_deref());
    out.push_str("type Query {\n");
    for r in &m.roots {
        desc(&mut out, "  ", r.description.as_deref());
        let line = match &r.kind {
            RootKind::Lookup(t) => format!(
                "{}(id: ID!): {}",
                r.name,
                r.ty.clone().unwrap_or_else(|| t.clone())
            ),
            RootKind::Connection(t) => format!(
                "{}(filter: {t}Filter, orderBy: [{t}OrderBy!], first: Int, after: String, last: Int, before: String, offset: Int): {}",
                r.name,
                r.ty.clone().unwrap_or_else(|| format!("{t}Connection!"))
            ),
            RootKind::List(t) => format!(
                "{}(filter: {t}Filter, orderBy: [{t}OrderBy!], first: Int, offset: Int): {}",
                r.name,
                r.ty.clone().unwrap_or_else(|| format!("[{t}!]!"))
            ),
            RootKind::Node => format!("{}(id: ID!): Node", r.name),
        };
        let _ = writeln!(out, "  {line}");
    }
    out.push_str("}\n");
    out
}

fn field_args(m: &Mapping, f: &FieldMap) -> String {
    let lang = matches!(f.target, Target::Scalar(s) if s.is_text());
    let mut args: Vec<String> = Vec::new();
    match (&f.target, f.list) {
        (Target::Object(o), true) => {
            if m.ty(o).is_some() {
                args.push(format!("filter: {o}Filter"));
                args.push(format!("orderBy: [{o}OrderBy!]"));
            }
            args.push("first: Int".into());
            args.push("offset: Int".into());
        }
        (Target::Object(_), false) => {}
        (_, true) => {
            args.push("first: Int".into());
            args.push("offset: Int".into());
            args.push("orderBy: ValueOrder".into());
        }
        (_, false) => {}
    }
    if lang {
        args.push(
            "\"Language ranges in order of preference; \\\"*\\\" matches any tag and \\\"\\\" none\" lang: [String!]"
                .into(),
        );
    }
    if args.is_empty() {
        String::new()
    } else {
        format!("({})", args.join(", "))
    }
}
