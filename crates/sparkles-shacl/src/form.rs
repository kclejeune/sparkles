//! The fields of an editing form generated from shapes (spec U02 §2.2, §4.1, §4.2): the
//! property shapes that apply to a focus node, merged by path, with the constraints a
//! browser can check, the non-validating characteristics that lay the form out, and the
//! widget each field gets.
//!
//! The property shapes of a node shape are those of its `sh:property` and those reached
//! through `sh:node` and `sh:and`, which apply to the same focus node. A property shape
//! reached through `sh:node` that names no `sh:group` of its own is placed in a group
//! named after the node shape it was reached through. Shapes reached through `sh:or`,
//! `sh:xone`, `sh:not` or qualified value shapes impose nothing on their own and give no
//! field. Two property shapes with the same path make one field that applies both, with
//! the stricter value of each count, length and range.

use crate::path::PropertyPath;
use crate::shapes::{Constraint, NodeKind, ShapeId, Shapes};
use oxrdf::{Literal, NamedNode, Term};
use rustc_hash::FxHashSet;
use sparkles_core::sparql::value::Value;
use std::cmp::Ordering;
use std::collections::BTreeSet;

/// The path of a field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldPath {
    /// a predicate path: the values are objects of the focus node
    Predicate(NamedNode),
    /// `sh:inversePath` of a predicate: the values are subjects with the focus as object
    Inverse(NamedNode),
    /// any other path, shown read-only because a change has no single triple to write
    Complex(PropertyPath),
}

impl FieldPath {
    fn of(p: &PropertyPath) -> FieldPath {
        match p {
            PropertyPath::Predicate(n) => FieldPath::Predicate(n.clone()),
            PropertyPath::Inverse(x) => match x.as_ref() {
                PropertyPath::Predicate(n) => FieldPath::Inverse(n.clone()),
                _ => FieldPath::Complex(p.clone()),
            },
            p => FieldPath::Complex(p.clone()),
        }
    }

    /// The path in SPARQL syntax (`<p>`, `^<p>`, or a complex path), which is the id of
    /// its field.
    pub fn sparql(&self) -> String {
        match self {
            FieldPath::Predicate(n) => format!("<{}>", n.as_str()),
            FieldPath::Inverse(n) => format!("^<{}>", n.as_str()),
            FieldPath::Complex(p) => p.to_sparql(),
        }
    }

    /// The predicate of a predicate or inverse path.
    pub fn predicate(&self) -> Option<&NamedNode> {
        match self {
            FieldPath::Predicate(n) | FieldPath::Inverse(n) => Some(n),
            FieldPath::Complex(_) => None,
        }
    }

    /// Whether a result path names this field's path.
    pub fn matches(&self, p: &PropertyPath) -> bool {
        FieldPath::of(p) == *self
    }
}

/// The group a field is placed in.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FieldGroup {
    /// a `sh:PropertyGroup` the property shape names with `sh:group`
    Group(Term),
    /// the node shape the property shape was reached through with `sh:node`
    Shape(Term),
}

impl FieldGroup {
    pub fn node(&self) -> &Term {
        match self {
            FieldGroup::Group(t) | FieldGroup::Shape(t) => t,
        }
    }
}

/// A bound of a range constraint: the term, and its value for comparisons.
#[derive(Clone, Debug)]
pub struct Bound {
    pub term: Term,
    value: Value,
}

