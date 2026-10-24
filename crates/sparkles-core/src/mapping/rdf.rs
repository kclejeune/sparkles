//! R2RML and both RML vocabularies, read from a Turtle or N-Triples mapping (spec F19
//! §3.2, §3.3): the W3C R2RML vocabulary, the rml.io vocabulary that adds logical
//! sources to R2RML's term maps, and the RML-Core vocabulary of the W3C Knowledge Graph
//! Construction Community Group, in which every construct lives under
//! `http://w3id.org/rml/`. A mapping may not mix RML-Core with the other two.

use super::CONNECTION_PREFIX;
use super::model::*;
use super::template::Template;
use crate::error::{Error, Result};
use crate::tabular::csvw::DialectDesc;
use oxrdf::vocab::rdf;
use oxrdf::{Literal, NamedNode, NamedOrBlankNode, Term};
use oxttl::TurtleParser;
use std::collections::HashMap;

pub const RR: &str = "http://www.w3.org/ns/r2rml#";
pub const RML_IO: &str = "http://semweb.mmlab.be/ns/rml#";
pub const QL: &str = "http://semweb.mmlab.be/ns/ql#";
pub const RML: &str = "http://w3id.org/rml/";
pub const FNML: &str = "http://semweb.mmlab.be/ns/fnml#";
pub const FNO: &str = "https://w3id.org/function/ontology#";
const FNO_HTTP: &str = "http://w3id.org/function/ontology#";
pub const CSVW: &str = "http://www.w3.org/ns/csvw#";
pub const D2RQ: &str = "http://www.wiwiss.fu-berlin.de/suhl/bizer/D2RQ/0.1#";

type Index = HashMap<NamedOrBlankNode, Vec<(NamedNode, Term)>>;

struct G {
    idx: Index,
    core: bool,
    warnings: Vec<String>,
}

fn subject_of(t: &Term) -> Option<NamedOrBlankNode> {
    match t {
        Term::NamedNode(n) => Some(n.clone().into()),
        Term::BlankNode(b) => Some(b.clone().into()),
        _ => None,
    }
}

fn show(n: &NamedOrBlankNode) -> String {
    match n {
        NamedOrBlankNode::NamedNode(n) => format!("<{}>", n.as_str()),
        NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
    }
}

impl G {
    /// The objects of `s` for property `local` of the vocabulary in use: RML-Core's, or
    /// R2RML's and rml.io's.
    fn all(&self, s: &NamedOrBlankNode, local: &str) -> Vec<&Term> {
        let Some(props) = self.idx.get(s) else {
            return Vec::new();
        };
        props
            .iter()
            .filter(|(p, _)| {
                let p = p.as_str();
                if self.core {
                    p.strip_prefix(RML) == Some(local)
                } else {
                    p.strip_prefix(RR) == Some(local) || p.strip_prefix(RML_IO) == Some(local)
                }
            })
            .map(|(_, o)| o)
            .collect()
    }

