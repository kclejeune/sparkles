//! ShExR: schemas as RDF graphs in the ShEx vocabulary (`http://www.w3.org/ns/shex#`,
//! the vocabulary behind ShExJ's JSON-LD context). A schema is the one node of type
//! `sx:Schema`; shape declarations are its `sx:shapes` list, triple expressions and
//! shape expressions are typed nodes (`sx:Shape`, `sx:EachOf`, `sx:TripleConstraint`,
//! …), and labels are the nodes' IRIs (or blank nodes).
//!
//! Reading follows the JSON-LD context: the members ShExJ writes as arrays are RDF lists
//! (`sx:shapes`, `sx:shapeExprs`, `sx:expressions`, `sx:values`, `sx:semActs`,
//! `sx:startActs`, `sx:imports`, and the singular `sx:annotation` and `sx:exclusion`),
//! `sx:extra` has one value per predicate (read sorted), and literals keep their
//! lexical forms; `sx:shapes`, `sx:imports` and `sx:extra` may also be given as several
//! values. A shape expression in a value position is a reference when its node is a
//! declared label, an IRI, or a blank node without a type (a label of an imported
//! schema); otherwise it is read in place. A triple expression is labelled when its node
//! is an IRI or a blank node used more than once: its first use (start, then the
//! declarations in order) defines it and the others include it, so a blank-node label
//! of a triple expression that nothing includes is not kept.
//!
//! Writing gives numeric facet bounds with a whole value as canonical `xsd:integer`s,
//! as ShExJ's JSON numbers become in RDF.

mod turtle;

use crate::ast::*;
use crate::check::check_facets;
use crate::error::ParseError;
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{
    BlankNode, Graph, Literal, NamedNode, NamedNodeRef, NamedOrBlankNode, NamedOrBlankNodeRef,
    Term, TermRef, Triple,
};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles::io::RdfFormat;

/// The ShEx vocabulary namespace.
pub const SX: &str = "http://www.w3.org/ns/shex#";

fn sx(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{SX}{local}"))
}

/// Read a schema from a graph in the ShEx vocabulary. Errors that are not at a place in
/// a text have line and column 0.
pub fn from_graph(graph: &Graph, base: Option<&str>) -> Result<Schema, ParseError> {
    let mut schema = read(graph).map_err(|m| ParseError::new(format!("ShExR: {m}"), 0, 0))?;
    schema.base = base.map(str::to_string);
    Ok(schema)
}

/// Read a schema from RDF text in `format` (any syntax Sparkles reads). Syntax errors of
/// the RDF carry their line and column. The triples of every graph of a dataset syntax
/// are read as one graph. The document's prefixes (but `sx:`) become the schema's.
pub fn from_text(text: &str, format: RdfFormat, base: Option<&str>) -> Result<Schema, ParseError> {
    let mut parser = oxrdfio::RdfParser::from_format(format);
    if let Some(b) = base {
        parser = parser
            .with_base_iri(b)
            .map_err(|e| ParseError::new(format!("invalid base IRI <{b}>: {e}"), 0, 0))?;
    }
    let mut graph = Graph::new();
    let mut quads = parser.for_slice(text.as_bytes());
    for q in quads.by_ref() {
        match q {
            Ok(q) => {
                graph.insert(&Triple::new(q.subject, q.predicate, q.object));
            }
            Err(e) => {
                let (line, column) = e.location().map_or((0, 0), |l| {
                    (l.start.line as usize + 1, l.start.column as usize + 1)
                });
                return Err(ParseError::new(e.to_string(), line, column));
            }
        }
    }
    let mut prefixes: Vec<(String, String)> = quads
        .prefixes()
        .filter(|(p, ns)| !(*p == "sx" && *ns == SX))
        .map(|(p, ns)| (p.to_string(), ns.to_string()))
        .collect();
    prefixes.sort();
    let mut schema = from_graph(&graph, base)?;
    schema.prefixes = prefixes;
    Ok(schema)
}

/// The schema as a graph in the ShEx vocabulary (labels are the declarations' IRIs or
/// blank nodes; other nodes are fresh blank nodes).
pub fn to_graph(schema: &Schema) -> Graph {
    let mut g = Graph::new();
    for t in &Writer::write(schema) {
        g.insert(t);
    }
    g
}

/// The schema as RDF text in `format`, with the schema's prefixes and `sx:`. Turtle is
/// written nested (blank nodes used once in place, RDF lists as collections), in the
/// schema's order.
pub fn to_text(schema: &Schema, format: RdfFormat) -> String {
    let triples = Writer::write(schema);
    let mut prefixes = schema.prefixes.clone();
    if !prefixes.iter().any(|(p, _)| p == "sx") {
        prefixes.push(("sx".to_string(), SX.to_string()));
    }
    if format == RdfFormat::Turtle {
        return turtle::write(&triples, &prefixes);
    }
    let ser = sparkles::io::with_prefixes(oxrdfio::RdfSerializer::from_format(format), prefixes);
    let mut w = ser.for_writer(Vec::new());
    for t in &triples {
        w.serialize_triple(t)
            .expect("writing to memory does not fail");
    }
    String::from_utf8(w.finish().expect("writing to memory does not fail"))
        .expect("RDF syntaxes are UTF-8")
}

// ------------------------------------------------------------------- reading ------

type R<T> = Result<T, String>;

