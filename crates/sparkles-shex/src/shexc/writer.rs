//! The ShExC pretty printer: prefixed names from the schema's prefixes, annotations and
//! semantic actions kept.
//!
//! What the parser reads back is the same AST, except for what ShExC cannot say:
//! `closed: false` and `inverse: false` are dropped, a lone `min` or `max` comes
//! back with the other made explicit, and a node constraint ShExC has no form for
//! (only ShExJ has those: no part at all, or a node kind with a datatype, say) is
//! written as an equivalent expression of its parts. Operands
//! of AND, OR and NOT that are themselves compound are parenthesized, and a node
//! constraint next to a shape is always joined with an explicit `AND`, so the nesting
//! of the AST survives the round trip.

use crate::PrefixMap;
use crate::ast::*;
use std::fmt::Write as _;

const INDENT: &str = "  ";

/// Write a schema as ShExC (see [`Schema::to_shexc`]).
pub fn write(schema: &Schema) -> String {
    let w = W {
        prefixes: &schema.prefixes,
    };
    let mut out = String::new();
    if let Some(b) = &schema.base {
        writeln!(out, "BASE {}", iriref(b)).unwrap();
    }
    for (p, ns) in &schema.prefixes {
        writeln!(out, "PREFIX {p}: {}", iriref(ns)).unwrap();
    }
    for i in &schema.imports {
        writeln!(out, "IMPORT {}", iriref(i)).unwrap();
    }
    if !out.is_empty() {
        out.push('\n');
    }
    if !schema.start_acts.is_empty() {
        out.push_str(&w.sem_acts(&schema.start_acts));
        out.push('\n');
    }
    if let Some(s) = &schema.start {
        writeln!(out, "start = {}", w.inline(s, 0)).unwrap();
    }
    for (i, d) in schema.shapes.iter().enumerate() {
        if i > 0 || schema.start.is_some() || !schema.start_acts.is_empty() {
            out.push('\n');
        }
        let body = match &d.expr {
            ShapeExpr::External => "EXTERNAL".to_string(),
            e => w.expr(e, 0),
        };
        writeln!(out, "{} {body}", w.label(&d.label)).unwrap();
    }
    out
}

/// `<iri>`, with the characters an IRIREF cannot hold escaped.
fn iriref(iri: &str) -> String {
    let mut s = String::with_capacity(iri.len() + 2);
    s.push('<');
    for c in iri.chars() {
        if c <= ' ' || matches!(c, '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\\') {
            write!(s, "\\u{:04X}", c as u32).unwrap();
        } else {
            s.push(c);
        }
    }
    s.push('>');
    s
}

/// A string literal, quoted and escaped.
fn quoted(v: &str) -> String {
    let mut s = String::with_capacity(v.len() + 2);
    s.push('"');
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if c < ' ' => write!(s, "\\u{:04X}", c as u32).unwrap(),
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

/// A regular expression between slashes: `/` escaped, a backslash before a slash
/// written as `\` (the parser undoes `\/` and `\u` only), line breaks as the
/// regular expression's own escapes.
fn regexp(pattern: &str, flags: Option<&str>) -> String {
    let mut s = String::from("/");
    let mut it = pattern.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '/' => s.push_str("\\/"),
            '\\' => match it.peek() {
                Some('/') => s.push_str("\\u005C"),
                Some(_) => {
                    s.push('\\');
                    s.push(it.next().unwrap());
                }
                None => s.push('\\'),
            },
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            c => s.push(c),
        }
    }
    s.push('/');
    s.push_str(flags.unwrap_or(""));
    s
}

/// The code of a semantic action, with `%` and `\` escaped.
fn code(c: &str) -> String {
    c.replace('\\', "\\\\").replace('%', "\\%")
}

/// A local name a prefixed name can carry as is.
fn plain_local(l: &str) -> bool {
    let mut chars = l.chars();
    match chars.next() {
        None => true,
        Some(c) if !(c.is_alphanumeric() || c == '_') => false,
        _ => {
            !l.ends_with('.')
                && l.chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
        }
    }
}

/// A cardinality, or nothing for the default.
fn card(min: Option<u32>, max: Option<i64>) -> String {
    if min.is_none() && max.is_none() {
        return String::new();
    }
    match (min.unwrap_or(1), max.unwrap_or(1)) {
        (0, 1) => "?".into(),
        (0, -1) => "*".into(),
        (1, -1) => "+".into(),
        (m, -1) => format!("{{{m},}}"),
        (m, n) if i64::from(m) == n => format!("{{{m}}}"),
        (m, n) => format!("{{{m},{n}}}"),
    }
}