    /// The objects of `s` for a full predicate IRI.
    fn by_iri(&self, s: &NamedOrBlankNode, iri: &str) -> Vec<&Term> {
        self.idx
            .get(s)
            .map(|props| {
                props
                    .iter()
                    .filter(|(p, _)| p.as_str() == iri)
                    .map(|(_, o)| o)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn one(&self, s: &NamedOrBlankNode, local: &str) -> Result<Option<&Term>> {
        let v = self.all(s, local);
        if v.len() > 1 {
            return Err(Error::invalid(format!(
                "{} has {} values of {local}; it takes one",
                show(s),
                v.len()
            )));
        }
        Ok(v.into_iter().next())
    }

    fn string(&self, s: &NamedOrBlankNode, local: &str) -> Result<Option<String>> {
        match self.one(s, local)? {
            None => Ok(None),
            Some(Term::Literal(l)) => Ok(Some(l.value().to_string())),
            Some(other) => Err(Error::invalid(format!(
                "{local} of {} must be a string, not {other}",
                show(s)
            ))),
        }
    }

    fn is_a(&self, s: &NamedOrBlankNode, class: &str) -> bool {
        self.by_iri(s, rdf::TYPE.as_str())
            .iter()
            .any(|t| matches!(t, Term::NamedNode(n) if n.as_str() == class))
    }
}

/// Parse an R2RML or RML mapping in Turtle (N-Triples is Turtle). `base` resolves the
/// mapping's relative IRIs.
pub fn parse(text: &str, base: Option<&str>) -> Result<Mapping> {
    let mut parser = TurtleParser::new();
    if let Some(b) = base {
        parser = parser
            .with_base_iri(b)
            .map_err(|e| Error::invalid(format!("the mapping's base {b:?}: {e}")))?;
    }
    let mut reader = parser.for_slice(text);
    let mut idx: Index = HashMap::new();
    let mut order: Vec<NamedOrBlankNode> = Vec::new();
    for t in reader.by_ref() {
        let t = t.map_err(|e| Error::invalid(format!("the mapping is not valid Turtle: {e}")))?;
        let s = t.subject.clone();
        if !idx.contains_key(&s) {
            order.push(s.clone());
        }
        idx.entry(s).or_default().push((t.predicate, t.object));
    }
    let prefixes: Vec<(String, String)> = reader
        .prefixes()
        .map(|(p, ns)| (p.to_string(), ns.to_string()))
        .collect();
    let doc_base = reader.base_iri().map(str::to_string);
    let (mut core, mut legacy, mut io) = (false, false, false);
    for props in idx.values() {
        for (p, _) in props {
            let p = p.as_str();
            if p.starts_with(RML) {
                core = true;
            } else if p.starts_with(RML_IO) {
                io = true;
                legacy = true;
            } else if p.starts_with(RR) {
                legacy = true;
            }
        }
    }
    if core && legacy {
        return Err(Error::invalid(
            "the mapping mixes the RML-Core vocabulary (http://w3id.org/rml/) with R2RML or rml.io terms; use one",
        ));
    }
    if !core && !legacy {
        return Err(Error::invalid(
            "the text uses neither the R2RML nor an RML vocabulary",
        ));
    }
    let vocabulary = if core {
        Vocabulary::RmlCore
    } else if io {
        Vocabulary::RmlIo
    } else {
        Vocabulary::R2rml
    };
    let g = G {
        idx,
        core,
        warnings: Vec::new(),
    };
    // the triples maps: typed so, or with a logical table or source
    let mut tms: Vec<NamedOrBlankNode> = order
        .iter()
        .filter(|s| {
            g.is_a(s, &format!("{RR}TriplesMap"))
                || g.is_a(s, &format!("{RML}TriplesMap"))
                || !g.all(s, "logicalTable").is_empty()
                || !g.all(s, "logicalSource").is_empty()
        })
        .cloned()
        .collect();
    // function values of the rml.io form are triples-map-like resources with a logical
    // source of their parent: they are not triples maps of the mapping
    let fn_values: Vec<NamedOrBlankNode> = g
        .idx
        .values()
        .flat_map(|props| props.iter())
        .filter(|(p, _)| p.as_str() == format!("{FNML}functionValue"))
        .filter_map(|(_, o)| subject_of(o))
        .collect();
    tms.retain(|t| !fn_values.contains(t));
    if tms.is_empty() {
        return Err(Error::invalid("the mapping has no triples map"));
    }
    let mut r = Reader {
        g,
        tms: tms.clone(),
        sources: Vec::new(),
    };
    let mut maps = Vec::new();
    for t in &tms {
        maps.push(r.triples_map(t)?);
    }
    let base = doc_base;
    let mut warnings = std::mem::take(&mut r.g.warnings);
    for t in &tms {
        if !r.g.all(t, "logicalTarget").is_empty() {
            warnings.push(format!(
                "{}: logical targets are ignored; the target is the dataset or the output file",
                show(t)
            ));
        }
    }
    warnings.dedup();
    Ok(Mapping {
        maps,
        sources: r.sources,
        base,
        vocabulary,
        warnings,
        prefixes,
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Subject,
    Predicate,
    Object,
    Graph,
    /// a function's parameter value, a datatype or language map: no default term type
    /// matters, the lexical value is used
    Value,
}

struct Reader {
    g: G,
    tms: Vec<NamedOrBlankNode>,
    sources: Vec<SourceDecl>,
}

impl Reader {
    fn bad<T>(&self, s: &NamedOrBlankNode, m: impl std::fmt::Display) -> Result<T> {
        Err(Error::invalid(format!("{}: {m}", show(s))))
    }

    fn triples_map(&mut self, t: &NamedOrBlankNode) -> Result<TriplesMap> {
        let id = match t {
            NamedOrBlankNode::NamedNode(n) => format!("<{}>", n.as_str()),
            NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
        };
        let decl = self.logical_source(t)?;
        let source = Mapping::source_index(&mut self.sources, decl);
        // the subject map, or the constant shortcut
        let sm = self.g.all(t, "subjectMap");
        let sc = self.g.all(t, "subject");
        let (subject, sm_node) = match (sm.as_slice(), sc.as_slice()) {
            ([one], []) => {
                let n = subject_of(one)
                    .ok_or_else(|| Error::invalid(format!("{id}: the subject map is a literal")))?;
                (self.term_map(&n, Role::Subject)?, Some(n))
            }
            ([], [c]) => (TermMap::constant((*c).clone()), None),
            ([], []) => return self.bad(t, "the triples map has no subject map"),
            _ => return self.bad(t, "the triples map has more than one subject map"),
        };
        let mut classes = Vec::new();
        let mut graphs = Vec::new();
        if let Some(n) = &sm_node {
            for c in self.g.all(n, "class") {
                match c {
                    Term::NamedNode(c) => classes.push(c.clone()),
                    other => return self.bad(n, format!("a class must be an IRI, not {other}")),
                }
            }
            graphs = self.graph_maps(n)?;
        }
        let mut pos = Vec::new();
        for po in self.g.all(t, "predicateObjectMap") {
            let Some(po) = subject_of(po) else {
                return self.bad(t, "a predicate-object map is a literal");
            };
            pos.push(self.predicate_object(&po)?);
        }
        let base = match self.g.one(t, "baseIRI")? {
            None => None,
            Some(Term::NamedNode(n)) => Some(n.as_str().to_string()),
            Some(Term::Literal(l)) => Some(l.value().to_string()),
            Some(other) => return self.bad(t, format!("a base IRI must be an IRI, not {other}")),
        };
        Ok(TriplesMap {
            id,
            source,
            subject,
            classes,
            graphs,
            predicate_objects: pos,
            base,
        })
    }

    fn graph_maps(&self, n: &NamedOrBlankNode) -> Result<Vec<TermMap>> {
        let mut out = Vec::new();
        for gm in self.g.all(n, "graphMap") {
            let Some(gm) = subject_of(gm) else {
                return self.bad(n, "a graph map is a literal");
            };
            out.push(self.term_map(&gm, Role::Graph)?);
        }
        for c in self.g.all(n, "graph") {
            match c {
                Term::NamedNode(_) => out.push(TermMap::constant(c.clone())),
                other => return self.bad(n, format!("a graph must be an IRI, not {other}")),
            }
        }
        Ok(out)
    }

    fn predicate_object(&self, po: &NamedOrBlankNode) -> Result<PredicateObject> {
        let mut predicates = Vec::new();
        for pm in self.g.all(po, "predicateMap") {
            let Some(pm) = subject_of(pm) else {
                return self.bad(po, "a predicate map is a literal");
            };
            predicates.push(self.term_map(&pm, Role::Predicate)?);
        }
        for c in self.g.all(po, "predicate") {
            match c {
                Term::NamedNode(_) => predicates.push(TermMap::constant(c.clone())),
                other => return self.bad(po, format!("a predicate must be an IRI, not {other}")),
            }
        }
        let mut objects = Vec::new();
        for om in self.g.all(po, "objectMap") {
            let Some(om) = subject_of(om) else {
                return self.bad(po, "an object map is a literal");
            };
            objects.push(self.object_map(&om)?);
        }
        for c in self.g.all(po, "object") {
            objects.push(ObjectMap::Term(TermMap::constant(c.clone())));
        }
        Ok(PredicateObject {
            predicates,
            objects,
            graphs: self.graph_maps(po)?,
        })
    }

    fn object_map(&self, om: &NamedOrBlankNode) -> Result<ObjectMap> {
        let Some(parent) = self.g.one(om, "parentTriplesMap")? else {
            return Ok(ObjectMap::Term(self.term_map(om, Role::Object)?));
        };
        let Some(pn) = subject_of(parent) else {
            return self.bad(om, "the parent triples map is a literal");
        };
        let Some(pi) = self.tms.iter().position(|t| *t == pn) else {
            return self.bad(om, format!("the parent {} is not a triples map", show(&pn)));
        };
        let mut conditions = Vec::new();
        for jc in self.g.all(om, "joinCondition") {
            let Some(jc) = subject_of(jc) else {
                return self.bad(om, "a join condition is a literal");
            };
            let side = |name: &str, map: &str| -> Result<String> {
                if let Some(s) = self.g.string(&jc, name)? {
                    return Ok(s);
                }
                if let Some(m) = self.g.one(&jc, map)?.and_then(subject_of)
                    && let Some(r) = self.g.string(&m, "reference")?
                {
                    return Ok(r);
                }
                self.bad(&jc, format!("a join condition needs a {name}"))
            };
            conditions.push((side("child", "childMap")?, side("parent", "parentMap")?));
        }
        Ok(ObjectMap::Join {
            parent: pi,
            conditions,
        })
    }

    fn term_map(&self, n: &NamedOrBlankNode, role: Role) -> Result<TermMap> {
        let constant = self.g.one(n, "constant")?;
        let reference = if self.g.core {
            self.g.string(n, "reference")?
        } else {
            match (self.g.string(n, "column")?, self.g.string(n, "reference")?) {
                (Some(_), Some(_)) => {
                    return self.bad(n, "a term map has both a column and a reference");
                }
                (a, b) => a.or(b),
            }
        };
        let template = self.g.string(n, "template")?;
        let function = self.function(n)?;
        let count = [
            constant.is_some(),
            reference.is_some(),
            template.is_some(),
            function.is_some(),
        ]
        .iter()
        .filter(|b| **b)
        .count();
        if count > 1 {
            return self.bad(n, "a term map has more than one of constant, column or reference, template and function");
        }
        let mut uri = false;
        let term_type = match self.g.one(n, "termType")? {
            None => None,
            Some(Term::NamedNode(t)) => {
                let local = t
                    .as_str()
                    .strip_prefix(RR)
                    .or_else(|| t.as_str().strip_prefix(RML));
                Some(match local {
                    Some("IRI") | Some("UnsafeIRI") => TermType::Iri,
                    Some("URI") => {
                        uri = true;
                        TermType::Iri
                    }
                    Some("BlankNode") => TermType::BlankNode,
                    Some("Literal") => TermType::Literal,
                    _ => return self.bad(n, format!("unknown term type <{}>", t.as_str())),
                })
            }
            Some(other) => return self.bad(n, format!("a term type must be an IRI, not {other}")),
        };
        let datatype = match self.g.one(n, "datatype")? {
            None => None,
            Some(Term::NamedNode(d)) => Some(Dt::Constant(d.clone())),
            Some(other) => return self.bad(n, format!("a datatype must be an IRI, not {other}")),
        };
        let datatype = match (datatype, self.g.one(n, "datatypeMap")?) {
            (d, None) => d,
            (None, Some(m)) => {
                let m = subject_of(m).ok_or_else(|| {
                    Error::invalid(format!("{}: a datatype map is a literal", show(n)))
                })?;
                Some(Dt::Map(Box::new(self.term_map(&m, Role::Value)?)))
            }
            (Some(_), Some(_)) => {
                return self.bad(n, "a term map has both a datatype and a datatype map");
            }
        };
        let language = match self.g.one(n, "language")? {
            None => None,
            Some(Term::Literal(l)) => {
                let tag = l.value();
                if Literal::new_language_tagged_literal("", tag).is_err() {
                    return self.bad(n, format!("{tag:?} is not a well-formed language tag"));
                }
                Some(Lang::Constant(tag.to_string()))
            }
            Some(other) => return self.bad(n, format!("a language must be a string, not {other}")),
        };
        let language = match (language, self.g.one(n, "languageMap")?) {
            (l, None) => l,
            (None, Some(m)) => {
                let m = subject_of(m).ok_or_else(|| {
                    Error::invalid(format!("{}: a language map is a literal", show(n)))
                })?;
                Some(Lang::Map(Box::new(self.term_map(&m, Role::Value)?)))
            }
            (Some(_), Some(_)) => {
                return self.bad(n, "a term map has both a language and a language map");
            }
        };
        if language.is_some() && datatype.is_some() {
            return self.bad(n, "a term map has both a language and a datatype");
        }
        let value = if let Some(c) = constant {
            Value::Constant(c.clone())
        } else if let Some(r) = reference {
            Value::Reference(r)
        } else if let Some(t) = template {
            Value::Template(
                Template::parse(&t)
                    .map(|t| if uri { t.with_uri_safe() } else { t })
                    .map_err(|e| Error::invalid(format!("{}: {e}", show(n))))?,
            )
        } else if let Some(f) = function {
            Value::Function(Box::new(f))
        } else if role == Role::Subject && term_type == Some(TermType::BlankNode) {
            Value::Fresh
        } else {
            return self.bad(
                n,
                "a term map needs a constant, a column or reference, a template or a function",
            );
        };
        let term_type = match term_type {
            Some(t) => t,
            None => match (&value, role) {
                (Value::Constant(Term::Literal(_)), _) => TermType::Literal,
                (Value::Constant(Term::NamedNode(_)), _) => TermType::Iri,
                (Value::Constant(Term::BlankNode(_)), _) => TermType::BlankNode,
                (_, Role::Object | Role::Value)
                    if matches!(value, Value::Reference(_) | Value::Function(_))
                        || language.is_some()
                        || datatype.is_some() =>
                {
                    TermType::Literal
                }
                (_, Role::Value) => TermType::Literal,
                _ => TermType::Iri,
            },
        };
        if (language.is_some() || datatype.is_some()) && term_type != TermType::Literal {
            return self.bad(n, "a language or datatype needs the literal term type");
        }
        if let Value::Constant(c) = &value {
            let ok = matches!(
                (c, term_type),
                (Term::NamedNode(_), TermType::Iri)
                    | (Term::BlankNode(_), TermType::BlankNode)
                    | (Term::Literal(_), TermType::Literal)
            );
            if !ok {
                return self.bad(
                    n,
                    format!("the constant {c} does not have the term map's term type"),
                );
            }
        }
        Ok(TermMap {
            value,
            term_type,
            datatype,
            language,
        })
    }

    /// A function map: the rml.io FNML form (`fnml:functionValue` with `fno:executes`)
    /// or RML-Core's (`rml:functionExecution` with `rml:function` and `rml:input`).
    fn function(&self, n: &NamedOrBlankNode) -> Result<Option<FunctionCall>> {
        let fv = self.g.by_iri(n, &format!("{FNML}functionValue"));
        if let Some(fv) = fv.first() {
            let Some(fv) = subject_of(fv) else {
                return self.bad(n, "a function value is a literal");
            };
            let mut function = None;
            let mut args = Vec::new();
            for po in self.g.all(&fv, "predicateObjectMap") {
                let Some(po) = subject_of(po) else { continue };
                let pom = self.predicate_object(&po)?;
                let pred = pom.predicates.first().and_then(|p| match &p.value {
                    Value::Constant(Term::NamedNode(n)) => Some(n.clone()),
                    _ => None,
                });
                let Some(pred) = pred else {
                    return self.bad(&po, "a function's parameter needs a constant predicate");
                };
                let Some(ObjectMap::Term(obj)) = pom.objects.into_iter().next() else {
                    return self.bad(&po, "a function's parameter needs an object map");
                };
                if pred.as_str() == format!("{FNO}executes")
                    || pred.as_str() == format!("{FNO_HTTP}executes")
                {
                    match obj.value {
                        Value::Constant(Term::NamedNode(f)) => function = Some(f),
                        _ => return self.bad(&po, "fno:executes needs a constant function IRI"),
                    }
                } else {
                    args.push((pred, obj));
                }
            }
            let Some(function) = function else {
                return self.bad(&fv, "the function value has no fno:executes");
            };
            return Ok(Some(FunctionCall { function, args }));
        }
        let Some(fe) = self.g.one(n, "functionExecution")? else {
            return Ok(None);
        };
        let Some(fe) = subject_of(fe) else {
            return self.bad(n, "a function execution is a literal");
        };
        let function = match (
            self.g.one(&fe, "function")?,
            self.g.one(&fe, "functionMap")?,
        ) {
            (Some(Term::NamedNode(f)), _) => f.clone(),
            (None, Some(m)) => match subject_of(m).map(|m| self.g.one(&m, "constant")) {
                Some(Ok(Some(Term::NamedNode(f)))) => f.clone(),
                _ => return self.bad(&fe, "a function map needs a constant function IRI"),
            },
            _ => return self.bad(&fe, "the function execution names no function"),
        };
        let mut args = Vec::new();
        for input in self.g.all(&fe, "input") {
            let Some(input) = subject_of(input) else {
                continue;
            };
            let param = match (
                self.g.one(&input, "parameter")?,
                self.g.one(&input, "parameterMap")?,
            ) {
                (Some(Term::NamedNode(p)), _) => p.clone(),
                (None, Some(m)) => match subject_of(m).map(|m| self.g.one(&m, "constant")) {
                    Some(Ok(Some(Term::NamedNode(p)))) => p.clone(),
                    _ => return self.bad(&input, "a parameter map needs a constant IRI"),
                },
                _ => return self.bad(&input, "an input names no parameter"),
            };
            let value = match (
                self.g.one(&input, "inputValue")?,
                self.g.one(&input, "inputValueMap")?,
            ) {
                (Some(c), _) => TermMap::constant(c.clone()),
                (None, Some(m)) => {
                    let Some(m) = subject_of(m) else {
                        return self.bad(&input, "an input value map is a literal");
                    };
                    self.term_map(&m, Role::Value)?
                }
                _ => return self.bad(&input, "an input has no value"),
            };
            args.push((param, value));
        }
        Ok(Some(FunctionCall { function, args }))
    }

    fn logical_source(&self, t: &NamedOrBlankNode) -> Result<SourceDecl> {
        let lt = self.g.all(t, "logicalTable");
        let ls = self.g.all(t, "logicalSource");
        let node = match (lt.as_slice(), ls.as_slice()) {
            ([one], []) | ([], [one]) => subject_of(one).ok_or_else(|| {
                Error::invalid(format!("{}: the logical source is a literal", show(t)))
            })?,
            ([], []) => return self.bad(t, "the triples map has no logical table or source"),
            _ => {
                return self.bad(
                    t,
                    "the triples map has more than one logical table or source",
                );
            }
        };
        let table = self.g.string(&node, "tableName")?;
        let sql = self
            .g
            .string(&node, "sqlQuery")?
            .or(self.g.string(&node, "query")?);
        let iterator = self.g.string(&node, "iterator")?;
        let formulation = match self.g.one(&node, "referenceFormulation")? {
            None => None,
            Some(Term::NamedNode(f)) => Some(f.as_str().to_string()),
            Some(other) => {
                return self.bad(
                    &node,
                    format!("a reference formulation must be an IRI, not {other}"),
                );
            }
        };
        let local = formulation.as_deref().map(|f| {
            f.strip_prefix(QL)
                .or_else(|| f.strip_prefix(RML))
                .unwrap_or(f)
                .to_string()
        });
        if matches!(
            local.as_deref(),
            Some("XPath" | "XPathReferenceFormulation")
        ) {
            return self.bad(
                &node,
                "XML sources and XPath iterators are not supported (spec F19, open question 1)",
            );
        }
        let source = self.g.one(&node, "source")?;
        // an R2RML logical table: a table or query of the run's default connection
        if source.is_none() {
            if let Some(t) = table {
                return Ok(sql_decl("", SqlQuery::Table(t)));
            }
            if let Some(q) = sql {
                return Ok(sql_decl("", SqlQuery::Query(q)));
            }
            return self.bad(
                &node,
                "the logical source has no source, table name or query",
            );
        }
        let source = source.expect("checked");
        let sql_query = |iter: Option<String>| -> Option<SqlQuery> {
            if let Some(t) = &table {
                return Some(SqlQuery::Table(t.clone()));
            }
            if let Some(q) = &sql {
                return Some(SqlQuery::Query(q.clone()));
            }
            match local.as_deref() {
                Some("SQL2008Table") => iter.map(SqlQuery::Table),
                Some("SQL2008Query") => iter.map(SqlQuery::Query),
                _ => iter.map(SqlQuery::Table),
            }
        };
        match source {
            Term::Literal(l) => {
                let file = l.value().to_string();
                self.file_decl(
                    &node,
                    file,
                    local.as_deref(),
                    iterator,
                    DialectDesc::default(),
                    Vec::new(),
                )
            }
            Term::NamedNode(n) if n.as_str().starts_with(CONNECTION_PREFIX) => {
                let name = n.as_str()[CONNECTION_PREFIX.len()..].to_string();
                let q = sql_query(iterator).ok_or_else(|| {
                    Error::invalid(format!(
                        "{}: a database source needs a table name or a query",
                        show(&node)
                    ))
                })?;
                Ok(sql_decl(&name, q))
            }
            Term::NamedNode(_) | Term::BlankNode(_) => {
                let s = subject_of(source).expect("a node");
                if self.g.is_a(&s, &format!("{D2RQ}Database"))
                    || !self.g.by_iri(&s, &format!("{D2RQ}jdbcDSN")).is_empty()
                {
                    for k in ["username", "password"] {
                        if !self.g.by_iri(&s, &format!("{D2RQ}{k}")).is_empty() {
                            return self.bad(
                                &s,
                                format!("d2rq:{k} is refused: credentials belong in a connection declared on the server or the command line, never in a mapping"),
                            );
                        }
                    }
                    let dsn = match self.g.by_iri(&s, &format!("{D2RQ}jdbcDSN")).first() {
                        Some(Term::Literal(l)) => l.value().to_string(),
                        _ => return self.bad(&s, "a D2RQ database needs a d2rq:jdbcDSN"),
                    };
                    let q = sql_query(iterator).ok_or_else(|| {
                        Error::invalid(format!(
                            "{}: a database source needs a table name or a query",
                            show(&node)
                        ))
                    })?;
                    return Ok(sql_decl(&dsn, q));
                }
                if let Some(Term::Literal(u)) = self.g.by_iri(&s, &format!("{CSVW}url")).first() {
                    let (dialect, null) = self.csvw_dialect(&s)?;
                    return self.file_decl(
                        &node,
                        u.value().to_string(),
                        local.as_deref(),
                        iterator,
                        dialect,
                        null,
                    );
                }
                // RML-Core and RML-IO file sources: a path
                let path = self.g.string(&s, "path")?.or(self.g.string(&s, "url")?);
                if let Some(p) = path {
                    let null: Vec<String> = self
                        .g
                        .all(&s, "null")
                        .into_iter()
                        .filter_map(|t| match t {
                            Term::Literal(l) => Some(l.value().to_string()),
                            _ => None,
                        })
                        .collect();
                    return self.file_decl(
                        &node,
                        p,
                        local.as_deref(),
                        iterator,
                        DialectDesc::default(),
                        null,
                    );
                }
                self.bad(&s, "the source is not a file path, a CSVW table, a D2RQ database or a urn:x-sparkles:connection: IRI")
            }
            #[allow(unreachable_patterns)]
            _ => self.bad(&node, "the source is not supported"),
        }
    }

    fn csvw_dialect(&self, s: &NamedOrBlankNode) -> Result<(DialectDesc, Vec<String>)> {
        let mut d = DialectDesc::default();
        let null: Vec<String> = self
            .g
            .by_iri(s, &format!("{CSVW}null"))
            .into_iter()
            .filter_map(|t| match t {
                Term::Literal(l) => Some(l.value().to_string()),
                _ => None,
            })
            .collect();
        let Some(dn) = self
            .g
            .by_iri(s, &format!("{CSVW}dialect"))
            .first()
            .and_then(|t| subject_of(t))
        else {
            return Ok((d, null));
        };
        let lit = |k: &str| -> Option<String> {
            match self.g.by_iri(&dn, &format!("{CSVW}{k}")).first() {
                Some(Term::Literal(l)) => Some(l.value().to_string()),
                _ => None,
            }
        };
        let one_byte = |k: &str, v: &str| -> Result<u8> {
            match v.as_bytes() {
                [b] => Ok(*b),
                _ => Err(Error::invalid(format!(
                    "csvw:{k} {v:?} must be one ASCII character"
                ))),
            }
        };
        if let Some(v) = lit("delimiter") {
            d.delimiter = Some(one_byte("delimiter", &v)?);
        }
        if let Some(v) = lit("quoteChar") {
            d.quote = Some(if v.is_empty() {
                None
            } else {
                Some(one_byte("quoteChar", &v)?)
            });
        }
        if let Some(v) = lit("header") {
            d.header = Some(v == "true" || v == "1");
        }
        if let Some(v) = lit("headerRowCount") {
            d.header_rows = v.parse().ok();
        }
        if let Some(v) = lit("skipRows") {
            d.skip_rows = v.parse().ok();
        }
        if let Some(v) = lit("doubleQuote") {
            d.double_quote = Some(v == "true" || v == "1");
        }
        if let Some(v) = lit("commentPrefix") {
            d.comment = Some(if v.is_empty() {
                None
            } else {
                Some(one_byte("commentPrefix", &v)?)
            });
        }
        if let Some(v) = lit("trim") {
            d.trim = Some(match v.as_str() {
                "false" => (false, false),
                "start" => (true, false),
                "end" => (false, true),
                _ => (true, true),
            });
        }
        if let Some(v) = lit("encoding")
            && !v.eq_ignore_ascii_case("utf-8")
            && !v.eq_ignore_ascii_case("utf8")
        {
            return Err(Error::invalid(format!(
                "csvw:encoding {v:?}: sources must be UTF-8"
            )));
        }
        Ok((d, null))
    }

    fn file_decl(
        &self,
        node: &NamedOrBlankNode,
        file: String,
        formulation: Option<&str>,
        iterator: Option<String>,
        dialect: DialectDesc,
        null: Vec<String>,
    ) -> Result<SourceDecl> {
        let lower = file.to_ascii_lowercase();
        let lower = crate::codec::Codec::strip_extension(&lower).to_string();
        let kind = match formulation {
            Some("CSV") => SourceKind::Csv {
                tsv: lower.ends_with(".tsv") || lower.ends_with(".tab"),
                file,
                dialect,
                null,
            },
            Some("JSONPath") => SourceKind::Json {
                lines: lower.ends_with(".jsonl") || lower.ends_with(".ndjson"),
                file,
            },
            Some("JSONLines") => SourceKind::Json { lines: true, file },
            Some("Parquet") => SourceKind::Parquet { file },
            Some(other) => {
                return self.bad(node, format!("the reference formulation {other} is not supported; CSV, JSONPath and Parquet are"));
            }
            None => {
                if lower.ends_with(".csv") {
                    SourceKind::Csv {
                        tsv: false,
                        file,
                        dialect,
                        null,
                    }
                } else if lower.ends_with(".tsv") || lower.ends_with(".tab") {
                    SourceKind::Csv {
                        tsv: true,
                        file,
                        dialect,
                        null,
                    }
                } else if lower.ends_with(".jsonl") || lower.ends_with(".ndjson") {
                    SourceKind::Json { lines: true, file }
                } else if lower.ends_with(".json") {
                    SourceKind::Json { lines: false, file }
                } else if lower.ends_with(".parquet") {
                    SourceKind::Parquet { file }
                } else {
                    return self.bad(node, format!("the source {file:?} names no reference formulation, and its extension is not .csv, .tsv, .json, .jsonl or .parquet"));
                }
            }
        };
        let iterator = match &kind {
            SourceKind::Json { .. } => iterator,
            _ => None,
        };
        Ok(SourceDecl { kind, iterator })
    }
}

fn sql_decl(connection: &str, query: SqlQuery) -> SourceDecl {
    SourceDecl {
        kind: SourceKind::Sql {
            connection: connection.to_string(),
            query,
        },
        iterator: None,
    }
}

// ------------------------------------------------------------------- printing ----

fn turtle_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn turtle_term(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => format!("<{}>", n.as_str()),
        Term::BlankNode(b) => format!("_:{}", b.as_str()),
        Term::Literal(l) => {
            if let Some(lang) = l.language() {
                format!("{}@{lang}", turtle_string(l.value()))
            } else if l.datatype() == oxrdf::vocab::xsd::STRING {
                turtle_string(l.value())
            } else {
                format!("{}^^<{}>", turtle_string(l.value()), l.datatype().as_str())
            }
        }
        #[allow(unreachable_patterns)]
        other => other.to_string(),
    }
}

/// The node of a triples map in printed Turtle: its IRI, or a blank node label.
fn map_node(id: &str) -> String {
    if id.starts_with('<') || id.starts_with("_:") {
        return id.to_string();
    }
    let label: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("_:{label}")
}

struct Printer {
    /// `rml:` for RML-Core, `rr:` for R2RML
    p: &'static str,
    r2rml: bool,
}

impl Printer {
    fn term_map(
        &self,
        t: &TermMap,
        role_default: TermType,
        out: &mut String,
        indent: &str,
    ) -> Result<()> {
        let p = self.p;
        out.push_str("[\n");
        let inner = format!("{indent}    ");
        match &t.value {
            Value::Constant(c) => {
                out.push_str(&format!("{inner}{p}:constant {} ;\n", turtle_term(c)))
            }
            Value::Reference(r) => {
                let k = if self.r2rml { "column" } else { "reference" };
                out.push_str(&format!("{inner}{p}:{k} {} ;\n", turtle_string(r)));
            }
            Value::Template(tp) => out.push_str(&format!(
                "{inner}{p}:template {} ;\n",
                turtle_string(tp.as_str())
            )),
            Value::Function(f) => {
                if self.r2rml {
                    return Err(Error::invalid(
                        "R2RML has no functions; convert to RML instead",
                    ));
                }
                out.push_str(&format!("{inner}rml:functionExecution [\n"));
                out.push_str(&format!(
                    "{inner}    rml:function <{}> ;\n",
                    f.function.as_str()
                ));
                for (param, v) in &f.args {
                    out.push_str(&format!(
                        "{inner}    rml:input [ rml:parameter <{}> ; rml:inputValueMap ",
                        param.as_str()
                    ));
                    self.term_map(v, TermType::Literal, out, &format!("{inner}        "))?;
                    out.push_str(" ] ;\n");
                }
                out.push_str(&format!("{inner}] ;\n"));
            }
            Value::Fresh => {}
        }
        if t.term_type != role_default || matches!(t.value, Value::Fresh) {
            let tt = match t.term_type {
                TermType::Iri => "IRI",
                TermType::BlankNode => "BlankNode",
                TermType::Literal => "Literal",
            };
            out.push_str(&format!("{inner}{p}:termType {p}:{tt} ;\n"));
        }
        match &t.datatype {
            Some(Dt::Constant(d)) => {
                out.push_str(&format!("{inner}{p}:datatype <{}> ;\n", d.as_str()))
            }
            Some(Dt::Map(m)) => {
                if self.r2rml {
                    return Err(Error::invalid(
                        "R2RML has no datatype maps; convert to RML instead",
                    ));
                }
                out.push_str(&format!("{inner}rml:datatypeMap "));
                self.term_map(m, TermType::Iri, out, &inner)?;
                out.push_str(" ;\n");
            }
            None => {}
        }
        match &t.language {
            Some(Lang::Constant(l)) => {
                out.push_str(&format!("{inner}{p}:language {} ;\n", turtle_string(l)))
            }
            Some(Lang::Map(m)) => {
                if self.r2rml {
                    return Err(Error::invalid(
                        "R2RML has no language maps; convert to RML instead",
                    ));
                }
                out.push_str(&format!("{inner}rml:languageMap "));
                self.term_map(m, TermType::Literal, out, &inner)?;
                out.push_str(" ;\n");
            }
            None => {}
        }
        out.push_str(indent);
        out.push(']');
        Ok(())
    }

    fn source(&self, d: &SourceDecl, out: &mut String) -> Result<()> {
        let p = self.p;
        match &d.kind {
            SourceKind::Sql { connection, query } => {
                if self.r2rml {
                    out.push_str("    rr:logicalTable [ ");
                    match query {
                        SqlQuery::Table(t) => {
                            out.push_str(&format!("rr:tableName {}", turtle_string(t)))
                        }
                        SqlQuery::Query(q) => {
                            out.push_str(&format!("rr:sqlQuery {}", turtle_string(q)))
                        }
                    }
                    out.push_str(" ] ;\n");
                    return Ok(());
                }
                let name = if connection.is_empty() {
                    "default"
                } else {
                    connection
                };
                let (f, it) = match query {
                    SqlQuery::Table(t) => ("SQL2008Table", t),
                    SqlQuery::Query(q) => ("SQL2008Query", q),
                };
                out.push_str(&format!(
                    "    rml:logicalSource [\n        rml:source <{CONNECTION_PREFIX}{name}> ;\n        rml:referenceFormulation rml:{f} ;\n        rml:iterator {}\n    ] ;\n",
                    turtle_string(it)
                ));
            }
            kind => {
                if self.r2rml {
                    return Err(Error::invalid(format!(
                        "R2RML reads databases only, and the mapping reads the {}; convert to RML instead",
                        kind.describe()
                    )));
                }
                let file = kind.file().unwrap_or_default();
                out.push_str("    rml:logicalSource [\n");
                match kind {
                    SourceKind::Csv { dialect, null, .. }
                        if *dialect != DialectDesc::default() || !null.is_empty() =>
                    {
                        out.push_str(&format!(
                            "        rml:source [ a csvw:Table ; csvw:url {}",
                            turtle_string(file)
                        ));
                        for n in null {
                            out.push_str(&format!(" ; csvw:null {}", turtle_string(n)));
                        }
                        if let Some(d) = dialect.delimiter {
                            out.push_str(&format!(
                                " ; csvw:dialect [ csvw:delimiter {} ]",
                                turtle_string(&char::from(d).to_string())
                            ));
                        }
                        out.push_str(" ] ;\n");
                    }
                    _ => out.push_str(&format!(
                        "        rml:source [ a rml:RelativePathSource ; rml:path {} ] ;\n",
                        turtle_string(file)
                    )),
                }
                match kind {
                    SourceKind::Csv { .. } => {
                        out.push_str(&format!("        {p}:referenceFormulation rml:CSV ;\n"))
                    }
                    SourceKind::Json { .. } => out.push_str(&format!(
                        "        {p}:referenceFormulation rml:JSONPath ;\n"
                    )),
                    _ => {}
                }
                if let Some(it) = &d.iterator {
                    out.push_str(&format!("        rml:iterator {} ;\n", turtle_string(it)));
                }
                out.push_str("    ] ;\n");
            }
        }
        Ok(())
    }
}

/// The mapping written as RML-Core Turtle (or R2RML when `r2rml` and every source is
/// SQL), for `sparkles map convert`.
pub fn to_turtle(m: &Mapping, r2rml: bool) -> Result<String> {
    let pr = Printer {
        p: if r2rml { "rr" } else { "rml" },
        r2rml,
    };
    let mut out = String::new();
    if r2rml {
        out.push_str(&format!("@prefix rr: <{RR}> .\n"));
    } else {
        out.push_str(&format!(
            "@prefix rml: <{RML}> .\n@prefix csvw: <{CSVW}> .\n"
        ));
    }
    for (p, ns) in &m.prefixes {
        if p != "rr" && p != "rml" && p != "csvw" {
            out.push_str(&format!("@prefix {p}: <{ns}> .\n"));
        }
    }
    if let Some(b) = &m.base {
        out.push_str(&format!("@base <{b}> .\n"));
    }
    out.push('\n');
    let p = pr.p;
    for tm in &m.maps {
        out.push_str(&format!("{} a {p}:TriplesMap ;\n", map_node(&tm.id)));
        pr.source(&m.sources[tm.source], &mut out)?;
        out.push_str(&format!("    {p}:subjectMap "));
        pr.term_map(&tm.subject, TermType::Iri, &mut out, "    ")?;
        // classes and graphs go inside the subject map
        if !tm.classes.is_empty() || !tm.graphs.is_empty() {
            out.truncate(out.len() - 1);
            for c in &tm.classes {
                out.push_str(&format!("    {p}:class <{}> ;\n", c.as_str()));
            }
            for g in &tm.graphs {
                out.push_str(&format!("    {p}:graphMap "));
                pr.term_map(g, TermType::Iri, &mut out, "    ")?;
                out.push_str(" ;\n");
            }
            out.push_str("    ]");
        }
        for po in &tm.predicate_objects {
            out.push_str(&format!(" ;\n    {p}:predicateObjectMap [\n"));
            for pm in &po.predicates {
                out.push_str(&format!("        {p}:predicateMap "));
                pr.term_map(pm, TermType::Iri, &mut out, "        ")?;
                out.push_str(" ;\n");
            }
            for o in &po.objects {
                match o {
                    ObjectMap::Term(t) => {
                        let default = match &t.value {
                            Value::Reference(_) | Value::Function(_) => TermType::Literal,
                            _ if t.language.is_some() || t.datatype.is_some() => TermType::Literal,
                            Value::Constant(Term::Literal(_)) => TermType::Literal,
                            _ => TermType::Iri,
                        };
                        out.push_str(&format!("        {p}:objectMap "));
                        pr.term_map(t, default, &mut out, "        ")?;
                        out.push_str(" ;\n");
                    }
                    ObjectMap::Join { parent, conditions } => {
                        out.push_str(&format!(
                            "        {p}:objectMap [\n            {p}:parentTriplesMap {} ;\n",
                            map_node(&m.maps[*parent].id)
                        ));
                        for (c, pa) in conditions {
                            out.push_str(&format!(
                                "            {p}:joinCondition [ {p}:child {} ; {p}:parent {} ] ;\n",
                                turtle_string(c),
                                turtle_string(pa)
                            ));
                        }
                        out.push_str("        ] ;\n");
                    }
                }
            }
            for g in &po.graphs {
                out.push_str(&format!("        {p}:graphMap "));
                pr.term_map(g, TermType::Iri, &mut out, "        ")?;
                out.push_str(" ;\n");
            }
            out.push_str("    ]");
        }
        out.push_str(" .\n\n");
    }
    Ok(out)
}