/// A node as errors name it.
fn name(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => format!("<{}>", n.as_str()),
        Term::BlankNode(b) => format!("_:{}", b.as_str()),
        t => t.to_string(),
    }
}

fn as_subject(t: &Term) -> Option<NamedOrBlankNodeRef<'_>> {
    match t {
        Term::NamedNode(n) => Some(n.as_ref().into()),
        Term::BlankNode(b) => Some(b.as_ref().into()),
        _ => None,
    }
}

fn read(g: &Graph) -> R<Schema> {
    let schema_type = sx("Schema");
    let mut roots = g.subjects_for_predicate_object(rdf::TYPE, &schema_type);
    let root: Term = match (roots.next(), roots.next()) {
        (Some(r), None) => r.into_owned().into(),
        (None, _) => return Err("no node has type sx:Schema".into()),
        (Some(_), Some(_)) => return Err("more than one node has type sx:Schema".into()),
    };
    let mut r = Reader {
        g,
        declared: FxHashSet::default(),
        te_defined: FxHashSet::default(),
        open: Vec::new(),
    };
    r.schema(&root)
}

struct Reader<'g> {
    g: &'g Graph,
    /// the members of `sx:shapes`
    declared: FxHashSet<Term>,
    /// labelled triple expressions already read (later uses include them)
    te_defined: FxHashSet<Term>,
    /// the blank nodes being read in place (one inside itself is a cycle)
    open: Vec<Term>,
}

