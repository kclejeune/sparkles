//! The JSON of form changes, as the HTTP routes and the bindings spell them: field
//! changes `{field, remove, add, list}`, expectations `{field: [term]}` and nested
//! entries `{node, parent, field, changes, expect}`, with terms in the SPARQL JSON
//! results form.

use super::*;
use oxrdf::BlankNode;

fn invalid(m: String) -> Error {
    Error::invalid(m)
}

/// A term of a change: an IRI, a literal or a triple term as the SPARQL JSON results
/// form spells it, or a blank node `{"type": "bnode", "value": label}`, which names a
/// stored nested child by the label the model gave it or a new child of the change.
pub fn term_from_json(t: &J, what: &str) -> Result<Term> {
    if t.get("type").and_then(J::as_str) == Some("bnode") {
        let label = t
            .get("value")
            .and_then(J::as_str)
            .ok_or_else(|| invalid(format!("{what}.value: a blank node label")))?;
        return BlankNode::new(label.trim_start_matches("_:"))
            .map(Term::BlankNode)
            .map_err(|e| invalid(format!("{what}: invalid blank node label: {e}")));
    }
    crate::overview::rdf_term_from_json(t, what)
}

fn terms(v: Option<&J>, what: &str) -> Result<Vec<Term>> {
    match v {
        None | Some(J::Null) => Ok(Vec::new()),
        Some(J::Array(a)) => a
            .iter()
            .enumerate()
            .map(|(i, t)| term_from_json(t, &format!("{what}[{i}]")))
            .collect(),
        Some(_) => Err(invalid(format!("{what}: an array of terms"))),
    }
}

/// Field changes from `[{field, remove?, add?, list?}]`. `list` is the members a list
/// field's `rdf:List` takes.
pub fn field_changes_from_json(v: Option<&J>, what: &str) -> Result<Vec<FieldChange>> {
    let Some(J::Array(a)) = v else {
        return Err(invalid(format!(
            "{what}: an array of {{field, remove, add}}"
        )));
    };
    let mut out = Vec::new();
    for (i, c) in a.iter().enumerate() {
        let o = c
            .as_object()
            .ok_or_else(|| invalid(format!("{what}[{i}]: an object of field, remove and add")))?;
        // other members are ignored, as they were before list changes
        let field = o
            .get("field")
            .and_then(J::as_str)
            .ok_or_else(|| invalid(format!("{what}[{i}].field: a field id")))?
            .to_string();
        let list = match o.get("list") {
            None | Some(J::Null) => None,
            v => Some(terms(v, &format!("{what}[{i}].list"))?),
        };
        out.push(FieldChange {
            field,
            remove: terms(o.get("remove"), &format!("{what}[{i}].remove"))?,
            add: terms(o.get("add"), &format!("{what}[{i}].add"))?,
            list,
        });
    }
    Ok(out)
}

/// Expectations from `{field id: [term]}`.
pub fn expect_from_json(v: Option<&J>, what: &str) -> Result<BTreeMap<String, Vec<Term>>> {
    let mut expect = BTreeMap::new();
    match v {
        None | Some(J::Null) => {}
        Some(J::Object(m)) => {
            for (k, v) in m {
                expect.insert(k.clone(), terms(Some(v), &format!("{what}[{k}]"))?);
            }
        }
        Some(_) => return Err(invalid(format!("{what}: an object of field ids to terms"))),
    }
    Ok(expect)
}

/// A blank node named in an entry: a blank node term, or its label as a string.
fn blank(v: &J, what: &str) -> Result<BlankNode> {
    let label = match v {
        J::String(s) => s.trim_start_matches("_:").to_string(),
        J::Object(_) => match term_from_json(v, what)? {
            Term::BlankNode(b) => return Ok(b),
            _ => return Err(invalid(format!("{what}: a blank node"))),
        },
        _ => return Err(invalid(format!("{what}: a blank node"))),
    };
    BlankNode::new(&label).map_err(|e| invalid(format!("{what}: invalid blank node label: {e}")))
}

/// Nested entries from `[{node, parent?, field, changes, expect?}]`.
pub fn nested_from_json(v: Option<&J>) -> Result<Vec<NodeChange>> {
    let a = match v {
        None | Some(J::Null) => return Ok(Vec::new()),
        Some(J::Array(a)) => a,
        Some(_) => {
            return Err(invalid(
                "nested: an array of {node, parent, field, changes, expect}".into(),
            ));
        }
    };
    let mut out = Vec::new();
    for (i, e) in a.iter().enumerate() {
        let what = format!("nested[{i}]");
        let o = e.as_object().ok_or_else(|| {
            invalid(format!(
                "{what}: an object of node, parent, field, changes and expect"
            ))
        })?;
        if let Some(k) = o.keys().find(|k| {
            !matches!(
                k.as_str(),
                "node" | "parent" | "field" | "changes" | "expect"
            )
        }) {
            return Err(invalid(format!(
                "{what}: unknown member '{k}': node, parent, field, changes and expect"
            )));
        }
        let node = blank(
            o.get("node")
                .ok_or_else(|| invalid(format!("{what}.node: a blank node")))?,
            &format!("{what}.node"),
        )?;
        let parent = match o.get("parent") {
            None | Some(J::Null) => None,
            Some(p) => Some(blank(p, &format!("{what}.parent"))?),
        };
        let field = o
            .get("field")
            .and_then(J::as_str)
            .ok_or_else(|| invalid(format!("{what}.field: the parent's field id")))?
            .to_string();
        let changes = match o.get("changes") {
            None | Some(J::Null) => Vec::new(),
            v => field_changes_from_json(v, &format!("{what}.changes"))?,
        };
        out.push(NodeChange {
            node,
            parent,
            field,
            changes,
            expect: expect_from_json(o.get("expect"), &format!("{what}.expect"))?,
        });
    }
    Ok(out)
}