struct W<'a> {
    prefixes: &'a PrefixMap,
}

impl W<'_> {
    fn iri(&self, iri: &str) -> String {
        self.prefixes
            .iter()
            .filter_map(|(p, ns)| Some((p, ns, iri.strip_prefix(ns.as_str())?)))
            .filter(|(_, ns, l)| !ns.is_empty() && plain_local(l))
            .max_by_key(|(_, ns, _)| ns.len())
            .map_or_else(|| iriref(iri), |(p, _, l)| format!("{p}:{l}"))
    }

    fn label(&self, l: &Label) -> String {
        match l {
            Label::Iri(i) => self.iri(i),
            Label::BNode(b) => format!("_:{b}"),
        }
    }

    fn literal(&self, l: &ObjectLiteral) -> String {
        let mut s = quoted(&l.value);
        if let Some(lang) = &l.language {
            write!(s, "@{lang}").unwrap();
        } else if let Some(dt) = &l.datatype {
            write!(s, "^^{}", self.iri(dt)).unwrap();
        }
        s
    }

    fn object(&self, o: &ObjectValue) -> String {
        match o {
            ObjectValue::Iri(i) => self.iri(i),
            ObjectValue::Literal(l) => self.literal(l),
        }
    }

    fn sem_acts(&self, acts: &[SemAct]) -> String {
        acts.iter()
            .map(|a| match &a.code {
                None => format!("%{}%", self.iri(&a.name)),
                Some(c) => format!("%{}{{{}%}}", self.iri(&a.name), code(c)),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn annotations(&self, anns: &[Annotation]) -> String {
        anns.iter()
            .map(|a| format!("// {} {}", self.iri(&a.predicate), self.object(&a.object)))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Annotations then semantic actions, each group preceded by a space.
    fn tail(&self, anns: &[Annotation], acts: &[SemAct]) -> String {
        let mut s = String::new();
        if !anns.is_empty() {
            s.push(' ');
            s.push_str(&self.annotations(anns));
        }
        if !acts.is_empty() {
            s.push(' ');
            s.push_str(&self.sem_acts(acts));
        }
        s
    }

    /// A shape expression where a full `shapeExpression` may stand (a declaration, or
    /// inside parentheses).
    fn expr(&self, e: &ShapeExpr, depth: usize) -> String {
        match e {
            ShapeExpr::Or(v) => v
                .iter()
                .map(|x| self.operand(x, depth, matches!(x, ShapeExpr::Or(_))))
                .collect::<Vec<_>>()
                .join(" OR "),
            ShapeExpr::And(v) => v
                .iter()
                .map(|x| self.operand(x, depth, matches!(x, ShapeExpr::Or(_) | ShapeExpr::And(_))))
                .collect::<Vec<_>>()
                .join(" AND "),
            ShapeExpr::Not(x) => format!(
                "NOT {}",
                self.operand(
                    x,
                    depth,
                    matches!(
                        &**x,
                        ShapeExpr::Or(_) | ShapeExpr::And(_) | ShapeExpr::Not(_)
                    )
                )
            ),
            ShapeExpr::Ref(l) => format!("@{}", self.label(l)),
            ShapeExpr::Nc(nc) => self.node_constraint(nc),
            ShapeExpr::Shape(s) => self.shape(s, depth, true),
            ShapeExpr::External => "EXTERNAL".into(),
        }
    }

    /// An operand of AND, OR or NOT: parenthesized when compound, or when it is a
    /// shape with annotations or semantic actions (the inline form has none).
    fn operand(&self, e: &ShapeExpr, depth: usize, compound: bool) -> String {
        if compound {
            format!("({})", self.expr(e, depth))
        } else {
            self.expr(e, depth)
        }
    }

    /// A shape expression where only an `inlineShapeExpression` may stand (a triple
    /// constraint's value, `start`): shapes with annotations or semantic actions, and
    /// compound expressions, are parenthesized.
    fn inline(&self, e: &ShapeExpr, depth: usize) -> String {
        let wrap = match e {
            ShapeExpr::Shape(s) => !s.annotations.is_empty() || !s.sem_acts.is_empty(),
            ShapeExpr::Or(_) | ShapeExpr::And(_) | ShapeExpr::Not(_) => true,
            _ => false,
        };
        if wrap {
            format!("({})", self.expr(e, depth))
        } else {
            self.expr(e, depth)
        }
    }

    /// A node constraint. ShExC writes one only as a node kind, a datatype or a value
    /// set followed by facets (string facets only after IRI, BNODE or NONLITERAL), or as
    /// facets alone of one sort; any other combination (only ShExJ has those) is written
    /// as the conjunction of its parts, in parentheses.
    fn node_constraint(&self, nc: &NodeConstraint) -> String {
        let mut heads: Vec<String> = Vec::new();
        if let Some(k) = nc.node_kind {
            heads.push(k.as_str().to_uppercase());
        }
        if let Some(dt) = &nc.datatype {
            heads.push(self.iri(dt));
        }
        if let Some(vs) = &nc.values {
            let vs: Vec<String> = vs.iter().map(|v| self.value(v)).collect();
            heads.push(format!("[{}]", vs.join(" ")));
        }
        let mut strings: Vec<String> = Vec::new();
        for (kw, v) in [
            ("LENGTH", nc.length),
            ("MINLENGTH", nc.min_length),
            ("MAXLENGTH", nc.max_length),
        ] {
            if let Some(v) = v {
                strings.push(format!("{kw} {v}"));
            }
        }
        if let Some(p) = &nc.pattern {
            strings.push(regexp(p, nc.flags.as_deref()));
        }
        let mut numbers: Vec<String> = Vec::new();
        for (kw, v) in [
            ("MININCLUSIVE", &nc.min_inclusive),
            ("MINEXCLUSIVE", &nc.min_exclusive),
            ("MAXINCLUSIVE", &nc.max_inclusive),
            ("MAXEXCLUSIVE", &nc.max_exclusive),
        ] {
            if let Some(
                NumericLiteral::Integer(s) | NumericLiteral::Decimal(s) | NumericLiteral::Double(s),
            ) = v
            {
                numbers.push(format!("{kw} {s}"));
            }
        }
        for (kw, v) in [
            ("TOTALDIGITS", nc.total_digits),
            ("FRACTIONDIGITS", nc.fraction_digits),
        ] {
            if let Some(v) = v {
                numbers.push(format!("{kw} {v}"));
            }
        }
        let non_literal = matches!(
            nc.node_kind,
            Some(NodeKind::Iri | NodeKind::BNode | NodeKind::NonLiteral)
        );
        let one = match heads.len() {
            0 => strings.is_empty() || numbers.is_empty(),
            1 => !(non_literal && !numbers.is_empty()),
            _ => false,
        };
        if one {
            let all: Vec<String> = heads.into_iter().chain(strings).chain(numbers).collect();
            if all.is_empty() {
                // no part at all: anything matches
                return "(NONLITERAL OR LITERAL)".into();
            }
            return all.join(" ");
        }
        let mut parts = heads;
        if !strings.is_empty() {
            parts.push(strings.join(" "));
        }
        if !numbers.is_empty() {
            parts.push(numbers.join(" "));
        }
        format!("({})", parts.join(" AND "))
    }

    fn exclusion(&self, x: &Exclusion, kind: char) -> String {
        let (v, stem) = match x {
            Exclusion::Value(v) => (v, ""),
            Exclusion::Stem(v) => (v, "~"),
        };
        let v = match kind {
            'i' => self.iri(v),
            'l' => quoted(v),
            _ => format!("@{v}"),
        };
        format!(" - {v}{stem}")
    }

    fn range(&self, stem: &Stem, exclusions: &[Exclusion], kind: char) -> String {
        let mut s = match stem {
            Stem::Wildcard => ".".to_string(),
            Stem::Value(v) => match kind {
                'i' => format!("{}~", self.iri(v)),
                'l' => format!("{}~", quoted(v)),
                _ => format!("@{v}~"),
            },
        };
        for x in exclusions {
            s.push_str(&self.exclusion(x, kind));
        }
        s
    }

    fn value(&self, v: &ValueSetValue) -> String {
        match v {
            ValueSetValue::Object(o) => self.object(o),
            ValueSetValue::IriStem(s) => format!("{}~", self.iri(s)),
            ValueSetValue::LiteralStem(s) => format!("{}~", quoted(s)),
            ValueSetValue::Language(l) => format!("@{l}"),
            ValueSetValue::LanguageStem(l) => format!("@{l}~"),
            ValueSetValue::IriStemRange { stem, exclusions } => self.range(stem, exclusions, 'i'),
            ValueSetValue::LiteralStemRange { stem, exclusions } => {
                self.range(stem, exclusions, 'l')
            }
            ValueSetValue::LanguageStemRange { stem, exclusions } => {
                self.range(stem, exclusions, 'g')
            }
        }
    }

    /// A shape; `tail` writes its annotations and semantic actions (a full shape
    /// expression), otherwise the caller made sure it has none.
    fn shape(&self, s: &Shape, depth: usize, tail: bool) -> String {
        let mut out = String::new();
        if s.is_closed() {
            out.push_str("CLOSED ");
        }
        if !s.extra.is_empty() {
            out.push_str("EXTRA ");
            for p in &s.extra {
                out.push_str(&self.iri(p));
                out.push(' ');
            }
        }
        match &s.expression {
            None => out.push_str("{ }"),
            Some(t) => {
                let pad = INDENT.repeat(depth + 1);
                out.push_str("{\n");
                out.push_str(&pad);
                out.push_str(&self.triple_expr(t, depth + 1, Parent::Top));
                out.push('\n');
                out.push_str(&INDENT.repeat(depth));
                out.push('}');
            }
        }
        if tail {
            out.push_str(&self.tail(&s.annotations, &s.sem_acts));
        }
        out
    }

    fn triple_expr(&self, t: &TripleExpr, depth: usize, parent: Parent) -> String {
        let pad = INDENT.repeat(depth);
        match t {
            TripleExpr::Include(l) => format!("&{}", self.label(l)),
            TripleExpr::Tc(tc) => {
                let mut s = String::new();
                if let Some(id) = &tc.id {
                    write!(s, "${} ", self.label(id)).unwrap();
                }
                if tc.is_inverse() {
                    s.push('^');
                }
                if tc.predicate == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
                    && !self.prefixed(&tc.predicate)
                {
                    s.push('a');
                } else {
                    s.push_str(&self.iri(&tc.predicate));
                }
                s.push(' ');
                match &tc.value_expr {
                    None => s.push('.'),
                    Some(v) => s.push_str(&self.inline(v, depth)),
                }
                s.push_str(&card(tc.min, tc.max));
                s.push_str(&self.tail(&tc.annotations, &tc.sem_acts));
                s
            }
            TripleExpr::EachOf(g) | TripleExpr::OneOf(g) => {
                let each = matches!(t, TripleExpr::EachOf(_));
                let plain = g.id.is_none()
                    && g.min.is_none()
                    && g.max.is_none()
                    && g.sem_acts.is_empty()
                    && g.annotations.is_empty();
                // an EachOf may stand bare at the top or inside a OneOf; a OneOf only at
                // the top
                let bare = plain
                    && match parent {
                        Parent::Top => true,
                        Parent::OneOf => each,
                        Parent::EachOf => false,
                    };
                let (sep, kid) = if each {
                    (";", Parent::EachOf)
                } else {
                    ("|", Parent::OneOf)
                };
                let inner_depth = if bare { depth } else { depth + 1 };
                let inner_pad = INDENT.repeat(inner_depth);
                let kids: Vec<String> = g
                    .exprs
                    .iter()
                    .map(|x| self.triple_expr(x, inner_depth, kid))
                    .collect();
                let body = kids.join(&format!(" {sep}\n{inner_pad}"));
                if bare {
                    return body;
                }
                let mut s = String::new();
                if let Some(id) = &g.id {
                    write!(s, "${} ", self.label(id)).unwrap();
                }
                write!(s, "(\n{inner_pad}{body}\n{pad})").unwrap();
                s.push_str(&card(g.min, g.max));
                s.push_str(&self.tail(&g.annotations, &g.sem_acts));
                s
            }
        }
    }

    /// Would `iri` be written as a prefixed name?
    fn prefixed(&self, iri: &str) -> bool {
        !self.iri(iri).starts_with('<')
    }
}

/// Where a triple expression stands.
#[derive(Clone, Copy)]
enum Parent {
    /// directly in a shape's braces, or in parentheses
    Top,
    EachOf,
    OneOf,
}

#[cfg(test)]
mod tests;