/// The constraints of a field, merged over its property shapes.
#[derive(Clone, Debug, Default)]
pub struct FieldConstraints {
    pub min_count: Option<u64>,
    pub max_count: Option<u64>,
    /// `sh:datatype` (the first, when shapes disagree; the server checks them all)
    pub datatype: Option<NamedNode>,
    pub node_kind: Option<NodeKind>,
    /// `sh:class`, every class
    pub classes: Vec<NamedNode>,
    /// `sh:in`: the values allowed by every shape that has one
    pub allowed: Option<Vec<Term>>,
    /// `sh:hasValue`
    pub has_value: Vec<Term>,
    pub min_inclusive: Option<Bound>,
    pub min_exclusive: Option<Bound>,
    pub max_inclusive: Option<Bound>,
    pub max_exclusive: Option<Bound>,
    pub min_length: Option<u64>,
    pub max_length: Option<u64>,
    /// `sh:pattern` with `sh:flags`, every pattern
    pub patterns: Vec<(String, Option<String>)>,
    /// `sh:languageIn`: the tags every shape that has one allows
    pub language_in: Option<Vec<String>>,
    pub unique_lang: bool,
    /// `sh:node` on the values
    pub node: Vec<Term>,
    /// the components only the server checks (`sh:sparql`, `sh:qualifiedValueShape`, …)
    pub server: BTreeSet<String>,
}

/// One field: the property shapes of one path.
#[derive(Clone, Debug)]
pub struct FormField {
    pub path: FieldPath,
    /// the property shapes, in the order they were reached
    pub sources: Vec<ShapeId>,
    pub names: Vec<Literal>,
    pub descriptions: Vec<Literal>,
    pub group: Option<FieldGroup>,
    pub order: Option<f64>,
    pub read_only: bool,
    pub editor: Option<NamedNode>,
    pub viewer: Option<NamedNode>,
    pub single_line: Option<bool>,
    pub default_value: Option<Term>,
    pub constraints: FieldConstraints,
}

impl FormField {
    /// The field's id: its path in SPARQL syntax.
    pub fn id(&self) -> String {
        self.path.sparql()
    }

    /// The widget the constraints choose (§2.3), before a `dash:editor` overrides it.
    pub fn widget(&self) -> &'static str {
        let c = &self.constraints;
        if let Some(values) = &c.allowed {
            return if values.len() <= 6 { "radio" } else { "select" };
        }
        let dt = c.datatype.as_ref().map(|d| d.as_str());
        let xsd = |local: &str| dt == Some(&*format!("{XSD}{local}"));
        if xsd("boolean") {
            return if c.min_count.unwrap_or(0) >= 1 {
                "checkbox"
            } else {
                "boolean-select"
            };
        }
        if xsd("date") {
            return "date";
        }
        if xsd("dateTime") || xsd("dateTimeStamp") {
            return "datetime";
        }
        if dt.is_some_and(is_numeric) {
            return "number";
        }
        if dt == Some(RDF_LANG_STRING) || c.language_in.is_some() {
            return "text-lang";
        }
        let string = dt.is_none_or(|d| d == format!("{XSD}string"));
        if string
            && (self.single_line == Some(false) || c.max_length.is_some_and(|m| m > 200))
            && (dt.is_some() || c.node_kind == Some(NodeKind::Literal))
        {
            return "textarea";
        }
        if dt.is_some() {
            return "text";
        }
        if !c.classes.is_empty() || matches!(c.node_kind, Some(NodeKind::Iri)) {
            return "entity";
        }
        if !c.node.is_empty() && matches!(c.node_kind, Some(NodeKind::BlankNode)) {
            return "nested";
        }
        "term"
    }
}

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";

/// Whether a datatype IRI is a numeric XSD type.
pub fn is_numeric(dt: &str) -> bool {
    matches!(
        dt.strip_prefix(XSD),
        Some(
            "integer"
                | "decimal"
                | "float"
                | "double"
                | "long"
                | "int"
                | "short"
                | "byte"
                | "nonNegativeInteger"
                | "nonPositiveInteger"
                | "positiveInteger"
                | "negativeInteger"
                | "unsignedLong"
                | "unsignedInt"
                | "unsignedShort"
                | "unsignedByte"
        )
    )
}

/// The fields of a form for the focus nodes of `node_shapes`.
#[derive(Clone, Debug, Default)]
pub struct FormLayout {
    /// the fields, in the order their property shapes were reached
    pub fields: Vec<FormField>,
    /// some node shape is `sh:closed true`
    pub closed: bool,
    /// the predicates a closed shape allows besides its property shapes'
    /// (`sh:ignoredProperties`)
    pub ignored: Vec<Term>,
}