impl Reader<'_> {
    fn objects(&self, s: &Term, p: &str) -> Vec<Term> {
        let Some(s) = as_subject(s) else {
            return Vec::new();
        };
        self.g
            .objects_for_subject_predicate(s, &sx(p))
            .map(TermRef::into_owned)
            .collect()
    }

    /// The one value of `sx:p`, if any.
    fn one(&self, s: &Term, p: &str) -> R<Option<Term>> {
        let mut v = self.objects(s, p);
        match v.len() {
            0 => Ok(None),
            1 => Ok(v.pop()),
            _ => Err(format!("{} has more than one sx:{p}", name(s))),
        }
    }

    fn required(&self, s: &Term, p: &str) -> R<Term> {
        self.one(s, p)?
            .ok_or_else(|| format!("{} has no sx:{p}", name(s)))
    }

    /// The local name of the node's type in the ShEx vocabulary.
    fn sx_type(&self, s: &Term) -> R<Option<String>> {
        let Some(sub) = as_subject(s) else {
            return Ok(None);
        };
        let mut found = None;
        for t in self.g.objects_for_subject_predicate(sub, rdf::TYPE) {
            if let TermRef::NamedNode(n) = t
                && let Some(local) = n.as_str().strip_prefix(SX)
            {
                if found.is_some() {
                    return Err(format!("{} has more than one ShEx type", name(s)));
                }
                found = Some(local.to_string());
            }
        }
        Ok(found)
    }

    /// The members of the RDF list at `head`.
    fn list(&self, head: &Term, owner: &Term, p: &str) -> R<Vec<Term>> {
        let bad = || format!("sx:{p} of {} is not an RDF list", name(owner));
        let nil: Term = rdf::NIL.into_owned().into();
        let (first, rest) = (NamedNode::from(rdf::FIRST), NamedNode::from(rdf::REST));
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        let mut cur = head.clone();
        while cur != nil {
            if !seen.insert(cur.clone()) {
                return Err(bad());
            }
            let s = as_subject(&cur).ok_or_else(bad)?;
            let one = |p: &NamedNode| {
                let mut it = self.g.objects_for_subject_predicate(s, p);
                match (it.next(), it.next()) {
                    (Some(x), None) => Ok(x.into_owned()),
                    _ => Err(bad()),
                }
            };
            out.push(one(&first)?);
            cur = one(&rest)?;
        }
        Ok(out)
    }

    /// The members of the list that is the value of `sx:p` (none if absent).
    fn list_of(&self, s: &Term, p: &str) -> R<Vec<Term>> {
        match self.one(s, p)? {
            Some(head) => self.list(&head, s, p),
            None => Ok(Vec::new()),
        }
    }

    /// The members of `sx:p`: the items of a list, or the values themselves (a few
    /// writers give `sx:shapes` or `sx:extra` one value per member).
    fn members(&self, s: &Term, p: &str) -> R<Vec<Term>> {
        let first = NamedNode::from(rdf::FIRST);
        let mut out = Vec::new();
        for v in self.objects(s, p) {
            let is_list = v == Term::from(rdf::NIL.into_owned())
                || as_subject(&v)
                    .is_some_and(|c| self.g.object_for_subject_predicate(c, &first).is_some());
            if is_list {
                out.extend(self.list(&v, s, p)?);
            } else {
                out.push(v);
            }
        }
        Ok(out)
    }

    fn schema(&mut self, root: &Term) -> R<Schema> {
        let imports = self
            .members(root, "imports")?
            .iter()
            .map(|t| iri(t, "an import"))
            .collect::<R<Vec<_>>>()?;
        let start_acts = self
            .list_of(root, "startActs")?
            .iter()
            .map(|t| self.sem_act(t))
            .collect::<R<Vec<_>>>()?;
        let decls = self.members(root, "shapes")?;
        for d in &decls {
            if !self.declared.insert(d.clone()) {
                return Err(format!("{} is declared twice in sx:shapes", name(d)));
            }
        }
        let start = match self.one(root, "start")? {
            Some(s) => Some(self.shape_expr(&s)?),
            None => None,
        };
        let mut shapes = Vec::with_capacity(decls.len());
        for d in &decls {
            shapes.push(ShapeDecl {
                label: label(d)?,
                expr: self.declaration(d)?,
            });
        }
        Ok(Schema {
            base: None,
            prefixes: Vec::new(),
            imports,
            start,
            start_acts,
            shapes,
        })
    }

    /// The shape expression a declared node defines.
    fn declaration(&mut self, d: &Term) -> R<ShapeExpr> {
        match self.sx_type(d)?.as_deref() {
            Some("ShapeDecl") => {
                self.no_2_2(d)?;
                let e = self.required(d, "shapeExpr")?;
                self.shape_expr(&e)
            }
            Some(t) => self.shape_expr_at(d, t),
            None => Err(format!(
                "{} is declared in sx:shapes but has no shape expression type",
                name(d)
            )),
        }
    }

    /// A shape expression where one is used: a reference, or one read in place.
    fn shape_expr(&mut self, t: &Term) -> R<ShapeExpr> {
        if self.declared.contains(t) {
            return Ok(ShapeExpr::Ref(label(t)?));
        }
        match t {
            Term::NamedNode(n) => Ok(ShapeExpr::Ref(Label::Iri(n.as_str().to_string()))),
            Term::BlankNode(b) => match self.sx_type(t)? {
                None => Ok(ShapeExpr::Ref(Label::BNode(b.as_str().to_string()))),
                Some(ty) if ty == "ShapeDecl" => Err(format!(
                    "{}: a sx:ShapeDecl is only a member of sx:shapes",
                    name(t)
                )),
                Some(ty) => {
                    if self.open.contains(t) {
                        return Err(format!(
                            "the shape expression {} contains itself; declare it in sx:shapes",
                            name(t)
                        ));
                    }
                    self.open.push(t.clone());
                    let e = self.shape_expr_at(t, &ty);
                    self.open.pop();
                    e
                }
            },
            t => Err(format!("expected a shape expression, found {}", name(t))),
        }
    }

    /// The shape expression of type `ty` at node `n`.
    fn shape_expr_at(&mut self, n: &Term, ty: &str) -> R<ShapeExpr> {
        Ok(match ty {
            "ShapeOr" | "ShapeAnd" => {
                let members = self.list_of(n, "shapeExprs")?;
                if members.len() < 2 {
                    return Err(format!(
                        "sx:shapeExprs of {} needs at least two shape expressions",
                        name(n)
                    ));
                }
                let v = members
                    .iter()
                    .map(|m| self.shape_expr(m))
                    .collect::<R<Vec<_>>>()?;
                if ty == "ShapeOr" {
                    ShapeExpr::Or(v)
                } else {
                    ShapeExpr::And(v)
                }
            }
            "ShapeNot" => {
                let e = self.required(n, "shapeExpr")?;
                ShapeExpr::Not(Box::new(self.shape_expr(&e)?))
            }
            "NodeConstraint" => {
                let nc = self.node_constraint(n)?;
                check_facets(&nc).map_err(|m| format!("{}: {m}", name(n)))?;
                ShapeExpr::Nc(Box::new(nc))
            }
            "Shape" => ShapeExpr::Shape(Box::new(self.shape(n)?)),
            "ShapeExternal" => ShapeExpr::External,
            t => {
                return Err(format!(
                    "{} has type sx:{t}, which is not a shape expression",
                    name(n)
                ));
            }
        })
    }

    /// ShEx 2.2 members are errors that name the feature.
    fn no_2_2(&self, n: &Term) -> R<()> {
        for p in ["abstract", "extends", "restricts"] {
            for v in self.objects(n, p) {
                if !(p == "abstract" && boolean(&v, p).is_ok_and(|b| !b)) {
                    return Err(format!(
                        "{}: sx:{p} is ShEx 2.2, which is not supported",
                        name(n)
                    ));
                }
            }
        }
        Ok(())
    }

    fn opt_string(&self, n: &Term, p: &str) -> R<Option<String>> {
        self.one(n, p)?.map(|t| string(&t, p)).transpose()
    }

    fn opt_u64(&self, n: &Term, p: &str) -> R<Option<u64>> {
        self.one(n, p)?
            .map(|t| {
                integer(&t)
                    .and_then(|i| u64::try_from(i).ok())
                    .ok_or_else(|| {
                        format!(
                            "sx:{p} of {} is not a non-negative integer: {}",
                            name(n),
                            name(&t)
                        )
                    })
            })
            .transpose()
    }

    fn numeric(&self, n: &Term, p: &str) -> R<Option<NumericLiteral>> {
        let Some(t) = self.one(n, p)? else {
            return Ok(None);
        };
        let bad = || format!("sx:{p} of {} is not a number: {}", name(n), name(&t));
        let Term::Literal(l) = &t else {
            return Err(bad());
        };
        let v = l.value().to_string();
        let dt = l.datatype();
        Ok(Some(if dt == xsd::DECIMAL {
            NumericLiteral::Decimal(v)
        } else if dt == xsd::DOUBLE || dt == xsd::FLOAT {
            NumericLiteral::Double(v)
        } else if integer(&t).is_some() {
            NumericLiteral::Integer(v)
        } else {
            return Err(bad());
        }))
    }

    fn node_constraint(&mut self, n: &Term) -> R<NodeConstraint> {
        let node_kind = match self.one(n, "nodeKind")? {
            None => None,
            Some(k) => Some(
                match k {
                    Term::NamedNode(ref i) => i.as_str().strip_prefix(SX),
                    _ => None,
                }
                .and_then(|k| match k {
                    "iri" => Some(NodeKind::Iri),
                    "bnode" => Some(NodeKind::BNode),
                    "literal" => Some(NodeKind::Literal),
                    "nonliteral" => Some(NodeKind::NonLiteral),
                    _ => None,
                })
                .ok_or_else(|| format!("{}: unknown sx:nodeKind {}", name(n), name(&k)))?,
            ),
        };
        let values = match self.one(n, "values")? {
            None => None,
            Some(head) => Some(
                self.list(&head, n, "values")?
                    .iter()
                    .map(|v| self.value(v))
                    .collect::<R<Vec<_>>>()?,
            ),
        };
        Ok(NodeConstraint {
            node_kind,
            datatype: self
                .one(n, "datatype")?
                .map(|t| iri(&t, "sx:datatype"))
                .transpose()?,
            length: self.opt_u64(n, "length")?,
            min_length: self.opt_u64(n, "minlength")?,
            max_length: self.opt_u64(n, "maxlength")?,
            pattern: self.opt_string(n, "pattern")?,
            flags: self.opt_string(n, "flags")?,
            min_inclusive: self.numeric(n, "mininclusive")?,
            min_exclusive: self.numeric(n, "minexclusive")?,
            max_inclusive: self.numeric(n, "maxinclusive")?,
            max_exclusive: self.numeric(n, "maxexclusive")?,
            total_digits: self.opt_u64(n, "totaldigits")?,
            fraction_digits: self.opt_u64(n, "fractiondigits")?,
            values,
        })
    }

    fn value(&self, v: &Term) -> R<ValueSetValue> {
        let ty = match v {
            Term::NamedNode(i) => {
                return Ok(ValueSetValue::Object(ObjectValue::Iri(
                    i.as_str().to_string(),
                )));
            }
            Term::Literal(l) => {
                return Ok(ValueSetValue::Object(ObjectValue::Literal(object_literal(
                    l,
                ))));
            }
            _ => self
                .sx_type(v)?
                .ok_or_else(|| format!("the value-set value {} has no type", name(v)))?,
        };
        Ok(match ty.as_str() {
            "IriStem" => ValueSetValue::IriStem(self.stem_string(v)?),
            "LiteralStem" => ValueSetValue::LiteralStem(self.stem_string(v)?),
            "LanguageStem" => ValueSetValue::LanguageStem(self.stem_string(v)?),
            "Language" => {
                let t = self.required(v, "languageTag")?;
                ValueSetValue::Language(string(&t, "languageTag")?)
            }
            "IriStemRange" => ValueSetValue::IriStemRange {
                stem: self.stem(v)?,
                exclusions: self.exclusions(v, "IriStem")?,
            },
            "LiteralStemRange" => ValueSetValue::LiteralStemRange {
                stem: self.stem(v)?,
                exclusions: self.exclusions(v, "LiteralStem")?,
            },
            "LanguageStemRange" => ValueSetValue::LanguageStemRange {
                stem: self.stem(v)?,
                exclusions: self.exclusions(v, "LanguageStem")?,
            },
            t => {
                return Err(format!(
                    "{} has type sx:{t}, which is not a value-set value",
                    name(v)
                ));
            }
        })
    }

    /// The stem of a stem: a literal's lexical form, or an IRI.
    fn stem_string(&self, n: &Term) -> R<String> {
        let t = self.required(n, "stem")?;
        match &t {
            Term::NamedNode(i) => Ok(i.as_str().to_string()),
            _ => string(&t, "stem"),
        }
    }

    /// The stem of a stem range: a value, or a node of type `sx:Wildcard`.
    fn stem(&self, n: &Term) -> R<Stem> {
        let t = self.required(n, "stem")?;
        if matches!(t, Term::BlankNode(_)) {
            return match self.sx_type(&t)?.as_deref() {
                Some("Wildcard") => Ok(Stem::Wildcard),
                _ => Err(format!(
                    "sx:stem of {} is neither a value nor a sx:Wildcard",
                    name(n)
                )),
            };
        }
        self.stem_string(n).map(Stem::Value)
    }

    /// The exclusions of a stem range: values, or stems of type `stem_type`.
    fn exclusions(&self, n: &Term, stem_type: &str) -> R<Vec<Exclusion>> {
        let mut items = self.list_of(n, "exclusion")?;
        items.extend(self.list_of(n, "exclusions")?);
        items
            .iter()
            .map(|x| match x {
                Term::NamedNode(i) => Ok(Exclusion::Value(i.as_str().to_string())),
                Term::Literal(l) => Ok(Exclusion::Value(l.value().to_string())),
                _ if self.sx_type(x)?.as_deref() == Some(stem_type) => {
                    self.stem_string(x).map(Exclusion::Stem)
                }
                _ => Err(format!(
                    "an exclusion of {} is neither a value nor a sx:{stem_type}",
                    name(n)
                )),
            })
            .collect()
    }

    fn shape(&mut self, n: &Term) -> R<Shape> {
        self.no_2_2(n)?;
        let mut extra = self
            .members(n, "extra")?
            .iter()
            .map(|e| iri(e, "sx:extra"))
            .collect::<R<Vec<_>>>()?;
        // the values of a predicate have no order
        extra.sort();
        let expression = match self.one(n, "expression")? {
            Some(e) => Some(self.triple_expr(&e)?),
            None => None,
        };
        Ok(Shape {
            closed: self
                .one(n, "closed")?
                .map(|t| boolean(&t, "closed"))
                .transpose()?,
            extra,
            expression,
            sem_acts: self.sem_acts(n)?,
            annotations: self.annotations(n)?,
        })
    }

    fn sem_acts(&self, n: &Term) -> R<Vec<SemAct>> {
        self.list_of(n, "semActs")?
            .iter()
            .map(|a| self.sem_act(a))
            .collect()
    }

    fn annotations(&self, n: &Term) -> R<Vec<Annotation>> {
        let mut items = self.list_of(n, "annotation")?;
        items.extend(self.list_of(n, "annotations")?);
        items.iter().map(|a| self.annotation(a)).collect()
    }

    /// Is the triple-expression node a label (an IRI, or a blank node used twice)?
    fn labelled(&self, t: &Term) -> bool {
        match t {
            Term::NamedNode(_) => true,
            Term::BlankNode(_) => self.g.triples_for_object(t).nth(1).is_some(),
            _ => false,
        }
    }

    fn triple_expr(&mut self, t: &Term) -> R<TripleExpr> {
        let Some(ty) = self.sx_type(t)? else {
            // a label defined elsewhere (an imported schema)
            return Ok(TripleExpr::Include(label(t)?));
        };
        if self.labelled(t) {
            if !self.te_defined.insert(t.clone()) {
                return Ok(TripleExpr::Include(label(t)?));
            }
            return self.triple_expr_at(t, &ty, Some(label(t)?));
        }
        if self.open.contains(t) {
            return Err(format!("the triple expression {} contains itself", name(t)));
        }
        self.open.push(t.clone());
        let e = self.triple_expr_at(t, &ty, None);
        self.open.pop();
        e
    }

    fn triple_expr_at(&mut self, n: &Term, ty: &str, id: Option<Label>) -> R<TripleExpr> {
        let min = self
            .one(n, "min")?
            .map(|t| {
                integer(&t)
                    .and_then(|i| u32::try_from(i).ok())
                    .ok_or_else(|| format!("sx:min of {} is not a non-negative integer", name(n)))
            })
            .transpose()?;
        let max = self
            .one(n, "max")?
            .map(|t| {
                integer(&t)
                    .filter(|&i| i >= -1)
                    .and_then(|i| i64::try_from(i).ok())
                    .ok_or_else(|| {
                        format!("sx:max of {} is not -1 or a non-negative integer", name(n))
                    })
            })
            .transpose()?;
        if let (Some(lo), Some(hi)) = (min, max)
            && hi >= 0
            && i64::from(lo) > hi
        {
            return Err(format!("{}: min {lo} is greater than max {hi}", name(n)));
        }
        Ok(match ty {
            "EachOf" | "OneOf" => {
                let members = self.list_of(n, "expressions")?;
                if members.len() < 2 {
                    return Err(format!(
                        "sx:expressions of {} needs at least two triple expressions",
                        name(n)
                    ));
                }
                let exprs = members
                    .iter()
                    .map(|m| self.triple_expr(m))
                    .collect::<R<Vec<_>>>()?;
                let g = Group {
                    id,
                    exprs,
                    min,
                    max,
                    sem_acts: self.sem_acts(n)?,
                    annotations: self.annotations(n)?,
                };
                if ty == "EachOf" {
                    TripleExpr::EachOf(g)
                } else {
                    TripleExpr::OneOf(g)
                }
            }
            "TripleConstraint" => {
                let value_expr = match self.one(n, "valueExpr")? {
                    Some(v) => Some(Box::new(self.shape_expr(&v)?)),
                    None => None,
                };
                TripleExpr::Tc(TripleConstraint {
                    id,
                    inverse: self
                        .one(n, "inverse")?
                        .map(|t| boolean(&t, "inverse"))
                        .transpose()?,
                    predicate: iri(&self.required(n, "predicate")?, "sx:predicate")?,
                    value_expr,
                    min,
                    max,
                    sem_acts: self.sem_acts(n)?,
                    annotations: self.annotations(n)?,
                })
            }
            t => {
                return Err(format!(
                    "{} has type sx:{t}, which is not a triple expression",
                    name(n)
                ));
            }
        })
    }

    fn sem_act(&self, a: &Term) -> R<SemAct> {
        Ok(SemAct {
            name: iri(&self.required(a, "name")?, "sx:name")?,
            code: self.opt_string(a, "code")?,
        })
    }

    fn annotation(&self, a: &Term) -> R<Annotation> {
        Ok(Annotation {
            predicate: iri(&self.required(a, "predicate")?, "sx:predicate")?,
            object: match self.required(a, "object")? {
                Term::NamedNode(i) => ObjectValue::Iri(i.as_str().to_string()),
                Term::Literal(l) => ObjectValue::Literal(object_literal(&l)),
                o => {
                    return Err(format!(
                        "sx:object of {} is neither an IRI nor a literal: {}",
                        name(a),
                        name(&o)
                    ));
                }
            },
        })
    }
}

