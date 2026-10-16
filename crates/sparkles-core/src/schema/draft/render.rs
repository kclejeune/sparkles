//! The draft as text: a SHACL shapes graph in Turtle and in the SHACL Compact Syntax, a
//! ShEx schema in ShExC and the query shape map that goes with it. The counts go into
//! comments.

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

/// Whether any property of the draft refers to the shape of a class.
fn has_references(d: &ShapesDraft) -> bool {
    d.shapes
        .iter()
        .flat_map(|s| &s.properties)
        .any(|p| p.constraints.iter().any(|c| c.component == "node"))
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
    if has_references(d) {
        let _ = writeln!(
            out,
            "# The shapes refer to each other for the classes of values, and the references may form cycles. Both validators handle recursion."
        );
    }
}

/// The comment after a constraint: how many satisfy it of how many. A bound of a range
/// says it is the observed one.
fn note(c: &ConstraintDraft) -> String {
    let counts = if c.excluded == 0 && c.applicable == 1 {
        "the 1 instance".to_string()
    } else if c.excluded == 0 {
        format!("all {} instances", c.applicable)
    } else {
        format!(
            "{} of {} instances, excludes {}",
            c.satisfied, c.applicable, c.excluded
        )
    };
    match c.component {
        "minInclusive" => format!("{counts}, the observed minimum, not an intended one"),
        "maxInclusive" => format!("{counts}, the observed maximum, not an intended one"),
        _ => counts,
    }
}

/// A string literal for Turtle and SHACLC.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
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
        ConstraintValue::Term(t) => term(t, names, used),
        ConstraintValue::Text(t) => quoted(t),
        ConstraintValue::Bool(b) => b.to_string(),
        ConstraintValue::List(items) if c.component == "or" => format!(
            "( {} )",
            items
                .iter()
                .map(|d| format!("[ sh:datatype {} ]", names.iri(d, used)))
                .collect::<Vec<_>>()
                .join(" ")
        ),
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

/// The path of a property shape in Turtle.
fn shacl_path(p: &PropertyDraft, names: &Prefixes, used: &mut BTreeSet<usize>) -> String {
    let first = names.iri(&p.path, used);
    match (&p.then, p.inverse) {
        (Some(q), _) => format!("( {first} {} )", names.iri(q, used)),
        (None, true) => format!("[ sh:inversePath {first} ]"),
        (None, false) => first,
    }
}

/// The path of a property shape in the SHACL Compact Syntax.
fn shaclc_path(p: &PropertyDraft, names: &Prefixes, used: &mut BTreeSet<usize>) -> String {
    let first = names.iri(&p.path, used);
    match (&p.then, p.inverse) {
        (Some(q), _) => format!("{first}/{}", names.iri(q, used)),
        (None, true) => format!("^{first}"),
        (None, false) => first,
    }
}

/// How the instances with a value are described: of the class, the subjects that point
/// at them, or the values at the end of a path.
fn with_a_value(p: &PropertyDraft) -> &'static str {
    if p.inverse {
        "with a subject pointing at it"
    } else {
        "with a value"
    }
}

