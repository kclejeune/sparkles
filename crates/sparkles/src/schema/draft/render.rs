//! The draft as text: a SHACL shapes graph in Turtle, a ShEx schema in ShExC and the
//! query shape map that goes with it. The counts go into comments.

use super::{ConstraintDraft, ConstraintValue, NodeShapeDraft, PropertyDraft, SH, ShapesDraft};
use std::collections::BTreeSet;
use std::fmt::Write as _;

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// IRIs written as prefixed names where a prefix fits.
pub struct Prefixes {
    /// (prefix, namespace), longest namespace first
    list: Vec<(String, String)>,
}

/// Whether `l` can be written as the local part of a prefixed name in both Turtle and
/// ShExC (a conservative subset of `PN_LOCAL`).
fn simple_local(l: &str) -> bool {
    !l.is_empty()
        && l.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
        && l.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn simple_prefix(p: &str) -> bool {
    p.is_empty()
        || (p.starts_with(|c: char| c.is_ascii_alphabetic())
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
}

impl Prefixes {
    /// `sh`, `rdf`, `xsd`, `rdfs`, `owl`, `shape` (the shape namespace) and the dataset's
    /// prefixes; a dataset prefix never rebinds one of the others.
    pub fn new(prefixes: &[(String, String)], base: &str) -> Prefixes {
        let mut list: Vec<(String, String)> = Vec::new();
        let mut add = |p: &str, ns: &str| {
            if simple_prefix(p) && !list.iter().any(|(q, n)| q == p || n == ns) && !ns.is_empty() {
                list.push((p.to_string(), ns.to_string()));
            }
        };
        add("sh", SH);
        add("rdf", RDF);
        add("xsd", XSD);
        add("rdfs", "http://www.w3.org/2000/01/rdf-schema#");
        add("owl", "http://www.w3.org/2002/07/owl#");
        let shape = (0..)
            .map(|i| {
                if i == 0 {
                    "shape".to_string()
                } else {
                    format!("shape{i}")
                }
            })
            .find(|p| !prefixes.iter().any(|(q, _)| q == p))
            .unwrap_or_default();
        add(&shape, base);
        for (p, ns) in prefixes {
            add(p, ns);
        }
        list.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        Prefixes { list }
    }

    /// The IRI as a prefixed name or `<iri>`; records the prefix used.
    fn iri(&self, iri: &str, used: &mut BTreeSet<usize>) -> String {
        for (i, (p, ns)) in self.list.iter().enumerate() {
            if let Some(local) = iri.strip_prefix(ns.as_str())
                && simple_local(local)
            {
                used.insert(i);
                return format!("{p}:{local}");
            }
        }
        format!("<{iri}>")
    }

    /// `@prefix` (Turtle) or `PREFIX` (ShExC) lines for the prefixes used.
    fn header(&self, used: &BTreeSet<usize>, turtle: bool) -> String {
        let mut lines: Vec<String> = used
            .iter()
            .map(|&i| {
                let (p, ns) = &self.list[i];
                if turtle {
                    format!("@prefix {p}: <{ns}> .")
                } else {
                    format!("PREFIX {p}: <{ns}>")
                }
            })
            .collect();
        lines.sort();
        let mut out = lines.join("\n");
        out.push('\n');
        out
    }
}

/// A term in N-Triples syntax (from the draft) with its IRIs compacted.
fn term(t: &str, names: &Prefixes, used: &mut BTreeSet<usize>) -> String {
    if let Some(iri) = t.strip_prefix('<').and_then(|i| i.strip_suffix('>')) {
        return names.iri(iri, used);
    }
    // a typed literal: compact its datatype
    if let Some(at) = t.rfind("^^<")
        && t.ends_with('>')
    {
        let dt = &t[at + 3..t.len() - 1];
        return format!("{}^^{}", &t[..at], names.iri(dt, used));
    }
    t.to_string()
}

fn plural(n: u64, one: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {one}s")
    }
}

fn header_comment(d: &ShapesDraft, out: &mut String, what: &str) {
    let _ = writeln!(
        out,
        "# {what} drafted by Sparkles from dataset \"{}\" at version {}, generation {}.",
        d.dataset.replace(['\n', '\r'], " "),
        d.snapshot.version,
        d.snapshot.generation
    );
    let _ = writeln!(
        out,
        "# Graph {}{}, support {}{}. Counts are of the instances each constraint applies to.",
        d.selection.graph,
        if d.selection.reasoning {
            " with inferences"
        } else {
            ""
        },
        d.options.support,
        if d.options.closed {
            ", closed shapes"
        } else {
            ""
        },
    );
}

/// The comment after a constraint: how many satisfy it of how many.
fn note(c: &ConstraintDraft) -> String {
    if c.excluded == 0 && c.applicable == 1 {
        "the 1 instance".into()
    } else if c.excluded == 0 {
        format!("all {} instances", c.applicable)
    } else {
        format!(
            "{} of {} instances, excludes {}",
            c.satisfied, c.applicable, c.excluded
        )
    }
}

/// A SHACL constraint's predicate and object.
fn shacl_constraint(
    c: &ConstraintDraft,
    names: &Prefixes,
    used: &mut BTreeSet<usize>,
) -> (String, String) {
    let obj = match &c.value {
        ConstraintValue::Count(n) => n.to_string(),
        ConstraintValue::Iri(i) => names.iri(i, used),
        ConstraintValue::Bool(b) => b.to_string(),
        ConstraintValue::List(items) if c.component == "languageIn" => format!(
            "( {} )",
            items
                .iter()
                .map(|l| format!("\"{l}\""))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        ConstraintValue::List(items) => format!(
            "( {} )",
            items
                .iter()
                .map(|t| term(t, names, used))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    };
    (names.iri(&format!("{SH}{}", c.component), used), obj)
}

fn shacl_property(p: &PropertyDraft, names: &Prefixes, used: &mut BTreeSet<usize>) -> String {
    let mut lines = vec![format!(
        "sh:path {} ;  # {} with a value",
        names.iri(&p.path, used),
        plural(p.instances, "instance")
    )];
    for c in &p.constraints {
        let (pred, obj) = shacl_constraint(c, names, used);
        lines.push(format!("{pred} {obj} ;  # {}", note(c)));
    }
    for c in &p.rejected {
        let (pred, obj) = shacl_constraint(c, names, used);
        lines.push(format!(
            "# not drafted: {pred} {obj}  ({} of {} instances, would exclude {})",
            c.satisfied, c.applicable, c.excluded
        ));
    }
    let mut out = String::from("    sh:property [\n");
    for l in lines {
        let _ = writeln!(out, "        {l}");
    }
    out.push_str("    ]");
    out
}

/// The shapes graph in Turtle.
pub fn shacl(d: &ShapesDraft, names: &Prefixes) -> String {
    let mut used = BTreeSet::new();
    let mut body = String::new();
    for s in &d.shapes {
        let _ = writeln!(body);
        let _ = writeln!(body, "{}", names.iri(&s.shape, &mut used));
        let _ = writeln!(body, "    a sh:NodeShape ;");
        let mut parts = vec![format!(
            "    sh:targetClass {}",
            names.iri(&s.class, &mut used)
        )];
        let mut first_note = Some(format!("  # {}", plural(s.instances, "instance")));
        if s.closed {
            parts.push("    sh:closed true".into());
            parts.push(format!(
                "    sh:ignoredProperties ( {} )",
                names.iri(&format!("{RDF}type"), &mut used)
            ));
        }
        for p in &s.properties {
            parts.push(shacl_property(p, names, &mut used));
        }
        let n = parts.len();
        for (i, part) in parts.into_iter().enumerate() {
            let end = if i + 1 == n { " ." } else { " ;" };
            let comment = first_note.take().unwrap_or_default();
            let _ = writeln!(body, "{part}{end}{comment}");
        }
    }
    // sh: and rdf: are always declared: the body uses them
    used.extend(
        names
            .list
            .iter()
            .enumerate()
            .filter(|(_, (p, _))| p == "sh")
            .map(|(i, _)| i),
    );
    let mut out = String::new();
    header_comment(d, &mut out, "SHACL shapes");
    out.push_str(&names.header(&used, true));
    out.push_str(&body);
    out
}

/// The ShExC value expression of a property, or `.`.
fn shex_value(
    p: &PropertyDraft,
    names: &Prefixes,
    used: &mut BTreeSet<usize>,
    subclasses: &dyn Fn(&str) -> Vec<String>,
) -> String {
    let get = |k: &str| p.constraints.iter().find(|c| c.component == k);
    if let Some(ConstraintValue::List(items)) = get("in").map(|c| &c.value) {
        let vals: Vec<String> = items.iter().map(|t| term(t, names, used)).collect();
        return format!("[{}]", vals.join(" "));
    }
    if let Some(ConstraintValue::List(tags)) = get("languageIn").map(|c| &c.value) {
        let vals: Vec<String> = tags.iter().map(|t| format!("@{t}")).collect();
        return format!("[{}]", vals.join(" "));
    }
    if let Some(ConstraintValue::Iri(dt)) = get("datatype").map(|c| &c.value) {
        return names.iri(dt, used);
    }
    let kind = get("nodeKind").and_then(|c| match &c.value {
        ConstraintValue::Iri(k) => k.strip_prefix(SH),
        _ => None,
    });
    let kind = kind.map(|k| match k {
        "IRI" => "IRI",
        "BlankNode" => "BNODE",
        "Literal" => "LITERAL",
        "BlankNodeOrIRI" => "NONLITERAL",
        "IRIOrLiteral" => "(IRI OR LITERAL)",
        _ => "(BNODE OR LITERAL)",
    });
    let class = get("class").and_then(|c| match &c.value {
        ConstraintValue::Iri(k) => Some(k.as_str()),
        _ => None,
    });
    match (kind, class) {
        (k, Some(class)) => {
            let ty = names.iri(&format!("{RDF}type"), used);
            let set: Vec<String> = subclasses(class)
                .iter()
                .map(|c| names.iri(c, used))
                .collect();
            let shape = format!("EXTRA {ty} {{ {ty} [{}] + }}", set.join(" "));
            match k {
                Some(k) => format!("({k} AND {shape})"),
                None => format!("({shape})"),
            }
        }
        (Some(k), None) => k.to_string(),
        (None, None) => ".".into(),
    }
}

fn cardinality(p: &PropertyDraft) -> String {
    let count = |k: &str| {
        p.constraints
            .iter()
            .find(|c| c.component == k)
            .and_then(|c| match c.value {
                ConstraintValue::Count(n) => Some(n),
                _ => None,
            })
    };
    match (count("minCount").unwrap_or(0), count("maxCount")) {
        (0, None) => "*".into(),
        (1, None) => "+".into(),
        (0, Some(1)) => "?".into(),
        (m, None) => format!("{{{m},*}}"),
        (m, Some(x)) if m == x => format!("{{{m}}}"),
        (m, Some(x)) => format!("{{{m},{x}}}"),
    }
}

fn shex_shape(
    s: &NodeShapeDraft,
    names: &Prefixes,
    used: &mut BTreeSet<usize>,
    subclasses: &dyn Fn(&str) -> Vec<String>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# {}: {}",
        names.iri(&s.class, used),
        plural(s.instances, "instance")
    );
    let _ = write!(out, "{}", names.iri(&s.shape, used));
    if s.closed {
        out.push_str(" CLOSED");
    }
    out.push_str(" {\n");
    let mut items: Vec<String> = Vec::new();
    for p in &s.properties {
        let mut line = format!(
            "  {} {} {}",
            names.iri(&p.path, used),
            shex_value(p, names, used, subclasses),
            cardinality(p)
        );
        if p.constraints.iter().any(|c| c.component == "uniqueLang") {
            line.push_str("  # sh:uniqueLang has no ShEx counterpart");
        }
        items.push(line);
    }
    if s.closed {
        items.push(format!("  {} . *", names.iri(&format!("{RDF}type"), used)));
    }
    // ` ;` separates triple constraints; a comment ends the line
    let n = items.len();
    for (i, item) in items.into_iter().enumerate() {
        let sep = if i + 1 < n { " ;" } else { "" };
        match item.split_once("  # ") {
            Some((tc, comment)) => {
                let _ = writeln!(out, "{tc}{sep}  # {comment}");
            }
            None => {
                let _ = writeln!(out, "{item}{sep}");
            }
        }
    }
    out.push_str("}\n");
    out
}

/// The ShEx schema in ShExC.
pub fn shexc(
    d: &ShapesDraft,
    names: &Prefixes,
    subclasses: &dyn Fn(&str) -> Vec<String>,
) -> String {
    let mut used = BTreeSet::new();
    let mut body = String::new();
    for s in &d.shapes {
        body.push('\n');
        body.push_str(&shex_shape(s, names, &mut used, subclasses));
    }
    let mut out = String::new();
    header_comment(d, &mut out, "ShEx schema");
    out.push_str("# Validate it with the query shape map that comes with it.\n");
    out.push_str(&names.header(&used, false));
    out.push_str(&body);
    out
}

/// The query shape map: `{FOCUS rdf:type <C>}@<shape>` per shape, with full IRIs.
pub fn shape_map(d: &ShapesDraft, _names: &Prefixes) -> String {
    d.shapes
        .iter()
        .map(|s| format!("{{FOCUS <{RDF}type> <{}>}}@<{}>", s.class, s.shape))
        .collect::<Vec<_>>()
        .join(",\n")
}