fn label(t: &Term) -> R<Label> {
    match t {
        Term::NamedNode(n) => Ok(Label::Iri(n.as_str().to_string())),
        Term::BlankNode(b) => Ok(Label::BNode(b.as_str().to_string())),
        t => Err(format!(
            "a label is an IRI or a blank node, not {}",
            name(t)
        )),
    }
}

fn iri(t: &Term, what: &str) -> R<String> {
    match t {
        Term::NamedNode(n) => Ok(n.as_str().to_string()),
        t => Err(format!("{what} is an IRI, not {}", name(t))),
    }
}

fn string(t: &Term, p: &str) -> R<String> {
    match t {
        Term::Literal(l) => Ok(l.value().to_string()),
        t => Err(format!("sx:{p} is a literal, not {}", name(t))),
    }
}

/// The value of a literal of an integer lexical form.
fn integer(t: &Term) -> Option<i128> {
    let Term::Literal(l) = t else {
        return None;
    };
    let v = l.value().trim();
    v.strip_prefix('+').unwrap_or(v).parse().ok()
}

fn boolean(t: &Term, p: &str) -> R<bool> {
    match t {
        Term::Literal(l) if matches!(l.value(), "true" | "1") => Ok(true),
        Term::Literal(l) if matches!(l.value(), "false" | "0") => Ok(false),
        t => Err(format!("sx:{p} is true or false, not {}", name(t))),
    }
}

