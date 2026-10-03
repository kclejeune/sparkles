//! Drafts of a mapping schema (§3.5): SDL with comments that explain each decision,
//! for an administrator to review and install. A draft is made from SHACL shapes (the
//! write-time guard's, or a named graph), or from the shapes C02 drafts from the data.
//! Nothing is installed here.

use crate::mapping::RESERVED_TYPES;
use crate::names::{assign, assign_bases, local_name, prefix_of, sanitize};
use crate::scalars::{RDF, RDFS, XSD};
use oxrdf::{Graph, NamedNodeRef, NamedOrBlankNode, NamedOrBlankNodeRef, Term, TermRef};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write;

const SH: &str = "http://www.w3.org/ns/shacl#";

/// The type of a drafted field's values.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "iri")]
pub enum Range {
    Datatype(String),
    Class(String),
    /// any node (`sh:nodeKind sh:IRI` without a class)
    Node,
    /// `sh:in` of IRIs
    Enum(Vec<String>),
    /// nothing says
    Unknown,
}

/// A drafted field.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftField {
    pub path: String,
    pub inverse: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sh_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip)]
    pub order: Option<f64>,
    pub single: bool,
    /// `sh:minCount` of at least 1
    pub min_one: bool,
    pub required: bool,
    pub range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<Vec<String>>,
    /// a value outside the 32-bit range was observed
    pub big: bool,
    pub notes: Vec<String>,
    /// the name and type written in the draft
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

/// A drafted object type.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftType {
    pub class: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub notes: Vec<String>,
    pub fields: Vec<DraftField>,
}

/// A draft: the SDL and the decisions behind it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub source: String,
    pub sdl: String,
    pub types: Vec<DraftType>,
    pub skipped: Vec<String>,
}

fn sh(local: &str) -> NamedNodeRef<'_> {
    // the callers pass the static SH names below
    NamedNodeRef::new_unchecked(local)
}

fn iri<'a>(g: &'a Graph, s: NamedOrBlankNodeRef<'a>, p: &str) -> Option<String> {
    match g.object_for_subject_predicate(s, sh(p))? {
        TermRef::NamedNode(n) => Some(n.as_str().to_string()),
        _ => None,
    }
}

fn literal<'a>(g: &'a Graph, s: NamedOrBlankNodeRef<'a>, p: &str) -> Option<String> {
    match g.object_for_subject_predicate(s, sh(p))? {
        TermRef::Literal(l) => Some(l.value().to_string()),
        _ => None,
    }
}

/// The members of an RDF list.
fn list(g: &Graph, head: TermRef<'_>) -> Vec<Term> {
    let first = format!("{RDF}first");
    let rest = format!("{RDF}rest");
    let nil = format!("{RDF}nil");
    let mut out = Vec::new();
    let mut cur = head.into_owned();
    for _ in 0..10_000 {
        let node: NamedOrBlankNode = match &cur {
            Term::NamedNode(n) if n.as_str() == nil => break,
            Term::NamedNode(n) => n.clone().into(),
            Term::BlankNode(b) => b.clone().into(),
            _ => break,
        };
        if let Some(v) = g.object_for_subject_predicate(&node, sh(&first)) {
            out.push(v.into_owned());
        }
        match g.object_for_subject_predicate(&node, sh(&rest)) {
            Some(r) => cur = r.into_owned(),
            None => break,
        }
    }
    out
}

