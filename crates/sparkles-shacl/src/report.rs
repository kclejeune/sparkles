//! Validation reports (SHACL §3.6) and their RDF form.

use crate::path::PropertyPath;
use crate::vocab::{rdf, sh};
use anyhow::{Result, anyhow};
use oxrdf::vocab::xsd;
use oxrdf::{BlankNode, Graph, Literal, NamedNode, Term, Triple};
use oxttl::TurtleSerializer;
use std::fmt;

/// One validation result (`sh:ValidationResult`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ValidationResult {
    pub focus_node: Term,
    pub result_path: Option<PropertyPath>,
    pub value: Option<Term>,
    pub source_shape: Term,
    pub source_constraint_component: NamedNode,
    /// `sh:sourceConstraint` (SHACL-SPARQL constraints)
    pub source_constraint: Option<Term>,
    pub severity: NamedNode,
    /// `sh:resultMessage` values (from `sh:message`, or a generated message)
    pub messages: Vec<Literal>,
}

impl ValidationResult {
    /// The first message text, if any.
    pub fn message(&self) -> Option<&str> {
        self.messages.first().map(|l| l.value())
    }
}

/// A validation report (`sh:ValidationReport`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidationReport {
    pub conforms: bool,
    pub results: Vec<ValidationResult>,
}

impl ValidationReport {
    /// Number of results with severity `sh:Violation`.
    pub fn violations(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.severity.as_ref() == sh::VIOLATION)
            .count()
    }

    /// The report as RDF triples (`[] a sh:ValidationReport ; sh:conforms … ; sh:result …`).
    pub fn to_rdf(&self) -> Vec<Triple> {
        let mut out = Vec::new();
        let report = BlankNode::default();
        out.push(Triple::new(
            report.clone(),
            rdf::TYPE.into_owned(),
            sh::VALIDATION_REPORT.into_owned(),
        ));
        out.push(Triple::new(
            report.clone(),
            sh::CONFORMS.into_owned(),
            Literal::new_typed_literal(self.conforms.to_string(), xsd::BOOLEAN),
        ));
        for r in &self.results {
            let n = BlankNode::default();
            out.push(Triple::new(
                report.clone(),
                sh::RESULT.into_owned(),
                n.clone(),
            ));
            out.push(Triple::new(
                n.clone(),
                rdf::TYPE.into_owned(),
                sh::VALIDATION_RESULT.into_owned(),
            ));
            out.push(Triple::new(
                n.clone(),
                sh::FOCUS_NODE.into_owned(),
                r.focus_node.clone(),
            ));
            if let Some(p) = &r.result_path {
                let pn = p.to_rdf(&mut out);
                out.push(Triple::new(n.clone(), sh::RESULT_PATH.into_owned(), pn));
            }
            if let Some(v) = &r.value {
                out.push(Triple::new(n.clone(), sh::VALUE.into_owned(), v.clone()));
            }
            out.push(Triple::new(
                n.clone(),
                sh::SOURCE_SHAPE.into_owned(),
                r.source_shape.clone(),
            ));
            out.push(Triple::new(
                n.clone(),
                sh::SOURCE_CONSTRAINT_COMPONENT.into_owned(),
                r.source_constraint_component.clone(),
            ));
            if let Some(c) = &r.source_constraint {
                out.push(Triple::new(
                    n.clone(),
                    sh::SOURCE_CONSTRAINT.into_owned(),
                    c.clone(),
                ));
            }
            out.push(Triple::new(
                n.clone(),
                sh::RESULT_SEVERITY.into_owned(),
                r.severity.clone(),
            ));
            for m in &r.messages {
                out.push(Triple::new(
                    n.clone(),
                    sh::RESULT_MESSAGE.into_owned(),
                    m.clone(),
                ));
            }
        }
        out
    }

    /// The report as an oxrdf graph.
    pub fn to_graph(&self) -> Graph {
        let mut g = Graph::new();
        for t in self.to_rdf() {
            g.insert(&t);
        }
        g
    }

    /// Turtle serialization of [`to_rdf`](Self::to_rdf).
    pub fn to_turtle(&self) -> String {
        let mut ser = TurtleSerializer::new();
        for (p, ns) in [
            ("sh", crate::vocab::SH_NS),
            ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
            ("xsd", "http://www.w3.org/2001/XMLSchema#"),
        ] {
            ser = ser.with_prefix(p, ns).expect("valid prefix");
        }
        let mut w = ser.for_writer(Vec::new());
        for t in self.to_rdf() {
            w.serialize_triple(&t)
                .expect("writing to a Vec cannot fail");
        }
        String::from_utf8(w.finish().expect("writing to a Vec cannot fail"))
            .expect("Turtle is UTF-8")
    }

    /// Read a report from RDF (the first `sh:ValidationReport` node, or `node`).
    pub fn from_rdf(graph: &Graph, node: Option<&Term>) -> Result<ValidationReport> {
        let g = crate::shapes::G { g: graph };
        let report = match node {
            Some(n) => n.clone(),
            None => g
                .subjects(rdf::TYPE, &sh::VALIDATION_REPORT.into_owned().into())
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("no sh:ValidationReport in graph"))?,
        };
        let conforms = match g.object(&report, sh::CONFORMS) {
            Some(Term::Literal(l)) => l.value() == "true" || l.value() == "1",
            _ => return Err(anyhow!("report {report} has no sh:conforms")),
        };
        let mut results = Vec::new();
        for r in g.objects(&report, sh::RESULT) {
            let named = |p| match g.object(&r, p) {
                Some(Term::NamedNode(n)) => Some(n),
                _ => None,
            };
            results.push(ValidationResult {
                focus_node: g
                    .object(&r, sh::FOCUS_NODE)
                    .ok_or_else(|| anyhow!("result {r} has no sh:focusNode"))?,
                result_path: g
                    .object(&r, sh::RESULT_PATH)
                    .map(|p| PropertyPath::from_rdf(graph, &p))
                    .transpose()?,
                value: g.object(&r, sh::VALUE),
                source_shape: g
                    .object(&r, sh::SOURCE_SHAPE)
                    .ok_or_else(|| anyhow!("result {r} has no sh:sourceShape"))?,
                source_constraint_component: named(sh::SOURCE_CONSTRAINT_COMPONENT)
                    .ok_or_else(|| anyhow!("result {r} has no sh:sourceConstraintComponent"))?,
                source_constraint: g.object(&r, sh::SOURCE_CONSTRAINT),
                severity: named(sh::RESULT_SEVERITY).unwrap_or(sh::VIOLATION.into_owned()),
                messages: g
                    .objects(&r, sh::RESULT_MESSAGE)
                    .into_iter()
                    .filter_map(|t| match t {
                        Term::Literal(l) => Some(l),
                        _ => None,
                    })
                    .collect(),
            });
        }
        Ok(ValidationReport { conforms, results })
    }
}

