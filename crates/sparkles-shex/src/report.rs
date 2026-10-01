//! Result-map writers: the Sparkles JSON report, the ShapeMap JSON result map, the
//! compact result map and Jena's text report.

use crate::{PrefixMap, ResultMap, ShapeLabel, ShapeResult, Status};
use oxrdf::{BlankNode, NamedNode, Term};
use serde_json::{Value as J, json};
use sparkles::sparql::results::term_json;

/// An IRI as a prefixed name with the longest namespace of `prefixes` whose remainder
/// is a plain local name, or as `<iri>`.
pub fn compact_iri(iri: &str, prefixes: &PrefixMap) -> String {
    prefixes
        .iter()
        .filter(|(_, ns)| !ns.is_empty() && iri.starts_with(ns.as_str()))
        .filter(|(_, ns)| is_plain_local(&iri[ns.len()..]))
        .max_by_key(|(_, ns)| ns.len())
        .map_or_else(
            || format!("<{iri}>"),
            |(p, ns)| format!("{p}:{}", &iri[ns.len()..]),
        )
}

/// A local name that can be written after `prefix:` without escapes: letters, digits,
/// `_`, `-`, `.` (not last) and `:`, not starting with `-` or `.`.
fn is_plain_local(s: &str) -> bool {
    let ok = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':');
    s.chars().all(ok) && !s.starts_with(['-', '.']) && !s.ends_with('.')
}

/// A shape label in the compact syntax: `<iri>`, `_:label` or `START`.
pub fn label_str(l: &ShapeLabel) -> String {
    match l {
        ShapeLabel::Iri(i) => format!("<{i}>"),
        ShapeLabel::BNode(b) => format!("_:{b}"),
        ShapeLabel::Start => "START".to_string(),
    }
}

/// A shape label as a SPARQL JSON term, `{"type": "start"}` for START.
fn label_json(l: &ShapeLabel) -> J {
    match l {
        ShapeLabel::Iri(i) => term_json(&NamedNode::new_unchecked(i.as_str()).into()),
        ShapeLabel::BNode(b) => term_json(&BlankNode::new_unchecked(b.as_str()).into()),
        ShapeLabel::Start => json!({"type": "start"}),
    }
}

fn status_str(s: Status) -> &'static str {
    match s {
        Status::Conformant => "conformant",
        Status::Nonconformant => "nonconformant",
    }
}

/// `appinfo`: the failures and the prints, `None` when there are neither.
fn appinfo(r: &ShapeResult) -> Option<J> {
    if r.failures.is_empty() && r.prints.is_empty() {
        return None;
    }
    let mut o = serde_json::Map::new();
    o.insert(
        "failures".into(),
        serde_json::to_value(&r.failures).unwrap_or_else(|_| json!([])),
    );
    if !r.prints.is_empty() {
        o.insert("prints".into(), json!(r.prints));
    }
    Some(J::Object(o))
}

/// One result, with `node` and `shape` as given.
fn result_json(r: &ShapeResult, node: J, shape: J) -> J {
    let mut o = serde_json::Map::new();
    o.insert("node".into(), node);
    o.insert("shape".into(), shape);
    o.insert("status".into(), status_str(r.status).into());
    if let Some(reason) = &r.reason {
        o.insert("reason".into(), reason.as_str().into());
    }
    if let Some(a) = appinfo(r) {
        o.insert("appinfo".into(), a);
    }
    J::Object(o)
}

/// See [`ResultMap::to_json`].
pub fn to_json(r: &ResultMap) -> serde_json::Value {
    json!({
        "conforms": r.conforms,
        "counts": {"conformant": r.conformant, "nonconformant": r.nonconformant},
        "results": r
            .results
            .iter()
            .map(|x| result_json(x, term_json(&x.node), label_json(&x.shape)))
            .collect::<Vec<_>>(),
        "warnings": r.warnings,
        "millis": r.millis,
    })
}

/// See [`ResultMap::to_shapemap_json`].
pub fn to_shapemap_json(r: &ResultMap) -> serde_json::Value {
    J::Array(
        r.results
            .iter()
            .map(|x| result_json(x, x.node.to_string().into(), label_str(&x.shape).into()))
            .collect(),
    )
}

/// See [`ResultMap::to_smap`].
pub fn to_smap(r: &ResultMap) -> String {
    let mut s = String::new();
    for x in &r.results {
        let bang = if x.status == Status::Nonconformant {
            "!"
        } else {
            ""
        };
        s.push_str(&format!("{}@{bang}{}\n", x.node, label_str(&x.shape)));
    }
    s
}

/// See [`ResultMap::to_text`].
pub fn to_text(r: &ResultMap) -> String {
    if r.conforms {
        return "OK\n".to_string();
    }
    let mut s = String::new();
    for x in &r.results {
        let node = term_str(&x.node);
        s.push_str(&format!(
            "{node} @ {} :: Focus = {node}, Status = {}",
            label_str(&x.shape),
            status_str(x.status)
        ));
        if x.status == Status::Nonconformant {
            s.push_str(", Reason = ");
            s.push_str(x.reason.as_deref().unwrap_or("--"));
        }
        s.push('\n');
    }
    s
}