/// What a set of SHACL shapes says about classes and their properties.
pub fn read_shapes(g: &Graph) -> (Vec<DraftType>, Vec<String>) {
    let sh_target = format!("{SH}targetClass");
    let rdf_type = format!("{RDF}type");
    let node_shape = format!("{SH}NodeShape");
    let rdfs_class = format!("{RDFS}Class");
    let mut skipped = Vec::new();
    // shape -> classes
    let mut targets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut shapes: Vec<NamedOrBlankNode> = Vec::new();
    for t in g.triples_for_predicate(sh(&sh_target)) {
        if let TermRef::NamedNode(c) = t.object {
            let s = t.subject.into_owned();
            targets
                .entry(s.to_string())
                .or_default()
                .push(c.as_str().to_string());
            if !shapes.contains(&s) {
                shapes.push(s);
            }
        }
    }
    for s in g.subjects_for_predicate_object(sh(&rdf_type), sh(&node_shape)) {
        let s = s.into_owned();
        let is_class = g.contains(oxrdf::TripleRef::new(&s, sh(&rdf_type), sh(&rdfs_class)));
        match (&s, is_class) {
            (NamedOrBlankNode::NamedNode(n), true) => {
                targets
                    .entry(s.to_string())
                    .or_default()
                    .push(n.as_str().to_string());
                if !shapes.contains(&s) {
                    shapes.push(s.clone());
                }
            }
            _ if !targets.contains_key(&s.to_string()) => {
                skipped.push(format!(
                    "the shape {s} has no sh:targetClass and is not a class: skipped"
                ));
            }
            _ => {}
        }
    }
    // a shape referenced by sh:node gives the class of its target
    let class_of_shape = |n: &Term| -> Option<String> {
        targets.get(&n.to_string()).and_then(|c| c.first().cloned())
    };
    let mut by_class: BTreeMap<String, DraftType> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for s in &shapes {
        let classes = targets.get(&s.to_string()).cloned().unwrap_or_default();
        let desc = literal(g, s.as_ref(), &format!("{SH}description"))
            .or_else(|| literal(g, s.as_ref(), &format!("{RDFS}comment")));
        let mut fields = Vec::new();
        for ps in g.objects_for_subject_predicate(s, sh(&format!("{SH}property"))) {
            let ps: NamedOrBlankNode = match ps {
                TermRef::NamedNode(n) => n.into_owned().into(),
                TermRef::BlankNode(b) => b.into_owned().into(),
                _ => continue,
            };
            let psr = ps.as_ref();
            let (path, inverse) = match g
                .object_for_subject_predicate(psr, sh(&format!("{SH}path")))
            {
                Some(TermRef::NamedNode(p)) => (p.as_str().to_string(), false),
                Some(TermRef::BlankNode(b)) => {
                    match iri(g, b.into(), &format!("{SH}inversePath")) {
                        Some(p) => (p, true),
                        None => {
                            skipped.push(format!(
                                "a property shape of {s} has a path other than an IRI or an inverse IRI: skipped"
                            ));
                            continue;
                        }
                    }
                }
                _ => continue,
            };
            let count =
                |p: &str| literal(g, psr, &format!("{SH}{p}")).and_then(|v| v.parse::<u64>().ok());
            let class = iri(g, psr, &format!("{SH}class")).or_else(|| {
                g.object_for_subject_predicate(psr, sh(&format!("{SH}node")))
                    .and_then(|n| class_of_shape(&n.into_owned()))
            });
            let datatype = iri(g, psr, &format!("{SH}datatype"));
            let node_kind = iri(g, psr, &format!("{SH}nodeKind"));
            let ins: Vec<Term> = g
                .object_for_subject_predicate(psr, sh(&format!("{SH}in")))
                .map(|h| list(g, h))
                .unwrap_or_default();
            let range = if let Some(d) = datatype {
                Range::Datatype(d)
            } else if let Some(c) = class {
                Range::Class(c)
            } else if !ins.is_empty() && ins.iter().all(|t| matches!(t, Term::NamedNode(_))) {
                Range::Enum(
                    ins.iter()
                        .filter_map(|t| match t {
                            Term::NamedNode(n) => Some(n.as_str().to_string()),
                            _ => None,
                        })
                        .collect(),
                )
            } else if matches!(
                node_kind.as_deref().and_then(|k| k.strip_prefix(SH)),
                Some("IRI" | "BlankNodeOrIRI" | "BlankNode")
            ) {
                Range::Node
            } else {
                Range::Unknown
            };
            let lang: Option<Vec<String>> = g
                .object_for_subject_predicate(psr, sh(&format!("{SH}languageIn")))
                .map(|h| {
                    let mut l: Vec<String> = list(g, h)
                        .iter()
                        .filter_map(|t| match t {
                            Term::Literal(l) => Some(l.value().to_string()),
                            _ => None,
                        })
                        .collect();
                    l.push("*".into());
                    l
                });
            fields.push(DraftField {
                path,
                inverse,
                sh_name: literal(g, psr, &format!("{SH}name")),
                description: literal(g, psr, &format!("{SH}description")),
                order: literal(g, psr, &format!("{SH}order")).and_then(|v| v.parse().ok()),
                single: count("maxCount") == Some(1),
                min_one: count("minCount").is_some_and(|n| n >= 1),
                required: false,
                range,
                lang,
                big: false,
                notes: Vec::new(),
                name: String::new(),
                ty: String::new(),
            });
        }
        for c in classes {
            let t = by_class.entry(c.clone()).or_insert_with(|| {
                order.push(c.clone());
                DraftType {
                    class: c.clone(),
                    name: String::new(),
                    description: desc.clone(),
                    notes: Vec::new(),
                    fields: Vec::new(),
                }
            });
            for f in &fields {
                if !t
                    .fields
                    .iter()
                    .any(|x| x.path == f.path && x.inverse == f.inverse)
                {
                    t.fields.push(f.clone());
                }
            }
            t.notes.push(format!("from the shape {s}"));
        }
    }
    let mut types: Vec<DraftType> = order
        .into_iter()
        .filter_map(|c| by_class.remove(&c))
        .collect();
    for t in &mut types {
        t.fields.sort_by(|a, b| {
            a.order
                .unwrap_or(f64::MAX)
                .partial_cmp(&b.order.unwrap_or(f64::MAX))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    (types, skipped)
}

/// What C02's drafted shapes say, with the observations behind them.
pub fn read_observed(
    d: &sparkles::schema::draft::ShapesDraft,
    big: &dyn Fn(&str, &str) -> bool,
) -> Vec<DraftType> {
    use sparkles::schema::draft::ConstraintValue as V;
    let commit = d.snapshot.commit;
    d.shapes
        .iter()
        .map(|s| DraftType {
            class: s.class.clone(),
            name: String::new(),
            description: None,
            notes: vec![format!("{} instances at commit {commit}", s.instances)],
            fields: s
                .properties
                .iter()
                .map(|p| {
                    let get = |c: &str| p.constraints.iter().find(|x| x.component == c).map(|x| &x.value);
                    let range = match (get("datatype"), get("class"), get("in"), get("nodeKind")) {
                        (Some(V::Iri(dt)), _, _, _) => Range::Datatype(dt.clone()),
                        (_, Some(V::Iri(c)), _, _) => Range::Class(c.clone()),
                        (_, _, Some(V::List(vs)), _)
                            if vs.iter().all(|v| v.starts_with('<')) =>
                        {
                            Range::Enum(
                                vs.iter()
                                    .map(|v| v.trim_start_matches('<').trim_end_matches('>').to_string())
                                    .collect(),
                            )
                        }
                        (_, _, _, Some(V::Iri(k)))
                            if matches!(k.strip_prefix(SH), Some("IRI" | "BlankNodeOrIRI" | "BlankNode")) =>
                        {
                            Range::Node
                        }
                        _ => Range::Unknown,
                    };
                    let lang = match get("languageIn") {
                        Some(V::List(l)) if l.iter().any(|x| x == "en") => {
                            Some(vec!["en".into(), String::new(), "*".into()])
                        }
                        _ => None,
                    };
                    let single = p.max_values <= 1;
                    let mut notes = Vec::new();
                    if single {
                        notes.push(format!(
                            "observed at most one value per instance at commit {commit}; nothing enforces this"
                        ));
                    }
                    if p.instances < s.instances {
                        notes.push(format!(
                            "{} of {} instances have a value",
                            p.instances, s.instances
                        ));
                    }
                    let is_int = matches!(&range, Range::Datatype(dt) if int_type(dt));
                    DraftField {
                        path: p.path.clone(),
                        inverse: false,
                        sh_name: None,
                        description: None,
                        order: None,
                        single,
                        min_one: false,
                        required: false,
                        big: is_int && big(&s.class, &p.path),
                        range,
                        lang,
                        notes,
                        name: String::new(),
                        ty: String::new(),
                    }
                })
                .collect(),
        })
        .collect()
}

fn int_type(dt: &str) -> bool {
    dt.strip_prefix(XSD).is_some_and(|l| {
        matches!(
            l,
            "integer"
                | "long"
                | "int"
                | "short"
                | "byte"
                | "nonNegativeInteger"
                | "positiveInteger"
                | "nonPositiveInteger"
                | "negativeInteger"
                | "unsignedLong"
                | "unsignedInt"
                | "unsignedShort"
                | "unsignedByte"
        )
    })
}

/// The scalar of a datatype (§4.3).
fn scalar_of(dt: &str, big: bool) -> &'static str {
    if int_type(dt) {
        return if big { "Integer" } else { "Int" };
    }
    match dt.strip_prefix(XSD) {
        Some("boolean") => "Boolean",
        Some("decimal") => "Decimal",
        Some("double" | "float") => "Float",
        Some("dateTime" | "dateTimeStamp") => "DateTime",
        Some("date") => "Date",
        Some("time") => "Time",
        Some("duration" | "dayTimeDuration" | "yearMonthDuration") => "Duration",
        Some("anyURI") => "IRI",
        _ => "String",
    }
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && !n.starts_with("__")
        && n.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// An IRI as written in the draft: a prefixed name when a prefix fits.
fn written(iri: &str, prefixes: &[(String, String)], used: &mut Vec<(String, String)>) -> String {
    if let Some(p) = prefix_of(iri, prefixes)
        && let Some((_, ns)) = prefixes.iter().find(|(n, _)| n == p)
    {
        let local = &iri[ns.len()..];
        if !local.is_empty()
            && local
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            if !used.iter().any(|(n, _)| n == p) {
                used.push((p.to_string(), ns.clone()));
            }
            return format!("{p}:{local}");
        }
    }
    iri.to_string()
}

fn quoted(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// Write the SDL of a draft. `enforced` says whether `sh:minCount 1` is backed by the
/// write-time guard (§3.3), so that a field may be non-null.
pub fn render(
    mut types: Vec<DraftType>,
    skipped: Vec<String>,
    prefixes: &[(String, String)],
    enforced: bool,
    header: &str,
    source: &str,
) -> Draft {
    // type names
    let reserved_type = |n: &str| {
        n == "Query"
            || RESERVED_TYPES.contains(&n)
            || crate::scalars::Scalar::from_name(n).is_some()
            || ["Filter", "OrderBy", "Connection", "Edge"]
                .iter()
                .any(|s| n.ends_with(s))
    };
    let iris: Vec<String> = types.iter().map(|t| t.class.clone()).collect();
    let names = assign(&iris, prefixes, &reserved_type);
    for (t, n) in types.iter_mut().zip(names) {
        t.name = n;
    }
    let class_name =
        |c: &str, types: &[DraftType]| types.iter().find(|t| t.class == c).map(|t| t.name.clone());
    let snapshot = types.clone();
    let mut enums: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut used: Vec<(String, String)> = Vec::new();
    let mut lang_en = false;
    for t in &mut types {
        let items: Vec<(String, String)> = t
            .fields
            .iter()
            .map(|f| {
                let base = f
                    .sh_name
                    .clone()
                    .filter(|n| valid_name(n))
                    .unwrap_or_else(|| sanitize(local_name(&f.path)));
                let base = if f.inverse && f.sh_name.is_none() {
                    format!("{base}Of")
                } else {
                    base
                };
                (
                    format!("{}{}", if f.inverse { "^" } else { "" }, f.path),
                    base,
                )
            })
            .collect();
        let names = assign_bases(&items, prefixes, &|n| n == "id");
        for (f, n) in t.fields.iter_mut().zip(names) {
            f.name = n;
            let named = match &f.range {
                Range::Datatype(dt) => scalar_of(dt, f.big).to_string(),
                Range::Class(c) => match class_name(c, &snapshot) {
                    Some(n) => n,
                    None => {
                        f.notes.push(format!(
                            "the class <{c}> has no type in this draft: values are typed Node"
                        ));
                        "Node".into()
                    }
                },
                Range::Node => "Node".into(),
                Range::Enum(values) => {
                    let en = format!("{}{}", t.name, upper_first(&f.name));
                    let vnames = assign(values, prefixes, &|n| {
                        matches!(n, "true" | "false" | "null")
                    });
                    enums.push((
                        en.clone(),
                        vnames.into_iter().zip(values.iter().cloned()).collect(),
                    ));
                    en
                }
                Range::Unknown => {
                    f.notes
                        .push("no sh:datatype, sh:class or sh:nodeKind: typed String".into());
                    "String".into()
                }
            };
            if f.lang.as_ref().is_some_and(|l| l.iter().any(|x| x == "en")) {
                lang_en = true;
            }
            f.required = f.single && f.min_one && enforced;
            if f.min_one && !f.required && f.single {
                f.notes.push(
                    "sh:minCount 1, but no write-time guard in reject mode with a strict baseline enforces it: nullable"
                        .into(),
                );
            }
            f.ty = if f.single {
                format!("{named}{}", if f.required { "!" } else { "" })
            } else {
                format!("[{named}!]!")
            };
        }
    }
    let mut body = String::new();
    for t in &types {
        for n in &t.notes {
            let _ = writeln!(body, "# {n}");
        }
        if let Some(d) = &t.description {
            let _ = writeln!(body, "{}", quoted(d));
        }
        let class = written(&t.class, prefixes, &mut used);
        let _ = writeln!(body, "type {} @rdf(iri: {}) {{", t.name, quoted(&class));
        if t.fields.is_empty() {
            body.push_str("  # no properties: only id\n");
        }
        for f in &t.fields {
            for n in &f.notes {
                let _ = writeln!(body, "  # {n}");
            }
            if let Some(d) = &f.description {
                let _ = writeln!(body, "  {}", quoted(d));
            }
            let p = written(&f.path, prefixes, &mut used);
            let mut dirs = format!("@rdf(iri: {}", quoted(&p));
            if f.inverse {
                dirs.push_str(", inverse: true");
            }
            dirs.push(')');
            if let Some(l) = &f.lang
                && source == "shapes"
            {
                let _ = write!(
                    dirs,
                    " @lang(prefer: [{}])",
                    l.iter().map(|x| quoted(x)).collect::<Vec<_>>().join(", ")
                );
            }
            let _ = writeln!(body, "  {}: {} {dirs}", f.name, f.ty);
        }
        body.push_str("}\n\n");
    }
    for (name, values) in &enums {
        let _ = writeln!(body, "enum {name} {{");
        for (v, i) in values {
            let w = written(i, prefixes, &mut used);
            let _ = writeln!(body, "  {v} @rdf(iri: {})", quoted(&w));
        }
        body.push_str("}\n\n");
    }
    let mut sdl = String::new();
    for line in header.lines() {
        let _ = writeln!(sdl, "# {line}");
    }
    for s in &skipped {
        let _ = writeln!(sdl, "# {s}");
    }
    if types.is_empty() {
        sdl.push_str("# The selection has no classes to draft types from.\n");
    }
    let lang = lang_en && source == "observed";
    if !used.is_empty() || lang {
        sdl.push_str("extend schema");
        used.sort();
        for (p, ns) in &used {
            let _ = write!(sdl, "\n  @prefix(name: {}, iri: {})", quoted(p), quoted(ns));
        }
        if lang {
            sdl.push_str("\n  @lang(prefer: [\"en\", \"\", \"*\"])");
        }
        sdl.push_str("\n\n");
    }
    sdl.push_str(&body);
    Draft {
        source: source.into(),
        sdl: sdl.trim_end().to_string() + "\n",
        types,
        skipped,
    }
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The ASK of the observed draft for a value of an integer predicate outside 32 bits.
pub fn big_integer_query(class: &str, pred: &str) -> String {
    format!(
        "ASK {{ ?s a/<{RDFS}subClassOf>* <{class}> ; <{pred}> ?o FILTER(isNumeric(?o) && datatype(?o) != <{XSD}double> && datatype(?o) != <{XSD}float> && datatype(?o) != <{XSD}decimal> && (?o > 2147483647 || ?o < -2147483648)) }}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(ttl: &str) -> Graph {
        let mut g = Graph::new();
        for t in oxttl::TurtleParser::new().for_slice(ttl.as_bytes()) {
            g.insert(&t.unwrap());
        }
        g
    }

    #[test]
    fn from_shapes() {
        let g = graph(
            r#"@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://example.org/> .
            @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
            ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
              sh:property [ sh:path ex:name ; sh:datatype xsd:string ; sh:minCount 1 ; sh:maxCount 1 ] ;
              sh:property [ sh:path ex:age ; sh:datatype xsd:integer ; sh:maxCount 1 ; sh:minCount 1 ] ;
              sh:property [ sh:path ex:knows ; sh:class ex:Person ] ;
              sh:property [ sh:path [ sh:inversePath ex:author ] ; sh:class ex:Book ] ;
              sh:property [ sh:path ex:status ; sh:in ( ex:active ex:retired ) ; sh:maxCount 1 ] ;
              sh:property [ sh:path ( ex:a ex:b ) ] .
            ex:Employee a sh:NodeShape, <http://www.w3.org/2000/01/rdf-schema#Class> ;
              sh:property [ sh:path ex:salary ; sh:datatype xsd:decimal ; sh:maxCount 1 ] .
            ex:Loose a sh:NodeShape ."#,
        );
        let (types, skipped) = read_shapes(&g);
        assert_eq!(types.len(), 2);
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        let p = vec![("ex".to_string(), "http://example.org/".to_string())];
        let enforced = |n: &str| -> bool { n == "name" };
        let _ = enforced;
        let d = render(types, skipped, &p, true, "test", "shapes");
        let sdl = &d.sdl;
        assert!(
            sdl.contains("type Person @rdf(iri: \"ex:Person\")"),
            "{sdl}"
        );
        assert!(
            sdl.contains("name: String! @rdf(iri: \"ex:name\")"),
            "{sdl}"
        );
        assert!(sdl.contains("knows: [Person!]!"), "{sdl}");
        assert!(
            sdl.contains("authorOf: [Node!]! @rdf(iri: \"ex:author\", inverse: true)"),
            "{sdl}"
        );
        assert!(sdl.contains("status: PersonStatus"), "{sdl}");
        assert!(sdl.contains("active @rdf(iri: \"ex:active\")"), "{sdl}");
        assert!(sdl.contains("type Employee"), "{sdl}");
        assert!(sdl.contains("salary: Decimal"), "{sdl}");
        // the draft installs
        crate::Compiled::new(crate::Config::new(sdl.clone()), 1, &|_, _, _| true)
            .unwrap_or_else(|e| panic!("{}\n{sdl}", e.message()));
        // without a guard, nothing is non-null
        let (types, skipped) = read_shapes(&g);
        let d = render(types, skipped, &p, false, "test", "shapes");
        assert!(d.sdl.contains("name: String @rdf"), "{}", d.sdl);
    }
}