impl fmt::Display for ValidationReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "conforms: {} ({} result{})",
            self.conforms,
            self.results.len(),
            if self.results.len() == 1 { "" } else { "s" }
        )?;
        for r in &self.results {
            let sev = r
                .severity
                .as_str()
                .strip_prefix(crate::vocab::SH_NS)
                .unwrap_or(r.severity.as_str());
            let comp = r
                .source_constraint_component
                .as_str()
                .strip_prefix(crate::vocab::SH_NS)
                .unwrap_or(r.source_constraint_component.as_str());
            write!(f, "  [{sev}] {comp} focus={}", r.focus_node)?;
            if let Some(p) = &r.result_path {
                write!(f, " path={p}")?;
            }
            if let Some(v) = &r.value {
                write!(f, " value={v}")?;
            }
            write!(f, " shape={}", r.source_shape)?;
            if let Some(m) = r.message() {
                write!(f, " : {m}")?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

/// Compact JSON form of one result: terms are SPARQL-JSON term objects, and a complex
/// result path is `{"type": "path", "value": "<SPARQL property path>"}`.
pub fn result_json(r: &ValidationResult) -> serde_json::Value {
    use serde_json::json;
    use sparkles::sparql::results::term_json;
    let path = |p: &PropertyPath| match p {
        PropertyPath::Predicate(n) => term_json(&n.clone().into()),
        p => json!({ "type": "path", "value": p.to_string() }),
    };
    let mut o = json!({
        "focusNode": term_json(&r.focus_node),
        "resultPath": r.result_path.as_ref().map(path),
        "value": r.value.as_ref().map(term_json),
        "sourceShape": term_json(&r.source_shape),
        "sourceConstraintComponent": term_json(&r.source_constraint_component.clone().into()),
        "severity": term_json(&r.severity.clone().into()),
        "messages": r.messages.iter().map(|m| m.value()).collect::<Vec<_>>(),
    });
    if let Some(c) = &r.source_constraint {
        o["sourceConstraint"] = term_json(c);
    }
    o
}

/// Compact JSON form of a report: `{conforms, results}`.
pub fn to_json(report: &ValidationReport) -> serde_json::Value {
    serde_json::json!({
        "conforms": report.conforms,
        "results": report.results.iter().map(result_json).collect::<Vec<_>>(),
    })
}
