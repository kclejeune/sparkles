use oxrdf::{
    BaseDirection, BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple,
};
use serde_json::{Value, json};
use sparkles::{Error, Result};

pub fn term(v: &Value) -> Result<Term> {
    let value = v["value"]
        .as_str()
        .ok_or_else(|| Error::invalid("term.value must be a string"))?;
    Ok(match v["termType"].as_str() {
        Some("NamedNode") => NamedNode::new(value)
            .map_err(|e| Error::invalid(e.to_string()))?
            .into(),
        Some("BlankNode") => BlankNode::new(value)
            .map_err(|e| Error::invalid(e.to_string()))?
            .into(),
        Some("Literal") => {
            let language = v["language"].as_str().unwrap_or("");
            let direction = v["direction"].as_str().unwrap_or("");
            if !language.is_empty() {
                if direction.is_empty() {
                    Literal::new_language_tagged_literal(value, language)
                        .map_err(|e| Error::invalid(e.to_string()))?
                } else {
                    let direction = match direction {
                        "ltr" => BaseDirection::Ltr,
                        "rtl" => BaseDirection::Rtl,
                        _ => return Err(Error::invalid("invalid literal direction")),
                    };
                    Literal::new_directional_language_tagged_literal(value, language, direction)
                        .map_err(|e| Error::invalid(e.to_string()))?
                }
            } else {
                if !direction.is_empty() {
                    return Err(Error::invalid("direction requires a language"));
                }
                let datatype = v["datatype"]["value"]
                    .as_str()
                    .unwrap_or("http://www.w3.org/2001/XMLSchema#string");
                Literal::new_typed_literal(
                    value,
                    NamedNode::new(datatype).map_err(|e| Error::invalid(e.to_string()))?,
                )
            }
            .into()
        }
        Some("Quad") => {
            let q = quad(v)?;
            if !q.graph_name.is_default_graph() {
                return Err(Error::invalid("a triple term must have the default graph"));
            }
            Term::Triple(Box::new(Triple::new(q.subject, q.predicate, q.object)))
        }
        _ => return Err(Error::invalid("expected an RDF term")),
    })
}

pub fn node(v: &Value) -> Result<NamedOrBlankNode> {
    match term(v)? {
        Term::NamedNode(n) => Ok(n.into()),
        Term::BlankNode(n) => Ok(n.into()),
        _ => Err(Error::invalid(
            "subject and graph must be an IRI or blank node",
        )),
    }
}

pub fn quad(v: &Value) -> Result<Quad> {
    let predicate = match term(&v["predicate"])? {
        Term::NamedNode(n) => n,
        _ => return Err(Error::invalid("predicate must be an IRI")),
    };
    let graph = if v["graph"]["termType"] == "DefaultGraph" {
        GraphName::DefaultGraph
    } else {
        match node(&v["graph"])? {
            NamedOrBlankNode::NamedNode(n) => n.into(),
            NamedOrBlankNode::BlankNode(n) => n.into(),
        }
    };
    Ok(Quad::new(
        node(&v["subject"])?,
        predicate,
        term(&v["object"])?,
        graph,
    ))
}

pub fn encode(t: &Term) -> Value {
    match t {
        Term::NamedNode(n) => json!({"termType":"NamedNode","value":n.as_str()}),
        Term::BlankNode(n) => json!({"termType":"BlankNode","value":n.as_str()}),
        Term::Literal(l) => {
            json!({"termType":"Literal","value":l.value(),"language":l.language().unwrap_or(""),"direction":l.direction().map(|d| match d { BaseDirection::Ltr => "ltr", BaseDirection::Rtl => "rtl" }).unwrap_or(""),"datatype":{"value":l.datatype().as_str()}})
        }
        Term::Triple(t) => encode_quad(&Quad::new(
            t.subject.clone(),
            t.predicate.clone(),
            t.object.clone(),
            GraphName::DefaultGraph,
        )),
    }
}

pub fn encode_quad(q: &Quad) -> Value {
    let subject = match &q.subject {
        NamedOrBlankNode::NamedNode(n) => encode(&n.clone().into()),
        NamedOrBlankNode::BlankNode(n) => encode(&n.clone().into()),
    };
    let graph = match &q.graph_name {
        GraphName::DefaultGraph => json!({"termType":"DefaultGraph","value":""}),
        GraphName::NamedNode(n) => encode(&n.clone().into()),
        GraphName::BlankNode(n) => encode(&n.clone().into()),
    };
    json!({"termType":"Quad","value":"","subject":subject,"predicate":encode(&q.predicate.clone().into()),"object":encode(&q.object),"graph":graph})
}