fn object_literal(l: &Literal) -> ObjectLiteral {
    let language = l.language().map(str::to_string);
    let datatype = (language.is_none() && l.datatype() != xsd::STRING)
        .then(|| l.datatype().as_str().to_string());
    ObjectLiteral {
        value: l.value().to_string(),
        language,
        datatype,
    }
}

// ------------------------------------------------------------------- writing ------

/// A numeric facet bound as ShExJ's JSON number becomes in RDF: a whole value is a
/// canonical `xsd:integer` (`05.0` is `5`); others keep their lexical form and type.
fn numeric_literal(v: &NumericLiteral) -> Literal {
    let (lex, dt) = match v {
        NumericLiteral::Integer(s) => (s, xsd::INTEGER),
        NumericLiteral::Decimal(s) => (s, xsd::DECIMAL),
        NumericLiteral::Double(s) => (s, xsd::DOUBLE),
    };
    let t = lex.strip_prefix('+').unwrap_or(lex);
    match t.parse::<f64>() {
        // exactly representable whole numbers
        Ok(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => {
            Literal::new_typed_literal((f as i64).to_string(), xsd::INTEGER)
        }
        _ => Literal::new_typed_literal(lex, dt),
    }
}

/// Builds the triples of a schema in the schema's order.
struct Writer {
    triples: Vec<Triple>,
    /// the nodes of blank-node labels
    bnodes: FxHashMap<String, BlankNode>,
}

impl Writer {
    fn write(schema: &Schema) -> Vec<Triple> {
        let mut w = Writer {
            triples: Vec::new(),
            bnodes: FxHashMap::default(),
        };
        w.schema(schema);
        w.triples
    }

    fn add(&mut self, s: &NamedOrBlankNode, p: &str, o: impl Into<Term>) {
        self.triples.push(Triple::new(s.clone(), sx(p), o.into()));
    }

    fn typed(&mut self, s: &NamedOrBlankNode, ty: &str) {
        self.triples
            .push(Triple::new(s.clone(), rdf::TYPE.into_owned(), sx(ty)));
    }

    fn node(&mut self, l: &Label) -> NamedOrBlankNode {
        match l {
            Label::Iri(i) => NamedNode::new_unchecked(i.clone()).into(),
            Label::BNode(b) => self
                .bnodes
                .entry(b.clone())
                .or_insert_with(|| BlankNode::new(b.clone()).unwrap_or_default())
                .clone()
                .into(),
        }
    }

    /// An RDF list of the items (`rdf:nil` when empty).
    fn list(&mut self, items: Vec<Term>) -> Term {
        let cells: Vec<BlankNode> = items.iter().map(|_| BlankNode::default()).collect();
        for (i, item) in items.into_iter().enumerate() {
            let rest: Term = match cells.get(i + 1) {
                Some(next) => next.clone().into(),
                None => rdf::NIL.into_owned().into(),
            };
            let cell = NamedOrBlankNode::from(cells[i].clone());
            self.triples
                .push(Triple::new(cell.clone(), rdf::FIRST.into_owned(), item));
            self.triples
                .push(Triple::new(cell, rdf::REST.into_owned(), rest));
        }
        match cells.first() {
            Some(c) => c.clone().into(),
            None => rdf::NIL.into_owned().into(),
        }
    }

    fn put_list(&mut self, s: &NamedOrBlankNode, p: &str, items: Vec<Term>) {
        if !items.is_empty() {
            let l = self.list(items);
            self.add(s, p, l);
        }
    }

    fn schema(&mut self, schema: &Schema) {
        let root = NamedOrBlankNode::from(BlankNode::default());
        self.typed(&root, "Schema");
        let imports = schema
            .imports
            .iter()
            .map(|i| NamedNode::new_unchecked(i.clone()).into())
            .collect();
        self.put_list(&root, "imports", imports);
        let acts = schema.start_acts.iter().map(|a| self.sem_act(a)).collect();
        self.put_list(&root, "startActs", acts);
        if let Some(s) = &schema.start {
            let s = self.shape_expr(s);
            self.add(&root, "start", s);
        }
        let labels: Vec<NamedOrBlankNode> =
            schema.shapes.iter().map(|d| self.node(&d.label)).collect();
        self.put_list(
            &root,
            "shapes",
            labels.iter().cloned().map(Into::into).collect(),
        );
        for (d, n) in schema.shapes.iter().zip(&labels) {
            match &d.expr {
                ShapeExpr::Ref(l) => {
                    self.typed(n, "ShapeDecl");
                    let r = self.node(l);
                    self.add(n, "shapeExpr", r);
                }
                e => self.shape_expr_at(n, e),
            }
        }
    }

    /// A shape expression where it is used: a label's node, or a new blank node.
    fn shape_expr(&mut self, e: &ShapeExpr) -> Term {
        if let ShapeExpr::Ref(l) = e {
            return self.node(l).into();
        }
        let n = NamedOrBlankNode::from(BlankNode::default());
        self.shape_expr_at(&n, e);
        n.into()
    }

    fn shape_expr_at(&mut self, n: &NamedOrBlankNode, e: &ShapeExpr) {
        match e {
            ShapeExpr::Ref(_) => unreachable!("references are written where they are used"),
            ShapeExpr::Or(v) | ShapeExpr::And(v) => {
                self.typed(
                    n,
                    if matches!(e, ShapeExpr::Or(_)) {
                        "ShapeOr"
                    } else {
                        "ShapeAnd"
                    },
                );
                let items = v.iter().map(|x| self.shape_expr(x)).collect();
                let l = self.list(items);
                self.add(n, "shapeExprs", l);
            }
            ShapeExpr::Not(x) => {
                self.typed(n, "ShapeNot");
                let x = self.shape_expr(x);
                self.add(n, "shapeExpr", x);
            }
            ShapeExpr::External => self.typed(n, "ShapeExternal"),
            ShapeExpr::Nc(nc) => self.node_constraint(n, nc),
            ShapeExpr::Shape(s) => {
                self.typed(n, "Shape");
                if let Some(c) = s.closed {
                    self.add(n, "closed", Literal::from(c));
                }
                for p in &s.extra {
                    self.add(n, "extra", NamedNode::new_unchecked(p.clone()));
                }
                if let Some(t) = &s.expression {
                    let t = self.triple_expr(t);
                    self.add(n, "expression", t);
                }
                self.acts_and_annotations(n, &s.sem_acts, &s.annotations);
            }
        }
    }

    fn node_constraint(&mut self, n: &NamedOrBlankNode, nc: &NodeConstraint) {
        self.typed(n, "NodeConstraint");
        if let Some(k) = nc.node_kind {
            self.add(n, "nodeKind", sx(k.as_str()));
        }
        if let Some(dt) = &nc.datatype {
            self.add(n, "datatype", NamedNode::new_unchecked(dt.clone()));
        }
        let ints = [
            ("length", nc.length),
            ("minlength", nc.min_length),
            ("maxlength", nc.max_length),
            ("totaldigits", nc.total_digits),
            ("fractiondigits", nc.fraction_digits),
        ];
        for (p, v) in ints {
            if let Some(v) = v {
                self.add(n, p, Literal::from(v as i64));
            }
        }
        for (p, v) in [("pattern", &nc.pattern), ("flags", &nc.flags)] {
            if let Some(v) = v {
                self.add(n, p, Literal::new_simple_literal(v));
            }
        }
        let nums = [
            ("mininclusive", &nc.min_inclusive),
            ("minexclusive", &nc.min_exclusive),
            ("maxinclusive", &nc.max_inclusive),
            ("maxexclusive", &nc.max_exclusive),
        ];
        for (p, v) in nums {
            if let Some(v) = v {
                self.add(n, p, numeric_literal(v));
            }
        }
        if let Some(values) = &nc.values {
            let items = values.iter().map(|v| self.value(v)).collect();
            let l = self.list(items);
            self.add(n, "values", l);
        }
    }

    fn object(&mut self, o: &ObjectValue) -> Term {
        match o {
            ObjectValue::Iri(i) => NamedNode::new_unchecked(i.clone()).into(),
            ObjectValue::Literal(l) => match (&l.language, &l.datatype) {
                (Some(lang), _) => Literal::new_language_tagged_literal_unchecked(
                    &l.value,
                    lang.to_ascii_lowercase(),
                )
                .into(),
                (None, Some(dt)) => {
                    Literal::new_typed_literal(&l.value, NamedNodeRef::new_unchecked(dt)).into()
                }
                (None, None) => Literal::new_simple_literal(&l.value).into(),
            },
        }
    }

    /// A blank node of type `ty` with `sx:stem` `stem`.
    fn stem_node(&mut self, ty: &str, stem: Term) -> Term {
        let n = NamedOrBlankNode::from(BlankNode::default());
        self.typed(&n, ty);
        self.add(&n, "stem", stem);
        n.into()
    }

    fn range(&mut self, ty: &str, stem_ty: &str, stem: &Stem, exclusions: &[Exclusion]) -> Term {
        let stem = match stem {
            Stem::Value(s) => Literal::new_simple_literal(s).into(),
            Stem::Wildcard => {
                let w = NamedOrBlankNode::from(BlankNode::default());
                self.typed(&w, "Wildcard");
                w.into()
            }
        };
        let n = self.stem_node(ty, stem);
        let items = exclusions
            .iter()
            .map(|x| match x {
                // IRIs are excluded as IRIs; literals and language tags as strings
                Exclusion::Value(v) if ty == "IriStemRange" => {
                    NamedNode::new_unchecked(v.clone()).into()
                }
                Exclusion::Value(v) => Literal::new_simple_literal(v).into(),
                Exclusion::Stem(s) => {
                    self.stem_node(stem_ty, Literal::new_simple_literal(s).into())
                }
            })
            .collect();
        let l = self.list(items);
        let n = NamedOrBlankNode::try_from(n).expect("a blank node");
        self.add(&n, "exclusion", l);
        n.into()
    }

    fn value(&mut self, v: &ValueSetValue) -> Term {
        match v {
            ValueSetValue::Object(o) => self.object(o),
            ValueSetValue::IriStem(s) => {
                self.stem_node("IriStem", Literal::new_simple_literal(s).into())
            }
            ValueSetValue::LiteralStem(s) => {
                self.stem_node("LiteralStem", Literal::new_simple_literal(s).into())
            }
            ValueSetValue::LanguageStem(s) => {
                self.stem_node("LanguageStem", Literal::new_simple_literal(s).into())
            }
            ValueSetValue::Language(l) => {
                let n = NamedOrBlankNode::from(BlankNode::default());
                self.typed(&n, "Language");
                self.add(&n, "languageTag", Literal::new_simple_literal(l));
                n.into()
            }
            ValueSetValue::IriStemRange { stem, exclusions } => {
                self.range("IriStemRange", "IriStem", stem, exclusions)
            }
            ValueSetValue::LiteralStemRange { stem, exclusions } => {
                self.range("LiteralStemRange", "LiteralStem", stem, exclusions)
            }
            ValueSetValue::LanguageStemRange { stem, exclusions } => {
                self.range("LanguageStemRange", "LanguageStem", stem, exclusions)
            }
        }
    }

    fn triple_expr(&mut self, t: &TripleExpr) -> Term {
        let id = match t {
            TripleExpr::Include(l) => return self.node(l).into(),
            TripleExpr::EachOf(g) | TripleExpr::OneOf(g) => &g.id,
            TripleExpr::Tc(tc) => &tc.id,
        };
        let n = match id {
            Some(l) => self.node(l),
            None => BlankNode::default().into(),
        };
        match t {
            TripleExpr::EachOf(g) | TripleExpr::OneOf(g) => {
                self.typed(
                    &n,
                    if matches!(t, TripleExpr::EachOf(_)) {
                        "EachOf"
                    } else {
                        "OneOf"
                    },
                );
                let items = g.exprs.iter().map(|x| self.triple_expr(x)).collect();
                let l = self.list(items);
                self.add(&n, "expressions", l);
                self.card(&n, g.min, g.max);
                self.acts_and_annotations(&n, &g.sem_acts, &g.annotations);
            }
            TripleExpr::Tc(tc) => {
                self.typed(&n, "TripleConstraint");
                if let Some(i) = tc.inverse {
                    self.add(&n, "inverse", Literal::from(i));
                }
                self.add(
                    &n,
                    "predicate",
                    NamedNode::new_unchecked(tc.predicate.clone()),
                );
                if let Some(v) = &tc.value_expr {
                    let v = self.shape_expr(v);
                    self.add(&n, "valueExpr", v);
                }
                self.card(&n, tc.min, tc.max);
                self.acts_and_annotations(&n, &tc.sem_acts, &tc.annotations);
            }
            TripleExpr::Include(_) => unreachable!(),
        }
        n.into()
    }

    fn card(&mut self, n: &NamedOrBlankNode, min: Option<u32>, max: Option<i64>) {
        if let Some(m) = min {
            self.add(n, "min", Literal::from(i64::from(m)));
        }
        if let Some(m) = max {
            self.add(n, "max", Literal::from(m));
        }
    }

    fn sem_act(&mut self, a: &SemAct) -> Term {
        let n = NamedOrBlankNode::from(BlankNode::default());
        self.typed(&n, "SemAct");
        self.add(&n, "name", NamedNode::new_unchecked(a.name.clone()));
        if let Some(c) = &a.code {
            self.add(&n, "code", Literal::new_simple_literal(c));
        }
        n.into()
    }

    fn acts_and_annotations(
        &mut self,
        n: &NamedOrBlankNode,
        acts: &[SemAct],
        annotations: &[Annotation],
    ) {
        let acts = acts.iter().map(|a| self.sem_act(a)).collect();
        self.put_list(n, "semActs", acts);
        let anns = annotations
            .iter()
            .map(|a| {
                let x = NamedOrBlankNode::from(BlankNode::default());
                self.typed(&x, "Annotation");
                self.add(
                    &x,
                    "predicate",
                    NamedNode::new_unchecked(a.predicate.clone()),
                );
                let o = self.object(&a.object);
                self.add(&x, "object", o);
                x.into()
            })
            .collect();
        self.put_list(n, "annotation", anns);
    }
}

#[cfg(test)]
mod tests;
