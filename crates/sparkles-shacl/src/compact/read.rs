//! The SHACLC parser: the grammar and production rules of the SHACL Compact Syntax,
//! building the triples directly (see spec G03 §4).

use super::lex::{Spanned, Tok, tokenize};
use super::{Document, SyntaxError, is_datatype_iri, node_param, property_param};
use crate::vocab::{SH_NS, owl, rdf, rdfs, sh};
use oxiri::Iri;
use oxrdf::vocab::xsd;
use oxrdf::{BlankNode, Graph, Literal, NamedNode, NamedOrBlankNode, Term, Triple};

/// The prefixes every document starts with.
pub(crate) const STANDARD_PREFIXES: [(&str, &str); 4] = [
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("sh", "http://www.w3.org/ns/shacl#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
];

const NODE_KINDS: [&str; 6] = [
    "BlankNode",
    "IRI",
    "Literal",
    "BlankNodeOrIRI",
    "BlankNodeOrLiteral",
    "IRIOrLiteral",
];

pub(crate) fn parse(text: &str, base: Option<&str>) -> Result<Document, SyntaxError> {
    let toks = tokenize(text)?;
    let base = match base {
        Some(b) => Some(Iri::parse(b.to_string()).map_err(|e| SyntaxError {
            line: 1,
            column: 1,
            message: format!("invalid base IRI <{b}>: {e}"),
        })?),
        None => None,
    };
    let mut p = Parser {
        toks,
        i: 0,
        base,
        prefixes: STANDARD_PREFIXES
            .iter()
            .map(|(p, n)| (p.to_string(), n.to_string()))
            .collect(),
        declared: Vec::new(),
        imports: Vec::new(),
        out: Vec::new(),
    };
    p.document()?;
    let mut graph = Graph::new();
    for t in &p.out {
        graph.insert(t);
    }
    Ok(Document {
        graph,
        prefixes: p.declared,
        base: p.base.map(Iri::into_inner),
    })
}

/// A node of the graph being built: a subject or object.
type Node = NamedOrBlankNode;

/// A parsed value: a term, or an array (written as an RDF list).
enum Value {
    Term(Term),
    Array(Vec<Term>),
}

/// A parsed path, before it becomes triples.
enum Path {
    Iri(NamedNode),
    Alternative(Vec<Path>),
    Sequence(Vec<Path>),
    Inverse(Box<Path>),
    ZeroOrMore(Box<Path>),
    OneOrMore(Box<Path>),
    ZeroOrOne(Box<Path>),
}

struct Parser {
    toks: Vec<Spanned>,
    i: usize,
    base: Option<Iri<String>>,
    /// the prefix mapping in effect (the standard prefixes, then the declared ones)
    prefixes: Vec<(String, String)>,
    /// the prefixes the document declares, in order
    declared: Vec<(String, String)>,
    imports: Vec<NamedNode>,
    out: Vec<Triple>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i).map(|t| &t.0)
    }

    fn peek2(&self) -> Option<&Tok> {
        self.toks.get(self.i + 1).map(|t| &t.0)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.i).map(|t| t.0.clone());
        if t.is_some() {
            self.i += 1;
        }
        t
    }

    /// An error at the current token (or at the end).
    fn err(&self, msg: impl Into<String>) -> SyntaxError {
        let (line, column) = match self.toks.get(self.i) {
            Some(t) => (t.1, t.2),
            None => self.toks.last().map(|t| (t.1, t.2)).unwrap_or((1, 1)),
        };
        SyntaxError {
            line,
            column,
            message: msg.into(),
        }
    }

    fn expected(&self, what: &str) -> SyntaxError {
        match self.peek() {
            Some(t) => self.err(format!("expected {what}, found {}", t.describe())),
            None => self.err(format!("expected {what}, found the end of the document")),
        }
    }

    fn is_p(&self, p: &str) -> bool {
        matches!(self.peek(), Some(Tok::P(q)) if *q == p)
    }

    fn eat_p(&mut self, p: &str) -> bool {
        if self.is_p(p) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect_p(&mut self, p: &str) -> Result<(), SyntaxError> {
        if self.eat_p(p) {
            Ok(())
        } else {
            Err(self.expected(&format!("'{p}'")))
        }
    }

    /// Whether the current token is the keyword `kw` (keywords ignore case).
    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case(kw))
    }

    fn triple(&mut self, s: &Node, p: impl Into<NamedNode>, o: impl Into<Term>) {
        self.out.push(Triple::new(s.clone(), p.into(), o.into()));
    }

    // ------------------------------------------------------------------- terms ----

    fn resolve(&self, iri: &str) -> Result<NamedNode, SyntaxError> {
        let resolved = match &self.base {
            Some(b) => b
                .resolve(iri)
                .map(Iri::into_inner)
                .map_err(|e| self.err(format!("invalid IRI <{iri}>: {e}")))?,
            None => Iri::parse(iri.to_string())
                .map(Iri::into_inner)
                .map_err(|e| {
                    self.err(format!(
                        "invalid IRI <{iri}> (no base to resolve it against): {e}"
                    ))
                })?,
        };
        Ok(NamedNode::new_unchecked(resolved))
    }

    fn expand(&self, prefix: &str, local: &str) -> Result<NamedNode, SyntaxError> {
        let ns = self
            .prefixes
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .map(|(_, n)| n)
            .ok_or_else(|| self.err(format!("undeclared prefix '{prefix}:'")))?;
        NamedNode::new(format!("{ns}{local}"))
            .map_err(|e| self.err(format!("invalid IRI {prefix}:{local}: {e}")))
    }

    /// `iri`: an IRI reference or a prefixed name, if one is next.
    fn try_iri(&mut self) -> Result<Option<NamedNode>, SyntaxError> {
        let n = match self.peek() {
            Some(Tok::IriRef(i)) => {
                let i = i.clone();
                self.resolve(&i)?
            }
            Some(Tok::PName(p, l)) => {
                let (p, l) = (p.clone(), l.clone());
                self.expand(&p, &l)?
            }
            _ => return Ok(None),
        };
        self.i += 1;
        Ok(Some(n))
    }

    fn iri(&mut self, what: &str) -> Result<NamedNode, SyntaxError> {
        self.try_iri()?.ok_or_else(|| self.expected(what))
    }

    /// `literal` (or `None` if no literal is next).
    fn try_literal(&mut self) -> Result<Option<Literal>, SyntaxError> {
        let lit = match self.peek().cloned() {
            Some(Tok::String(s)) => {
                self.i += 1;
                match self.peek().cloned() {
                    Some(Tok::LangTag(tag)) => {
                        self.i += 1;
                        Literal::new_language_tagged_literal(s, &tag)
                            .map_err(|e| self.err(format!("invalid language tag @{tag}: {e}")))?
                    }
                    Some(Tok::P("^^")) => {
                        self.i += 1;
                        let dt = self.iri("a datatype IRI after '^^'")?;
                        Literal::new_typed_literal(s, dt)
                    }
                    _ => Literal::new_simple_literal(s),
                }
            }
            Some(Tok::Integer(n)) => {
                self.i += 1;
                Literal::new_typed_literal(n, xsd::INTEGER)
            }
            Some(Tok::Decimal(n)) => {
                self.i += 1;
                Literal::new_typed_literal(n, xsd::DECIMAL)
            }
            Some(Tok::Double(n)) => {
                self.i += 1;
                Literal::new_typed_literal(n, xsd::DOUBLE)
            }
            Some(Tok::Word(w))
                if w.eq_ignore_ascii_case("true") || w.eq_ignore_ascii_case("false") =>
            {
                self.i += 1;
                Literal::new_typed_literal(w.to_ascii_lowercase(), xsd::BOOLEAN)
            }
            _ => return Ok(None),
        };
        Ok(Some(lit))
    }

    fn iri_or_literal(&mut self) -> Result<Option<Term>, SyntaxError> {
        if let Some(n) = self.try_iri()? {
            return Ok(Some(n.into()));
        }
        Ok(self.try_literal()?.map(Term::from))
    }

    /// `iriOrLiteralOrArray`
    fn value(&mut self) -> Result<Value, SyntaxError> {
        if self.eat_p("[") {
            let mut items = Vec::new();
            while !self.eat_p("]") {
                let t = self
                    .iri_or_literal()?
                    .ok_or_else(|| self.expected("an IRI, a literal or ']'"))?;
                items.push(t);
            }
            return Ok(Value::Array(items));
        }
        self.iri_or_literal()?
            .map(Value::Term)
            .ok_or_else(|| self.expected("an IRI, a literal or an array"))
    }

    /// An RDF list of the terms; returns its head.
    fn list(&mut self, items: Vec<Term>) -> Term {
        let mut head: Term = rdf::NIL.into_owned().into();
        for item in items.into_iter().rev() {
            let cell: Node = BlankNode::default().into();
            self.triple(&cell, rdf::FIRST.into_owned(), item);
            self.triple(&cell, rdf::REST.into_owned(), head);
            head = cell.into();
        }
        head
    }

    fn value_term(&mut self, v: Value) -> Term {
        match v {
            Value::Term(t) => t,
            Value::Array(items) => self.list(items),
        }
    }

    // --------------------------------------------------------------- document ----

    fn document(&mut self) -> Result<(), SyntaxError> {
        while self.peek().is_some() {
            if self.is_kw("BASE") {
                self.i += 1;
                let Some(Tok::IriRef(i)) = self.next() else {
                    self.i -= 1;
                    return Err(self.expected("an IRI reference after BASE"));
                };
                let iri = self.resolve(&i)?;
                self.base = Some(Iri::parse(iri.into_string()).expect("a valid IRI"));
            } else if self.is_kw("PREFIX") {
                self.i += 1;
                let Some(Tok::PName(prefix, local)) = self.next() else {
                    self.i -= 1;
                    return Err(self.expected("a prefix such as 'ex:' after PREFIX"));
                };
                if !local.is_empty() {
                    self.i -= 1;
                    return Err(self.err(format!("expected a prefix, found {prefix}:{local}")));
                }
                let Some(Tok::IriRef(i)) = self.next() else {
                    self.i -= 1;
                    return Err(self.expected("an IRI reference after the prefix"));
                };
                let ns = self.resolve(&i)?.into_string();
                self.prefixes.push((prefix.clone(), ns.clone()));
                self.declared.retain(|(p, _)| *p != prefix);
                self.declared.push((prefix, ns));
            } else if self.is_kw("IMPORTS") {
                self.i += 1;
                let iri = self.iri("an IRI after IMPORTS")?;
                self.imports.push(iri);
            } else if self.is_kw("shapeClass") {
                self.i += 1;
                let shape: Node = self.iri("a shape IRI after 'shapeClass'")?.into();
                self.triple(&shape, rdf::TYPE.into_owned(), sh::NODE_SHAPE.into_owned());
                self.triple(&shape, rdf::TYPE.into_owned(), rdfs::CLASS.into_owned());
                self.node_body(&shape)?;
            } else if self.is_kw("shape") {
                self.i += 1;
                let shape: Node = self.iri("a shape IRI after 'shape'")?.into();
                self.triple(&shape, rdf::TYPE.into_owned(), sh::NODE_SHAPE.into_owned());
                if self.eat_p("->") {
                    let first = self.iri("a class IRI after '->'")?;
                    self.triple(&shape, sh::TARGET_CLASS.into_owned(), first);
                    while let Some(c) = self.try_iri()? {
                        self.triple(&shape, sh::TARGET_CLASS.into_owned(), c);
                    }
                }
                self.node_body(&shape)?;
            } else {
                return Err(self.expected("BASE, IMPORTS, PREFIX, 'shape' or 'shapeClass'"));
            }
        }
        match (&self.base, self.imports.is_empty()) {
            (Some(b), _) => {
                let b: Node = NamedNode::new_unchecked(b.as_str()).into();
                self.triple(&b, rdf::TYPE.into_owned(), owl::ONTOLOGY.into_owned());
                for i in std::mem::take(&mut self.imports) {
                    self.triple(&b, owl::IMPORTS.into_owned(), i);
                }
            }
            (None, false) => {
                return Err(SyntaxError {
                    line: 1,
                    column: 1,
                    message: "IMPORTS needs a base IRI (BASE, or the document's location)".into(),
                });
            }
            (None, true) => {}
        }
        Ok(())
    }

    /// `nodeShapeBody`: `{ constraint* }` with `shape` as the context shape.
    fn node_body(&mut self, shape: &Node) -> Result<(), SyntaxError> {
        self.expect_p("{")?;
        while !self.eat_p("}") {
            self.constraint(shape)?;
        }
        Ok(())
    }

    /// `constraint`: node parameters, a property shape or a shape reference, then `.`.
    fn constraint(&mut self, shape: &Node) -> Result<(), SyntaxError> {
        match self.peek() {
            Some(Tok::Word(_) | Tok::P("!")) => {
                // one or more nodeOr
                self.node_or(shape)?;
                while !self.is_p(".") {
                    if self.peek().is_none() || self.is_p("}") {
                        return Err(self.expected("'.'"));
                    }
                    self.node_or(shape)?;
                }
            }
            Some(Tok::AtPName(..) | Tok::At) => {
                // a shape reference standing alone (an extension of Jena's)
                let n = self.shape_ref()?.expect("a shape reference is next");
                self.triple(shape, sh::NODE.into_owned(), n);
            }
            Some(Tok::IriRef(_) | Tok::PName(..) | Tok::P("(" | "^")) => {
                self.property_shape(shape)?;
            }
            None => return Err(self.expected("a constraint or '}'")),
            Some(_) => return Err(self.expected("a constraint or '}'")),
        }
        self.expect_p(".")
    }

    /// `nodeOr`: `nodeNot ( | nodeNot )*`.
    fn node_or(&mut self, shape: &Node) -> Result<(), SyntaxError> {
        let first = self.node_not_parsed()?;
        if !self.is_p("|") {
            return self.emit_node_not(shape, first);
        }
        let mut items = vec![first];
        while self.eat_p("|") {
            items.push(self.node_not_parsed()?);
        }
        let mut elts = Vec::new();
        for item in items {
            let b: Node = BlankNode::default().into();
            self.emit_node_not(&b, item)?;
            elts.push(Term::from(b));
        }
        let list = self.list(elts);
        self.triple(shape, sh::OR.into_owned(), list);
        Ok(())
    }

    /// `nodeNot`: an optional `!` and `param = value`.
    fn node_not_parsed(&mut self) -> Result<(bool, String, Value), SyntaxError> {
        let neg = self.eat_p("!");
        let name = match self.peek() {
            Some(Tok::Word(w)) if node_param(w) => w.clone(),
            Some(Tok::Word(w)) => {
                return Err(self.err(format!(
                    "'{w}' is not a node parameter (in a node shape body, use name=value with \
                     one of: {})",
                    super::NODE_PARAMS.join(", ")
                )));
            }
            _ => return Err(self.expected("a node parameter")),
        };
        self.i += 1;
        self.expect_p("=")?;
        let v = self.value()?;
        Ok((neg, name, v))
    }

    fn emit_node_not(
        &mut self,
        ctx: &Node,
        (neg, name, v): (bool, String, Value),
    ) -> Result<(), SyntaxError> {
        let target = if neg {
            let b: Node = BlankNode::default().into();
            self.triple(ctx, sh::NOT.into_owned(), b.clone());
            b
        } else {
            ctx.clone()
        };
        let o = self.value_term(v);
        self.triple(
            &target,
            NamedNode::new_unchecked(format!("{SH_NS}{name}")),
            o,
        );
        Ok(())
    }

    // --------------------------------------------------------- property shapes ----

    fn property_shape(&mut self, shape: &Node) -> Result<(), SyntaxError> {
        let path = self.path()?;
        let ps: Node = BlankNode::default().into();
        self.triple(shape, sh::PROPERTY.into_owned(), ps.clone());
        let p = self.path_term(path);
        self.triple(&ps, sh::PATH.into_owned(), p);
        loop {
            if self.is_p("[") {
                self.count(&ps)?;
            } else if self.is_p(".") || self.peek().is_none() {
                return Ok(());
            } else if self.is_p("}") {
                return Err(self.expected("'.' at the end of the property shape"));
            } else {
                self.property_or(&ps)?;
            }
        }
    }

    /// `propertyCount`: `[ min .. max ]`.
    fn count(&mut self, ps: &Node) -> Result<(), SyntaxError> {
        self.expect_p("[")?;
        let Some(Tok::Integer(min)) = self.next() else {
            self.i -= 1;
            return Err(self.expected("an integer minimum count"));
        };
        self.expect_p("..")?;
        let max = match self.next() {
            Some(Tok::Integer(n)) => Some(n),
            Some(Tok::P("*")) => None,
            _ => {
                self.i -= 1;
                return Err(self.expected("an integer maximum count or '*'"));
            }
        };
        self.expect_p("]")?;
        // `[0..n]` produces no sh:minCount
        if min.parse::<i64>() != Ok(0) {
            self.triple(
                ps,
                sh::MIN_COUNT.into_owned(),
                Literal::new_typed_literal(min, xsd::INTEGER),
            );
        }
        if let Some(max) = max {
            self.triple(
                ps,
                sh::MAX_COUNT.into_owned(),
                Literal::new_typed_literal(max, xsd::INTEGER),
            );
        }
        Ok(())
    }

    /// `propertyOr`: `propertyNot ( | propertyNot )*`.
    fn property_or(&mut self, ps: &Node) -> Result<(), SyntaxError> {
        // parse each alternative into a fresh context, then pull a single one up
        let first: Node = BlankNode::default().into();
        let mark = self.out.len();
        self.property_not(&first)?;
        if !self.is_p("|") {
            // rewrite the subject of the alternative's own triples to the property shape
            for t in &mut self.out[mark..] {
                if t.subject == first {
                    t.subject = ps.clone();
                }
            }
            return Ok(());
        }
        let mut elts = vec![Term::from(first)];
        while self.eat_p("|") {
            let b: Node = BlankNode::default().into();
            self.property_not(&b)?;
            elts.push(b.into());
        }
        let list = self.list(elts);
        self.triple(ps, sh::OR.into_owned(), list);
        Ok(())
    }

    fn property_not(&mut self, ctx: &Node) -> Result<(), SyntaxError> {
        if self.eat_p("!") {
            let b: Node = BlankNode::default().into();
            self.triple(ctx, sh::NOT.into_owned(), b.clone());
            self.property_atom(&b)
        } else {
            self.property_atom(ctx)
        }
    }

    /// A shape reference (`@ex:S`, `@<iri>`), if one is next.
    fn shape_ref(&mut self) -> Result<Option<NamedNode>, SyntaxError> {
        match self.peek().cloned() {
            Some(Tok::AtPName(p, l)) => {
                self.i += 1;
                Ok(Some(self.expand(&p, &l)?))
            }
            Some(Tok::At) => {
                self.i += 1;
                match self.next() {
                    Some(Tok::IriRef(i)) => Ok(Some(self.resolve(&i)?)),
                    _ => {
                        self.i -= 1;
                        Err(self.expected("an IRI reference after '@'"))
                    }
                }
            }
            _ => Ok(None),
        }
    }

    /// `propertyAtom`: a type, a node kind, a shape reference, `param = value` or a
    /// nested body.
    fn property_atom(&mut self, ctx: &Node) -> Result<(), SyntaxError> {
        if let Some(n) = self.shape_ref()? {
            self.triple(ctx, sh::NODE.into_owned(), n);
            return Ok(());
        }
        if let Some(t) = self.try_iri()? {
            let p = if is_datatype_iri(t.as_str()) {
                sh::DATATYPE
            } else {
                sh::CLASS
            };
            self.triple(ctx, p.into_owned(), t);
            return Ok(());
        }
        if self.is_p("{") {
            let b: Node = BlankNode::default().into();
            self.triple(ctx, sh::NODE.into_owned(), b.clone());
            return self.node_body(&b);
        }
        let Some(Tok::Word(w)) = self.peek().cloned() else {
            return Err(self
                .expected("a type, a node kind, a shape reference, name=value or a nested shape"));
        };
        if matches!(self.peek2(), Some(Tok::P("="))) {
            if !property_param(&w) {
                return Err(self.err(format!(
                    "'{w}' is not a property parameter (one of: {})",
                    super::PROPERTY_PARAMS.join(", ")
                )));
            }
            self.i += 2;
            let v = self.value()?;
            let o = self.value_term(v);
            self.triple(ctx, NamedNode::new_unchecked(format!("{SH_NS}{w}")), o);
            return Ok(());
        }
        if NODE_KINDS.contains(&w.as_str()) {
            self.i += 1;
            self.triple(
                ctx,
                sh::NODE_KIND.into_owned(),
                NamedNode::new_unchecked(format!("{SH_NS}{w}")),
            );
            return Ok(());
        }
        Err(self.err(format!(
            "'{w}' is neither a node kind ({}) nor followed by '='",
            NODE_KINDS.join(", ")
        )))
    }

    // ------------------------------------------------------------------- paths ----

    /// `path`: alternatives of sequences of (inverse) elements with modifiers.
    fn path(&mut self) -> Result<Path, SyntaxError> {
        let mut alts = vec![self.path_sequence()?];
        while self.is_p("|") && self.path_starts_after_bar() {
            self.i += 1;
            alts.push(self.path_sequence()?);
        }
        Ok(if alts.len() == 1 {
            alts.pop().expect("one")
        } else {
            Path::Alternative(alts)
        })
    }

    /// Whether the `|` here continues a path. In a property shape, `|` after the path's
    /// first element always does (`ex:a|ex:b`), as the grammar reads it greedily.
    fn path_starts_after_bar(&self) -> bool {
        matches!(
            self.peek2(),
            Some(Tok::IriRef(_) | Tok::PName(..) | Tok::P("(" | "^"))
        )
    }

    fn path_sequence(&mut self) -> Result<Path, SyntaxError> {
        let mut seq = vec![self.path_elt_or_inverse()?];
        while self.eat_p("/") {
            seq.push(self.path_elt_or_inverse()?);
        }
        Ok(if seq.len() == 1 {
            seq.pop().expect("one")
        } else {
            Path::Sequence(seq)
        })
    }

    fn path_elt_or_inverse(&mut self) -> Result<Path, SyntaxError> {
        if self.eat_p("^") {
            return Ok(Path::Inverse(Box::new(self.path_elt()?)));
        }
        self.path_elt()
    }

    fn path_elt(&mut self) -> Result<Path, SyntaxError> {
        let primary = if self.eat_p("(") {
            let p = self.path()?;
            self.expect_p(")")?;
            p
        } else {
            Path::Iri(self.iri("a path")?)
        };
        Ok(if self.eat_p("?") {
            Path::ZeroOrOne(Box::new(primary))
        } else if self.eat_p("*") {
            Path::ZeroOrMore(Box::new(primary))
        } else if self.eat_p("+") {
            Path::OneOrMore(Box::new(primary))
        } else {
            primary
        })
    }

    /// The triples of a path; returns its node.
    fn path_term(&mut self, p: Path) -> Term {
        let unary = |s: &mut Parser, pred: oxrdf::NamedNodeRef<'_>, x: Path| -> Term {
            let inner = s.path_term(x);
            let b: Node = BlankNode::default().into();
            s.triple(&b, pred.into_owned(), inner);
            b.into()
        };
        match p {
            Path::Iri(n) => n.into(),
            Path::Sequence(xs) => {
                let items = xs.into_iter().map(|x| self.path_term(x)).collect();
                self.list(items)
            }
            Path::Alternative(xs) => {
                let items = xs.into_iter().map(|x| self.path_term(x)).collect();
                let list = self.list(items);
                let b: Node = BlankNode::default().into();
                self.triple(&b, sh::ALTERNATIVE_PATH.into_owned(), list);
                b.into()
            }
            Path::Inverse(x) => unary(self, sh::INVERSE_PATH, *x),
            Path::ZeroOrMore(x) => unary(self, sh::ZERO_OR_MORE_PATH, *x),
            Path::OneOrMore(x) => unary(self, sh::ONE_OR_MORE_PATH, *x),
            Path::ZeroOrOne(x) => unary(self, sh::ZERO_OR_ONE_PATH, *x),
        }
    }
}