/// The fields of the property shapes that apply to the focus nodes of `node_shapes`.
pub fn form_layout(shapes: &Shapes, node_shapes: &[ShapeId]) -> FormLayout {
    let mut walk = Walk {
        shapes,
        visited: FxHashSet::default(),
        out: FormLayout::default(),
    };
    for &si in node_shapes {
        walk.shape(si, None);
    }
    walk.out
}

struct Walk<'s> {
    shapes: &'s Shapes,
    visited: FxHashSet<ShapeId>,
    out: FormLayout,
}

impl Walk<'_> {
    fn shape(&mut self, si: ShapeId, via: Option<&Term>) {
        let shape = &self.shapes.shapes()[si];
        if shape.deactivated || !self.visited.insert(si) {
            return;
        }
        if shape.is_property_shape() {
            self.property(si, via);
            return;
        }
        for c in &shape.constraints {
            match c {
                Constraint::Property(ps) => {
                    if self.visited.insert(*ps) && !self.shapes.shapes()[*ps].deactivated {
                        self.property(*ps, via);
                    }
                }
                Constraint::Node(n) => {
                    let node = self.shapes.shapes()[*n].node.clone();
                    // a property shape reached through two node shapes keeps the first
                    self.shape(*n, Some(via.unwrap_or(&node)));
                }
                Constraint::And(list) => {
                    for n in list {
                        self.shape(*n, via);
                    }
                }
                Constraint::Closed { ignored, .. } => {
                    self.out.closed = true;
                    for t in ignored {
                        if !self.out.ignored.contains(t) {
                            self.out.ignored.push(t.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn property(&mut self, ps: ShapeId, via: Option<&Term>) {
        let shape = &self.shapes.shapes()[ps];
        let Some(p) = &shape.path else { return };
        let path = FieldPath::of(p);
        let info = shape.info();
        let i = match self.out.fields.iter().position(|f| f.path == path) {
            Some(i) => i,
            None => {
                self.out.fields.push(FormField {
                    path,
                    sources: Vec::new(),
                    names: Vec::new(),
                    descriptions: Vec::new(),
                    group: None,
                    order: None,
                    read_only: false,
                    editor: None,
                    viewer: None,
                    single_line: None,
                    default_value: None,
                    constraints: FieldConstraints::default(),
                });
                self.out.fields.len() - 1
            }
        };
        let f = &mut self.out.fields[i];
        f.sources.push(ps);
        f.names.extend(info.names.iter().cloned());
        f.descriptions.extend(info.descriptions.iter().cloned());
        if f.group.is_none() {
            f.group = match (&info.group, via) {
                (Some(g), _) => Some(FieldGroup::Group(g.clone())),
                (None, Some(n)) => Some(FieldGroup::Shape(n.clone())),
                (None, None) => None,
            };
        }
        f.order = match (f.order, info.order) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        f.read_only |= info.read_only;
        f.editor = f.editor.take().or_else(|| info.editor.clone());
        f.viewer = f.viewer.take().or_else(|| info.viewer.clone());
        f.single_line = f.single_line.or(info.single_line);
        f.default_value = f
            .default_value
            .take()
            .or_else(|| info.default_value.clone());
        let shapes = self.shapes;
        merge(&mut f.constraints, shapes, &shape.constraints);
    }
}

fn merge(c: &mut FieldConstraints, shapes: &Shapes, list: &[Constraint]) {
    let min = |a: Option<u64>, b: u64| Some(a.map_or(b, |a| a.min(b)));
    let max = |a: Option<u64>, b: u64| Some(a.map_or(b, |a| a.max(b)));
    for k in list {
        match k {
            Constraint::MinCount(n) => c.min_count = max(c.min_count, *n),
            Constraint::MaxCount(n) => c.max_count = min(c.max_count, *n),
            Constraint::MinLength(n) => c.min_length = max(c.min_length, *n),
            Constraint::MaxLength(n) => c.max_length = min(c.max_length, *n),
            Constraint::Datatype(d) => {
                if c.datatype.is_none() {
                    c.datatype = Some(d.clone());
                } else if c.datatype.as_ref() != Some(d) {
                    c.server.insert(k.component().into_string());
                }
            }
            Constraint::NodeKind(n) => {
                if c.node_kind.is_none() {
                    c.node_kind = Some(*n);
                }
            }
            Constraint::Class(t) => match shapes.term(*t) {
                Term::NamedNode(n) if !c.classes.contains(n) => c.classes.push(n.clone()),
                Term::NamedNode(_) => {}
                _ => {
                    c.server.insert(k.component().into_string());
                }
            },
            Constraint::In(ts) => {
                let values: Vec<Term> = ts.iter().map(|t| shapes.term(*t).clone()).collect();
                c.allowed = Some(match c.allowed.take() {
                    None => values,
                    Some(old) => old.into_iter().filter(|v| values.contains(v)).collect(),
                });
            }
            Constraint::HasValue(t) => {
                let t = shapes.term(*t).clone();
                if !c.has_value.contains(&t) {
                    c.has_value.push(t);
                }
            }
            Constraint::MinInclusive(t, v) => {
                tighter(&mut c.min_inclusive, t, v, Ordering::Greater)
            }
            Constraint::MinExclusive(t, v) => {
                tighter(&mut c.min_exclusive, t, v, Ordering::Greater)
            }
            Constraint::MaxInclusive(t, v) => tighter(&mut c.max_inclusive, t, v, Ordering::Less),
            Constraint::MaxExclusive(t, v) => tighter(&mut c.max_exclusive, t, v, Ordering::Less),
            Constraint::Pattern(p) => {
                let pf = (p.pattern.clone(), p.flags.clone());
                if !c.patterns.contains(&pf) {
                    c.patterns.push(pf);
                }
            }
            Constraint::LanguageIn(tags) => {
                c.language_in = Some(match c.language_in.take() {
                    None => tags.clone(),
                    Some(old) => old
                        .into_iter()
                        .filter(|t| tags.iter().any(|x| x.eq_ignore_ascii_case(t)))
                        .collect(),
                });
            }
            Constraint::UniqueLang => c.unique_lang = true,
            Constraint::Node(n) => {
                let t = shapes.shapes()[*n].node.clone();
                if !c.node.contains(&t) {
                    c.node.push(t);
                }
                c.server.insert(k.component().into_string());
            }
            _ => {
                c.server.insert(k.component().into_string());
            }
        }
    }
}

/// Keep the stricter of two bounds: the one `want` of the other (a greater lower bound,
/// a smaller upper bound). Bounds that do not compare keep the first.
fn tighter(slot: &mut Option<Bound>, t: &Term, v: &Value, want: Ordering) {
    match slot {
        Some(b) => {
            if matches!(sparkles_core::sparql::value::compare(v, &b.value), Ok(Some(o)) if o == want)
            {
                *b = Bound {
                    term: t.clone(),
                    value: v.clone(),
                };
            }
        }
        None => {
            *slot = Some(Bound {
                term: t.clone(),
                value: v.clone(),
            })
        }
    }
}

/// Pick a literal of `list` for the language `lang`: that language, then English, then
/// an untagged literal, then any.
pub fn pick_lang<'a>(list: &'a [Literal], lang: Option<&str>) -> Option<&'a Literal> {
    let tagged = |want: &str| {
        list.iter().find(|l| {
            l.language()
                .is_some_and(|t| t.eq_ignore_ascii_case(want) || t.split('-').next() == Some(want))
        })
    };
    lang.and_then(tagged)
        .or_else(|| tagged("en"))
        .or_else(|| list.iter().find(|l| l.language().is_none()))
        .or_else(|| list.first())
}
