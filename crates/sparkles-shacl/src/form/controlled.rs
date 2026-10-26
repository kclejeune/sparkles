//! Admission of form-owned layout copies, before recursive nested forms expand them.

use super::{FieldPath, FormField, FormLayout, Walk};
use crate::path::PropertyPath;
use crate::shapes::{Constraint, ShapeId, Shapes};
use anyhow::{Result, bail};
use oxrdf::Term;
use rustc_hash::FxHashSet;

type Control<'a> = &'a mut dyn FnMut() -> Result<()>;

/// Build a form layout while admitting preparation work before owned copies.
///
/// `check` consumes one unit for a structural step or 128 lexical bytes. It can
/// reject a cumulative budget or cancellation. An error discards the whole layout.
/// This controls layout preparation, not the physical reader or shape parser.
pub fn form_layout_with(
    shapes: &Shapes,
    node_shapes: &[ShapeId],
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<FormLayout> {
    check()?;
    let mut builder = Controlled {
        walk: Walk {
            shapes,
            visited: FxHashSet::default(),
            out: FormLayout::default(),
        },
        check,
    };
    for &shape in node_shapes {
        builder.shape(shape, None, 0)?;
    }
    Ok(builder.walk.out)
}

struct Controlled<'s, 'c> {
    walk: Walk<'s>,
    check: Control<'c>,
}

fn lexical(text: &str, check: &mut Control<'_>) -> Result<()> {
    for _ in 0..text.len().div_ceil(128) {
        check()?;
    }
    Ok(())
}

fn term(value: &Term, check: &mut Control<'_>, depth: usize) -> Result<()> {
    check()?;
    if depth > 64 {
        bail!("form term nesting exceeds the preparation limit");
    }
    match value {
        Term::NamedNode(n) => lexical(n.as_str(), check),
        Term::BlankNode(b) => lexical(b.as_str(), check),
        Term::Literal(l) => {
            lexical(l.value(), check)?;
            lexical(l.datatype().as_str(), check)?;
            if let Some(language) = l.language() {
                lexical(language, check)?;
            }
            Ok(())
        }
        Term::Triple(t) => {
            // Triple terms only contain a named or blank subject.
            match &t.subject {
                oxrdf::NamedOrBlankNode::NamedNode(n) => lexical(n.as_str(), check)?,
                oxrdf::NamedOrBlankNode::BlankNode(b) => lexical(b.as_str(), check)?,
            }
            lexical(t.predicate.as_str(), check)?;
            term(&t.object, check, depth + 1)
        }
    }
}

fn path(value: &PropertyPath, check: &mut Control<'_>, depth: usize) -> Result<()> {
    check()?;
    if depth > 64 {
        bail!("form path nesting exceeds the preparation limit");
    }
    match value {
        PropertyPath::Predicate(n) => lexical(n.as_str(), check),
        PropertyPath::Sequence(items) | PropertyPath::Alternative(items) => {
            for item in items {
                path(item, check, depth + 1)?;
            }
            Ok(())
        }
        PropertyPath::Inverse(p)
        | PropertyPath::ZeroOrMore(p)
        | PropertyPath::OneOrMore(p)
        | PropertyPath::ZeroOrOne(p) => path(p, check, depth + 1),
    }
}

fn field_path(value: &FieldPath, check: &mut Control<'_>) -> Result<()> {
    check()?;
    match value {
        FieldPath::Predicate(n) | FieldPath::Inverse(n) => lexical(n.as_str(), check),
        FieldPath::Complex(p) => path(p, check, 0),
    }
}

// FieldPath::matches makes an owned path. Admission itself uses only references.
fn same_path(field: &FieldPath, source: &PropertyPath) -> bool {
    match (field, source) {
        (FieldPath::Predicate(a), PropertyPath::Predicate(b)) => a == b,
        (FieldPath::Inverse(a), PropertyPath::Inverse(b)) => {
            matches!(b.as_ref(), PropertyPath::Predicate(n) if a == n)
        }
        (FieldPath::Complex(a), b) => a == b,
        _ => false,
    }
}

impl Controlled<'_, '_> {
    fn shape(&mut self, index: ShapeId, via: Option<&Term>, depth: usize) -> Result<()> {
        (self.check)()?;
        if depth > 64 {
            bail!("form shape nesting exceeds the preparation limit");
        }
        let shapes = self.walk.shapes;
        let shape = &shapes.shapes()[index];
        if shape.deactivated || !self.walk.visited.insert(index) {
            return Ok(());
        }
        if shape.is_property_shape() {
            return self.property(index, via);
        }
        for constraint in &shape.constraints {
            (self.check)()?;
            match constraint {
                Constraint::Property(property) => {
                    if self.walk.visited.insert(*property)
                        && !shapes.shapes()[*property].deactivated
                    {
                        self.property(*property, via)?;
                    }
                }
                Constraint::Node(node) => {
                    self.shape(
                        *node,
                        Some(via.unwrap_or(&shapes.shapes()[*node].node)),
                        depth + 1,
                    )?;
                }
                Constraint::And(nodes) => {
                    for &node in nodes {
                        self.shape(node, via, depth + 1)?;
                    }
                }
                Constraint::Closed { ignored, .. } => {
                    self.walk.out.closed = true;
                    for value in ignored {
                        term(value, &mut self.check, 0)?;
                        for old in &self.walk.out.ignored {
                            term(value, &mut self.check, 0)?;
                            term(old, &mut self.check, 0)?;
                        }
                        if !self.walk.out.ignored.contains(value) {
                            self.walk.out.ignored.push(value.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn property(&mut self, index: ShapeId, via: Option<&Term>) -> Result<()> {
        let shapes = self.walk.shapes;
        let shape = &shapes.shapes()[index];
        let Some(source_path) = &shape.path else {
            return Ok(());
        };
        path(source_path, &mut self.check, 0)?;
        let mut previous = None;
        for field in &self.walk.out.fields {
            // Admit the comparison work as well as the new owned path.
            field_path(&field.path, &mut self.check)?;
            path(source_path, &mut self.check, 0)?;
            if same_path(&field.path, source_path) {
                previous = Some(field);
                break;
            }
        }
        admit_property(shapes, index, via, previous, &mut self.check)?;
        self.walk.property(index, via);
        Ok(())
    }
}

fn admit_property(
    shapes: &Shapes,
    index: ShapeId,
    via: Option<&Term>,
    previous: Option<&FormField>,
    check: &mut Control<'_>,
) -> Result<()> {
    let shape = &shapes.shapes()[index];
    let info = shape.info();
    check()?;
    for literal in info.names.iter().chain(&info.descriptions) {
        check()?;
        lexical(literal.value(), check)?;
        lexical(literal.datatype().as_str(), check)?;
        if let Some(language) = literal.language() {
            lexical(language, check)?;
        }
    }
    if let Some(group) = info.group.as_ref().or(via) {
        term(group, check, 0)?;
    }
    for node in info.editor.iter().chain(&info.viewer) {
        lexical(node.as_str(), check)?;
    }
    if let Some(value) = &info.default_value {
        term(value, check, 0)?;
    }
    enum Allowed<'a> {
        Terms(&'a [Term]),
        Indices(&'a [u32]),
    }
    let mut allowed = previous
        .and_then(|f| f.constraints.allowed.as_deref())
        .map(Allowed::Terms);
    let mut languages = previous.and_then(|f| f.constraints.language_in.as_deref());
    for constraint in &shape.constraints {
        check()?;
        match constraint {
            Constraint::Datatype(n) => lexical(n.as_str(), check)?,
            Constraint::Class(t) | Constraint::HasValue(t) => {
                let value = shapes.term(*t);
                term(value, check, 0)?;
                if let Some(previous) = previous {
                    for old in &previous.constraints.has_value {
                        term(value, check, 0)?;
                        term(old, check, 0)?;
                    }
                    for old in &previous.constraints.classes {
                        term(value, check, 0)?;
                        lexical(old.as_str(), check)?;
                    }
                }
                // The actual output only contains earlier constraints. Including
                // later source entries conservatively admits every comparison.
                for other in &shape.constraints {
                    check()?;
                    if let Constraint::Class(t) | Constraint::HasValue(t) = other {
                        term(value, check, 0)?;
                        term(shapes.term(*t), check, 0)?;
                    }
                }
            }
            Constraint::In(values) => {
                for &value in values {
                    term(shapes.term(value), check, 0)?;
                }
                if let Some(old) = &allowed {
                    match old {
                        Allowed::Terms(old) => {
                            for value in *old {
                                for &candidate in values {
                                    term(value, check, 0)?;
                                    term(shapes.term(candidate), check, 0)?;
                                }
                            }
                        }
                        Allowed::Indices(old) => {
                            for &value in *old {
                                for &candidate in values {
                                    term(shapes.term(value), check, 0)?;
                                    term(shapes.term(candidate), check, 0)?;
                                }
                            }
                        }
                    }
                }
                // The intersection is a subset of this list, so its borrowed
                // source is a conservative bound for the next intersection.
                allowed = Some(Allowed::Indices(values));
            }
            Constraint::MinInclusive(t, _)
            | Constraint::MinExclusive(t, _)
            | Constraint::MaxInclusive(t, _)
            | Constraint::MaxExclusive(t, _) => {
                // Bound stores both the source term and its parsed value.
                term(t, check, 0)?;
                term(t, check, 0)?;
            }
            Constraint::Pattern(p) => {
                lexical(&p.pattern, check)?;
                if let Some(flags) = &p.flags {
                    lexical(flags, check)?;
                }
                if let Some(previous) = previous {
                    for (pattern, flags) in &previous.constraints.patterns {
                        check()?;
                        lexical(&p.pattern, check)?;
                        if let Some(flags) = &p.flags {
                            lexical(flags, check)?;
                        }
                        lexical(pattern, check)?;
                        if let Some(flags) = flags {
                            lexical(flags, check)?;
                        }
                    }
                }
                for other in &shape.constraints {
                    check()?;
                    if let Constraint::Pattern(other) = other {
                        lexical(&p.pattern, check)?;
                        if let Some(flags) = &p.flags {
                            lexical(flags, check)?;
                        }
                        lexical(&other.pattern, check)?;
                        if let Some(flags) = &other.flags {
                            lexical(flags, check)?;
                        }
                    }
                }
            }
            Constraint::LanguageIn(tags) => {
                for tag in tags {
                    check()?;
                    lexical(tag, check)?;
                }
                if let Some(old) = languages {
                    for old in old {
                        for tag in tags {
                            check()?;
                            lexical(old, check)?;
                            lexical(tag, check)?;
                        }
                    }
                }
                languages = Some(tags);
            }
            Constraint::Node(node) => term(&shapes.shapes()[*node].node, check, 0)?,
            Constraint::Component(component) => {
                let iri = component.component.iri.as_str();
                lexical(iri, check)?;
                if let Some(previous) = previous {
                    for old in &previous.constraints.server {
                        check()?;
                        lexical(iri, check)?;
                        lexical(old, check)?;
                    }
                }
                for other in &shape.constraints {
                    check()?;
                    if let Constraint::Component(other) = other {
                        lexical(iri, check)?;
                        lexical(other.component.iri.as_str(), check)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