/// A term in N-Triples syntax.
fn term_str(t: &Term) -> String {
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShexFailure;
    use oxrdf::Literal;

    fn iri(s: &str) -> Term {
        NamedNode::new_unchecked(s).into()
    }

    /// The acceptance example: alice and bob conform, carol does not.
    fn results() -> ResultMap {
        let person = ShapeLabel::Iri("http://ex.org/Person".into());
        let ok = |n: &str| ShapeResult {
            node: iri(n),
            shape: person.clone(),
            status: Status::Conformant,
            reason: None,
            failures: vec![],
            prints: vec![],
        };
        let age: Term = Literal::from(200).into();
        ResultMap {
            conforms: false,
            conformant: 2,
            nonconformant: 1,
            results: vec![
                ok("http://ex.org/alice"),
                ok("http://ex.org/bob"),
                ShapeResult {
                    node: iri("http://ex.org/carol"),
                    shape: person.clone(),
                    status: Status::Nonconformant,
                    reason: Some("ex:carol: foaf:age 200 fails MAXINCLUSIVE 150".into()),
                    failures: vec![ShexFailure::Facet {
                        value: age,
                        constraint: "MAXINCLUSIVE 150".into(),
                    }],
                    prints: vec![],
                },
                ShapeResult {
                    node: Literal::new_simple_literal("x").into(),
                    shape: ShapeLabel::Start,
                    status: Status::Conformant,
                    reason: None,
                    failures: vec![],
                    prints: vec!["\"hi\"".into()],
                },
            ],
            warnings: vec![
                "1 semantic action with extension <http://ex.org/js> was not run".into(),
            ],
            millis: 7,
            ..Default::default()
        }
    }

    #[test]
    fn json_report() {
        let j = to_json(&results());
        assert_eq!(j["conforms"], false);
        assert_eq!(j["counts"], json!({"conformant": 2, "nonconformant": 1}));
        assert_eq!(j["millis"], 7);
        assert_eq!(j["warnings"].as_array().unwrap().len(), 1);
        let rs = j["results"].as_array().unwrap();
        assert_eq!(rs.len(), 4);
        assert_eq!(
            rs[0],
            json!({"node": {"type": "uri", "value": "http://ex.org/alice"},
                "shape": {"type": "uri", "value": "http://ex.org/Person"},
                "status": "conformant"})
        );
        assert_eq!(rs[2]["status"], "nonconformant");
        assert_eq!(
            rs[2]["reason"],
            "ex:carol: foaf:age 200 fails MAXINCLUSIVE 150"
        );
        assert_eq!(
            rs[2]["appinfo"],
            json!({"failures": [{"kind": "facet", "constraint": "MAXINCLUSIVE 150",
                "value": {"type": "literal", "value": "200",
                    "datatype": "http://www.w3.org/2001/XMLSchema#integer"}}]})
        );
        assert_eq!(rs[3]["shape"], json!({"type": "start"}));
        assert_eq!(
            rs[3]["appinfo"],
            json!({"failures": [], "prints": ["\"hi\""]})
        );
    }

    #[test]
    fn shapemap_json() {
        let j = to_shapemap_json(&results());
        assert_eq!(
            j[0],
            json!({"node": "<http://ex.org/alice>", "shape": "<http://ex.org/Person>",
                "status": "conformant"})
        );
        assert_eq!(j[2]["status"], "nonconformant");
        assert_eq!(j[2]["appinfo"]["failures"][0]["kind"], "facet");
        assert_eq!(j[3]["node"], "\"x\"");
        assert_eq!(j[3]["shape"], "START");
    }

    #[test]
    fn compact_result_map() {
        assert_eq!(
            to_smap(&results()),
            "<http://ex.org/alice>@<http://ex.org/Person>\n\
             <http://ex.org/bob>@<http://ex.org/Person>\n\
             <http://ex.org/carol>@!<http://ex.org/Person>\n\
             \"x\"@START\n"
        );
    }

    #[test]
    fn text_report() {
        assert_eq!(
            to_text(&results()).lines().nth(2).unwrap(),
            "<http://ex.org/carol> @ <http://ex.org/Person> :: Focus = <http://ex.org/carol>, \
             Status = nonconformant, Reason = ex:carol: foaf:age 200 fails MAXINCLUSIVE 150"
        );
        assert_eq!(
            to_text(&results()).lines().next().unwrap(),
            "<http://ex.org/alice> @ <http://ex.org/Person> :: Focus = <http://ex.org/alice>, \
             Status = conformant"
        );
        let all_ok = ResultMap {
            conforms: true,
            ..Default::default()
        };
        assert_eq!(to_text(&all_ok), "OK\n");
    }

    #[test]
    fn compact_iris() {
        let p: PrefixMap = vec![
            ("ex".into(), "http://ex.org/".into()),
            ("sub".into(), "http://ex.org/sub/".into()),
        ];
        assert_eq!(compact_iri("http://ex.org/a", &p), "ex:a");
        assert_eq!(compact_iri("http://ex.org/sub/b", &p), "sub:b");
        assert_eq!(compact_iri("http://ex.org/a b", &p), "<http://ex.org/a b>");
        assert_eq!(compact_iri("http://ex.org/a.", &p), "<http://ex.org/a.>");
        assert_eq!(compact_iri("http://other.org/", &p), "<http://other.org/>");
    }
}