fn shacl_property(p: &PropertyDraft, names: &Prefixes, used: &mut BTreeSet<usize>) -> String {
    let mut lines = vec![format!(
        "sh:path {} ;  # {} {}",
        shacl_path(p, names, used),
        plural(p.instances, "instance"),
        with_a_value(p)
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

/// Whether SHACLC reads a property type IRI as `sh:datatype` (the `xsd:` namespace and
/// the RDF datatypes); otherwise as `sh:class`.
fn shaclc_datatype(iri: &str) -> bool {
    iri.starts_with(XSD)
        || ["langString", "HTML", "JSON", "XMLLiteral"]
            .iter()
            .any(|l| iri.strip_prefix(RDF) == Some(*l))
}

/// One SHACLC atom for a constraint (the counts are written together, elsewhere).
fn shaclc_atom(c: &ConstraintDraft, names: &Prefixes, used: &mut BTreeSet<usize>) -> String {
    match (c.component, &c.value) {
        ("nodeKind", ConstraintValue::Iri(k)) => match k.strip_prefix(SH) {
            Some(kind) => kind.to_string(),
            None => format!("nodeKind={}", names.iri(k, used)),
        },
        ("datatype", ConstraintValue::Iri(d)) if shaclc_datatype(d) => names.iri(d, used),
        ("class", ConstraintValue::Iri(k)) if !shaclc_datatype(k) => names.iri(k, used),
        ("node", ConstraintValue::Iri(s)) => format!("@{}", names.iri(s, used)),
        ("or", ConstraintValue::List(items)) => items
            .iter()
            .map(|d| names.iri(d, used))
            .collect::<Vec<_>>()
            .join(" | "),
        (name, ConstraintValue::Term(t)) => format!("{name}={}", term(t, names, used)),
        (name, ConstraintValue::Text(t)) => format!("{name}={}", quoted(t)),
        (name, ConstraintValue::Iri(i)) => format!("{name}={}", names.iri(i, used)),
        (name, ConstraintValue::Count(n)) => format!("{name}={n}"),
        (name, ConstraintValue::Bool(b)) => format!("{name}={b}"),
        ("languageIn", ConstraintValue::List(tags)) => format!(
            "languageIn=[{}]",
            tags.iter()
                .map(|l| format!("\"{l}\""))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        (name, ConstraintValue::List(items)) => format!(
            "{name}=[{}]",
            items
                .iter()
                .map(|t| term(t, names, used))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    }
}

/// A property shape in SHACLC: the path, then one atom per line with its counts as a
/// comment, and the rejected candidates as comments.
fn shaclc_property(p: &PropertyDraft, names: &Prefixes, used: &mut BTreeSet<usize>) -> String {
    // (atom, comment)
    let mut atoms: Vec<(String, String)> = Vec::new();
    let find = |k: &str| p.constraints.iter().find(|c| c.component == k);
    let (min, max) = (find("minCount"), find("maxCount"));
    if min.is_some() || max.is_some() {
        let n = |c: Option<&ConstraintDraft>, none: &str| match c.map(|c| &c.value) {
            Some(ConstraintValue::Count(n)) => n.to_string(),
            _ => none.to_string(),
        };
        let notes: Vec<String> = [("minCount", min), ("maxCount", max)]
            .into_iter()
            .filter_map(|(k, c)| c.map(|c| format!("{k}: {}", note(c))))
            .collect();
        atoms.push((
            format!("[{}..{}]", n(min, "0"), n(max, "*")),
            notes.join("; "),
        ));
    }
    for c in &p.constraints {
        if c.component != "minCount" && c.component != "maxCount" {
            atoms.push((shaclc_atom(c, names, used), note(c)));
        }
    }
    let mut out = format!(
        "    {}  # {} {}\n",
        shaclc_path(p, names, used),
        plural(p.instances, "instance"),
        with_a_value(p)
    );
    let n = atoms.len();
    for (i, (atom, comment)) in atoms.into_iter().enumerate() {
        let end = if i + 1 == n { " ." } else { "" };
        let _ = writeln!(out, "        {atom}{end}  # {comment}");
    }
    if n == 0 {
        out.push_str("        .\n");
    }
    for c in &p.rejected {
        let atom = match (c.component, &c.value) {
            ("minCount" | "maxCount", ConstraintValue::Count(n)) => {
                format!("sh:{} {n}", c.component)
            }
            _ => shaclc_atom(c, names, used),
        };
        let _ = writeln!(
            out,
            "        # not drafted: {atom}  ({} of {} instances, would exclude {})",
            c.satisfied, c.applicable, c.excluded
        );
    }
    out
}

/// The shapes graph in the SHACL Compact Syntax, with the counts as comments.
pub fn shaclc(d: &ShapesDraft, names: &Prefixes) -> String {
    let mut used = BTreeSet::new();
    let mut body = String::new();
    for s in &d.shapes {
        let _ = writeln!(body);
        let _ = writeln!(
            body,
            "shape {} -> {} {{  # {}",
            names.iri(&s.shape, &mut used),
            names.iri(&s.class, &mut used),
            plural(s.instances, "instance")
        );
        if s.closed {
            let _ = writeln!(
                body,
                "    closed=true ignoredProperties=[{}] .",
                names.iri(&format!("{RDF}type"), &mut used)
            );
        }
        for p in &s.properties {
            body.push_str(&shaclc_property(p, names, &mut used));
        }
        body.push_str("}\n");
    }
    let mut out = String::new();
    header_comment(d, &mut out, "SHACL shapes");
    out.push_str(&names.header(&used, false));
    out.push_str(&body);
    out
}

/// A number as a ShExC numeric literal, from a term in N-Triples syntax (`None` when it
/// is not a finite integer, decimal or double).
fn shex_number(t: &str) -> Option<String> {
    let rest = t.strip_prefix('"')?;
    let (lex, dt) = rest.split_once("\"^^<")?;
    let dt = dt.strip_suffix('>')?.strip_prefix(XSD)?;
    if !matches!(dt, "integer" | "decimal" | "double") {
        return None;
    }
    let body = lex.strip_prefix(['+', '-']).unwrap_or(lex);
    let (mantissa, exp) = match body.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e.strip_prefix(['+', '-']).unwrap_or(e))),
        None => (body, None),
    };
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    let ok_mantissa = match mantissa.split_once('.') {
        Some((i, f)) => {
            (digits(i) || i.is_empty()) && (digits(f) || f.is_empty()) && (digits(i) || digits(f))
        }
        None => digits(mantissa),
    };
    (ok_mantissa && exp.is_none_or(digits)).then(|| lex.to_string())
}

/// The ShExC value expression of a property, or `.`, and the comments for constraints
/// that have no ShEx counterpart.
fn shex_value(
    p: &PropertyDraft,
    names: &Prefixes,
    used: &mut BTreeSet<usize>,
    subclasses: &dyn Fn(&str) -> Vec<String>,
) -> (String, Vec<String>) {
    let get = |k: &str| p.constraints.iter().find(|c| c.component == k);
    let mut notes = Vec::new();
    // the facets: a pattern, and the bounds of a range of numbers
    let mut facets: Vec<String> = Vec::new();
    if let Some(ConstraintValue::Text(re)) = get("pattern").map(|c| &c.value) {
        facets.push(format!("/{}/", re.replace('/', "\\/")));
    }
    for (component, keyword) in [
        ("minInclusive", "MININCLUSIVE"),
        ("maxInclusive", "MAXINCLUSIVE"),
    ] {
        if let Some(ConstraintValue::Term(t)) = get(component).map(|c| &c.value) {
            match shex_number(t) {
                Some(n) => facets.push(format!("{keyword} {n}")),
                None => notes.push(format!(
                    "sh:{component} {} has no ShEx counterpart",
                    term(t, names, used)
                )),
            }
        }
    }
    let facets = facets.join(" ");
    let with = |base: String| {
        if facets.is_empty() {
            base
        } else {
            format!("{base} {facets}")
        }
    };
    if let Some(ConstraintValue::List(items)) = get("in").map(|c| &c.value) {
        let vals: Vec<String> = items.iter().map(|t| term(t, names, used)).collect();
        return (with(format!("[{}]", vals.join(" "))), notes);
    }
    if let Some(ConstraintValue::List(tags)) = get("languageIn").map(|c| &c.value) {
        let vals: Vec<String> = tags.iter().map(|t| format!("@{t}")).collect();
        return (with(format!("[{}]", vals.join(" "))), notes);
    }
    if let Some(ConstraintValue::Iri(dt)) = get("datatype").map(|c| &c.value) {
        return (with(names.iri(dt, used)), notes);
    }
    let mut parts: Vec<String> = Vec::new();
    let kind = get("nodeKind").and_then(|c| match &c.value {
        ConstraintValue::Iri(k) => k.strip_prefix(SH),
        _ => None,
    });
    if let Some(k) = kind {
        parts.push(
            match k {
                "IRI" => "IRI",
                "BlankNode" => "BNODE",
                "Literal" => "LITERAL",
                "BlankNodeOrIRI" => "NONLITERAL",
                "IRIOrLiteral" => "(IRI OR LITERAL)",
                _ => "(BNODE OR LITERAL)",
            }
            .to_string(),
        );
    }
    if let Some(ConstraintValue::List(items)) = get("or").map(|c| &c.value) {
        let alts: Vec<String> = items.iter().map(|d| names.iri(d, used)).collect();
        parts.push(format!("({})", alts.join(" OR ")));
    }
    let node = get("node").and_then(|c| match &c.value {
        ConstraintValue::Iri(s) => Some(s.as_str()),
        _ => None,
    });
    let class = get("class").and_then(|c| match &c.value {
        ConstraintValue::Iri(k) => Some(k.as_str()),
        _ => None,
    });
    match (node, class) {
        (Some(shape), _) => parts.push(format!("@{}", names.iri(shape, used))),
        (None, Some(class)) => {
            let ty = names.iri(&format!("{RDF}type"), used);
            let set: Vec<String> = subclasses(class)
                .iter()
                .map(|c| names.iri(c, used))
                .collect();
            parts.push(format!("(EXTRA {ty} {{ {ty} [{}] + }})", set.join(" ")));
        }
        (None, None) => {}
    }
    if !facets.is_empty() {
        parts.push(facets);
    }
    let value = match parts.len() {
        0 => ".".to_string(),
        1 => parts.remove(0),
        _ => format!("({})", parts.join(" AND ")),
    };
    (value, notes)
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
    let mut notes: Vec<String> = Vec::new();
    for p in &s.properties {
        // ShEx has no sequence paths: the shape of the values in the middle would ask
        // for more than the path does
        if let Some(q) = &p.then {
            notes.push(format!(
                "{}/{}: a sequence path has no ShEx counterpart",
                names.iri(&p.path, used),
                names.iri(q, used)
            ));
            continue;
        }
        let (value, more) = shex_value(p, names, used, subclasses);
        let mut path = names.iri(&p.path, used);
        if p.inverse {
            path.insert(0, '^');
        }
        let mut line = format!("  {path} {value} {}", cardinality(p));
        if p.constraints.iter().any(|c| c.component == "uniqueLang") {
            line.push_str("  # sh:uniqueLang has no ShEx counterpart");
        }
        items.push(line);
        notes.extend(more.into_iter().map(|m| format!("{path}: {m}")));
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
    for n in notes {
        let _ = writeln!(out, "  # {n}");
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
