//! ShExJ: the JSON-LD syntax of ShEx 2.1, read and written by hand through
//! `serde_json::Value` (string-or-object value expressions, `max: -1`, literals as
//! objects and IRIs as strings, `shapes` with `id`s, 2.next `ShapeDecl` wrappers).

use crate::ParseError;
use crate::ast::*;
use crate::check::check_facets;
use crate::error::NOT_IMPLEMENTED;
use oxiri::Iri;
use serde_json::{Map, Value};

/// The JSON-LD context of ShExJ.
pub const CONTEXT: &str = "http://www.w3.org/ns/shex.jsonld";

/// Is `text` a ShExJ schema (a JSON object with `"type": "Schema"`)? Tells a schema
/// posted as `application/json` from a request envelope.
pub fn is_shexj(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .is_ok_and(|v| v.get("type").and_then(|t| t.as_str()) == Some("Schema"))
}

/// Parse ShExJ. A JSON syntax error has its line and column; an error in the structure
/// names the JSON path of the offending member (`shapes[2].expression.min: …`), at line
/// 1, column 1.
pub fn from_shexj(json: &str) -> Result<Schema, ParseError> {
    from_shexj_with_base(json, None)
}

/// Parse ShExJ, resolving relative IRIs against `base` (all but imports, which the
/// resolver finds relative to the importing schema).
pub fn from_shexj_with_base(json: &str, base: Option<&str>) -> Result<Schema, ParseError> {
    let v: Value = serde_json::from_str(json)
        .map_err(|e| ParseError::new(format!("invalid JSON: {e}"), e.line(), e.column()))?;
    let mut s = schema(&v).map_err(|(path, msg)| {
        let at = if path.is_empty() {
            String::new()
        } else {
            format!("{path}: ")
        };
        ParseError::new(format!("ShExJ: {at}{msg}"), 1, 1)
    })?;
    if let Some(b) = base {
        let base = Iri::parse(b.to_string())
            .map_err(|e| ParseError::new(format!("invalid base IRI <{b}>: {e}"), 1, 1))?;
        let mut r = Rebase {
            base: &base,
            err: None,
        };
        r.schema(&mut s);
        if let Some(e) = r.err {
            return Err(ParseError::new(format!("ShExJ: {e}"), 1, 1));
        }
        s.base = Some(b.to_string());
    }
    Ok(s)
}

/// Resolves the relative IRIs of a schema against a base.
struct Rebase<'b> {
    base: &'b Iri<String>,
    /// the first IRI that does not resolve
    err: Option<String>,
}

impl Rebase<'_> {
    fn iri(&mut self, iri: &mut String) {
        match self.base.resolve(iri) {
            Ok(r) => *iri = r.into_inner(),
            Err(e) => {
                self.err
                    .get_or_insert_with(|| format!("invalid IRI <{iri}>: {e}"));
            }
        }
    }

    fn label(&mut self, l: &mut Label) {
        if let Label::Iri(i) = l {
            self.iri(i);
        }
    }

    fn schema(&mut self, s: &mut Schema) {
        for a in &mut s.start_acts {
            self.iri(&mut a.name);
        }
        if let Some(e) = &mut s.start {
            self.shape_expr(e);
        }
        for d in &mut s.shapes {
            self.label(&mut d.label);
            self.shape_expr(&mut d.expr);
        }
    }

    fn shape_expr(&mut self, e: &mut ShapeExpr) {
        match e {
            ShapeExpr::Or(v) | ShapeExpr::And(v) => v.iter_mut().for_each(|x| self.shape_expr(x)),
            ShapeExpr::Not(x) => self.shape_expr(x),
            ShapeExpr::Ref(l) => self.label(l),
            ShapeExpr::External => {}
            ShapeExpr::Nc(nc) => {
                if let Some(dt) = &mut nc.datatype {
                    self.iri(dt);
                }
                for v in nc.values.iter_mut().flatten() {
                    self.value(v);
                }
            }
            ShapeExpr::Shape(sh) => {
                sh.extra.iter_mut().for_each(|p| self.iri(p));
                if let Some(t) = &mut sh.expression {
                    self.triple_expr(t);
                }
                self.acts_and_annotations(&mut sh.sem_acts, &mut sh.annotations);
            }
        }
    }

    fn value(&mut self, v: &mut ValueSetValue) {
        match v {
            ValueSetValue::Object(o) => self.object(o),
            ValueSetValue::IriStem(s) => self.iri(s),
            ValueSetValue::IriStemRange { stem, exclusions } => {
                if let Stem::Value(s) = stem {
                    self.iri(s);
                }
                for x in exclusions {
                    let (Exclusion::Value(i) | Exclusion::Stem(i)) = x;
                    self.iri(i);
                }
            }
            ValueSetValue::LiteralStem(_)
            | ValueSetValue::LiteralStemRange { .. }
            | ValueSetValue::Language(_)
            | ValueSetValue::LanguageStem(_)
            | ValueSetValue::LanguageStemRange { .. } => {}
        }
    }

    fn object(&mut self, o: &mut ObjectValue) {
        match o {
            ObjectValue::Iri(i) => self.iri(i),
            ObjectValue::Literal(l) => {
                if let Some(dt) = &mut l.datatype {
                    self.iri(dt);
                }
            }
        }
    }

    fn acts_and_annotations(&mut self, acts: &mut [SemAct], anns: &mut [Annotation]) {
        for a in acts {
            self.iri(&mut a.name);
        }
        for a in anns {
            self.iri(&mut a.predicate);
            self.object(&mut a.object);
        }
    }

    fn triple_expr(&mut self, t: &mut TripleExpr) {
        match t {
            TripleExpr::EachOf(g) | TripleExpr::OneOf(g) => {
                if let Some(id) = &mut g.id {
                    self.label(id);
                }
                g.exprs.iter_mut().for_each(|x| self.triple_expr(x));
                self.acts_and_annotations(&mut g.sem_acts, &mut g.annotations);
            }
            TripleExpr::Tc(tc) => {
                if let Some(id) = &mut tc.id {
                    self.label(id);
                }
                self.iri(&mut tc.predicate);
                if let Some(v) = &mut tc.value_expr {
                    self.shape_expr(v);
                }
                self.acts_and_annotations(&mut tc.sem_acts, &mut tc.annotations);
            }
            TripleExpr::Include(l) => self.label(l),
        }
    }
}

/// Write ShExJ.
pub fn to_shexj(schema: &Schema) -> Value {
    let _ = schema;
    unimplemented!("ShExJ writer: {NOT_IMPLEMENTED}")
}

// ------------------------------------------------------------------- reading ------

/// An error: the JSON path and the message.
type PathErr = (String, String);
type R<T> = Result<T, PathErr>;

fn fail<T>(path: &str, msg: impl Into<String>) -> R<T> {
    Err((path.to_string(), msg.into()))
}

fn member(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn object<'v>(v: &'v Value, path: &str) -> R<&'v Map<String, Value>> {
    v.as_object()
        .map_or_else(|| fail(path, "expected an object"), Ok)
}

fn string(v: &Value, path: &str) -> R<String> {
    v.as_str()
        .map_or_else(|| fail(path, "expected a string"), |s| Ok(s.to_string()))
}

fn array<'v>(v: &'v Value, path: &str) -> R<&'v [Value]> {
    v.as_array()
        .map_or_else(|| fail(path, "expected an array"), |a| Ok(a.as_slice()))
}

/// The members of an optional array, each converted.
fn list<T>(
    o: &Map<String, Value>,
    path: &str,
    key: &str,
    f: impl Fn(&Value, &str) -> R<T>,
) -> R<Vec<T>> {
    let Some(v) = o.get(key) else {
        return Ok(Vec::new());
    };
    let p = member(path, key);
    array(v, &p)?
        .iter()
        .enumerate()
        .map(|(i, x)| f(x, &format!("{p}[{i}]")))
        .collect()
}

fn opt_string(o: &Map<String, Value>, path: &str, key: &str) -> R<Option<String>> {
    o.get(key)
        .map(|v| string(v, &member(path, key)))
        .transpose()
}

fn opt_bool(o: &Map<String, Value>, path: &str, key: &str) -> R<Option<bool>> {
    o.get(key)
        .map(|v| {
            v.as_bool()
                .map_or_else(|| fail(&member(path, key), "expected true or false"), Ok)
        })
        .transpose()
}

fn opt_u64(o: &Map<String, Value>, path: &str, key: &str) -> R<Option<u64>> {
    o.get(key)
        .map(|v| {
            v.as_u64().map_or_else(
                || fail(&member(path, key), "expected a non-negative integer"),
                Ok,
            )
        })
        .transpose()
}

fn type_of<'v>(o: &'v Map<String, Value>, path: &str) -> R<&'v str> {
    match o.get("type") {
        Some(Value::String(t)) => Ok(t),
        Some(_) => fail(&member(path, "type"), "expected a string"),
        None => fail(path, "missing \"type\""),
    }
}

fn label(v: &Value, path: &str) -> R<Label> {
    Ok(Label::from_shexj(&string(v, path)?))
}

/// ShEx 2.2 members are errors that name the feature.
fn no_2_2(o: &Map<String, Value>, path: &str) -> R<()> {
    for key in ["abstract", "extends", "restricts"] {
        if let Some(v) = o.get(key)
            && *v != Value::Bool(false)
        {
            return fail(
                &member(path, key),
                format!("\"{key}\" is ShEx 2.2, which is not supported"),
            );
        }
    }
    Ok(())
}

fn schema(v: &Value) -> R<Schema> {
    let o = object(v, "")?;
    if type_of(o, "")? != "Schema" {
        return fail("type", "expected \"Schema\"");
    }
    Ok(Schema {
        base: None,
        prefixes: Vec::new(),
        imports: list(o, "", "imports", string)?,
        start: o.get("start").map(|s| shape_expr(s, "start")).transpose()?,
        start_acts: list(o, "", "startActs", sem_act)?,
        shapes: list(o, "", "shapes", shape_decl)?,
    })
}

fn shape_decl(v: &Value, path: &str) -> R<ShapeDecl> {
    let o = object(v, path)?;
    let Some(id) = o.get("id") else {
        return fail(path, "a shape declaration needs an \"id\"");
    };
    let label = label(id, &member(path, "id"))?;
    let expr = if type_of(o, path)? == "ShapeDecl" {
        no_2_2(o, path)?;
        let p = member(path, "shapeExpr");
        match o.get("shapeExpr") {
            Some(e) => shape_expr(e, &p)?,
            None => return fail(path, "missing \"shapeExpr\""),
        }
    } else {
        shape_expr_object(o, path)?
    };
    Ok(ShapeDecl { label, expr })
}

fn shape_expr(v: &Value, path: &str) -> R<ShapeExpr> {
    match v {
        Value::String(s) => Ok(ShapeExpr::Ref(Label::from_shexj(s))),
        Value::Object(o) => {
            if o.contains_key("id") {
                return fail(
                    path,
                    "a nested shape expression cannot have an \"id\"; declare it in \"shapes\"",
                );
            }
            shape_expr_object(o, path)
        }
        _ => fail(path, "expected a shape expression (a label or an object)"),
    }
}

fn shape_exprs(o: &Map<String, Value>, path: &str) -> R<Vec<ShapeExpr>> {
    let v = list(o, path, "shapeExprs", shape_expr)?;
    if v.len() < 2 {
        return fail(
            &member(path, "shapeExprs"),
            "expected at least two shape expressions",
        );
    }
    Ok(v)
}

fn shape_expr_object(o: &Map<String, Value>, path: &str) -> R<ShapeExpr> {
    Ok(match type_of(o, path)? {
        "ShapeOr" => ShapeExpr::Or(shape_exprs(o, path)?),
        "ShapeAnd" => ShapeExpr::And(shape_exprs(o, path)?),
        "ShapeNot" => {
            let p = member(path, "shapeExpr");
            match o.get("shapeExpr") {
                Some(e) => ShapeExpr::Not(Box::new(shape_expr(e, &p)?)),
                None => return fail(path, "missing \"shapeExpr\""),
            }
        }
        "NodeConstraint" => {
            let nc = node_constraint(o, path)?;
            check_facets(&nc).or_else(|m| fail(path, m))?;
            ShapeExpr::Nc(Box::new(nc))
        }
        "Shape" => ShapeExpr::Shape(Box::new(shape(o, path)?)),
        "ShapeExternal" => ShapeExpr::External,
        "ShapeDecl" => return fail(path, "a ShapeDecl only appears in \"shapes\""),
        t => {
            return fail(
                &member(path, "type"),
                format!("unknown shape expression type \"{t}\""),
            );
        }
    })
}

fn node_kind(v: &Value, path: &str) -> R<NodeKind> {
    Ok(match string(v, path)?.as_str() {
        "iri" => NodeKind::Iri,
        "bnode" => NodeKind::BNode,
        "nonliteral" => NodeKind::NonLiteral,
        "literal" => NodeKind::Literal,
        k => return fail(path, format!("unknown node kind \"{k}\"")),
    })
}

/// A numeric facet bound: a JSON number, kept with its lexical form.
fn numeric(v: &Value, path: &str) -> R<NumericLiteral> {
    let Value::Number(n) = v else {
        return fail(path, "expected a number");
    };
    let s = n.to_string();
    Ok(if n.is_i64() || n.is_u64() {
        NumericLiteral::Integer(s)
    } else if s.contains(['e', 'E']) {
        NumericLiteral::Double(s)
    } else {
        NumericLiteral::Decimal(s)
    })
}

fn node_constraint(o: &Map<String, Value>, path: &str) -> R<NodeConstraint> {
    let num = |key: &str| {
        o.get(key)
            .map(|v| numeric(v, &member(path, key)))
            .transpose()
    };
    Ok(NodeConstraint {
        node_kind: o
            .get("nodeKind")
            .map(|v| node_kind(v, &member(path, "nodeKind")))
            .transpose()?,
        datatype: opt_string(o, path, "datatype")?,
        length: opt_u64(o, path, "length")?,
        min_length: opt_u64(o, path, "minlength")?,
        max_length: opt_u64(o, path, "maxlength")?,
        pattern: opt_string(o, path, "pattern")?,
        flags: opt_string(o, path, "flags")?,
        min_inclusive: num("mininclusive")?,
        min_exclusive: num("minexclusive")?,
        max_inclusive: num("maxinclusive")?,
        max_exclusive: num("maxexclusive")?,
        total_digits: opt_u64(o, path, "totaldigits")?,
        fraction_digits: opt_u64(o, path, "fractiondigits")?,
        values: match o.get("values") {
            None => None,
            Some(_) => Some(list(o, path, "values", value_set_value)?),
        },
    })
}

fn object_literal(o: &Map<String, Value>, path: &str) -> R<ObjectLiteral> {
    let value = match o.get("value") {
        Some(v) => string(v, &member(path, "value"))?,
        None => return fail(path, "missing \"value\""),
    };
    let language = opt_string(o, path, "language")?;
    let datatype = opt_string(o, path, "type")?;
    if language.is_some() && datatype.is_some() {
        return fail(path, "a literal has a language or a datatype, not both");
    }
    Ok(ObjectLiteral {
        value,
        language,
        datatype,
    })
}

/// An IRI (a string) or a literal (an object with a `value`).
fn object_value(v: &Value, path: &str) -> R<ObjectValue> {
    match v {
        Value::String(s) => Ok(ObjectValue::Iri(s.clone())),
        Value::Object(o) => Ok(ObjectValue::Literal(object_literal(o, path)?)),
        _ => fail(path, "expected an IRI or a literal"),
    }
}

fn stem(o: &Map<String, Value>, path: &str) -> R<Stem> {
    let p = member(path, "stem");
    match o.get("stem") {
        Some(Value::String(s)) => Ok(Stem::Value(s.clone())),
        Some(Value::Object(w)) if type_of(w, &p)? == "Wildcard" => Ok(Stem::Wildcard),
        Some(_) => fail(&p, "expected a string or a Wildcard"),
        None => fail(path, "missing \"stem\""),
    }
}

fn stem_string(o: &Map<String, Value>, path: &str) -> R<String> {
    match o.get("stem") {
        Some(v) => string(v, &member(path, "stem")),
        None => fail(path, "missing \"stem\""),
    }
}

/// The exclusions of a stem range: values, or stems of type `stem_type`.
fn exclusions(o: &Map<String, Value>, path: &str, stem_type: &str) -> R<Vec<Exclusion>> {
    list(o, path, "exclusions", |v, p| match v {
        Value::String(s) => Ok(Exclusion::Value(s.clone())),
        Value::Object(x) => {
            // a literal exclusion may be written as an ObjectLiteral
            if x.contains_key("value") && !x.contains_key("type") {
                return Ok(Exclusion::Value(string(&x["value"], &member(p, "value"))?));
            }
            if type_of(x, p)? != stem_type {
                return fail(p, format!("expected a value or a {stem_type}"));
            }
            Ok(Exclusion::Stem(stem_string(x, p)?))
        }
        _ => fail(p, format!("expected a value or a {stem_type}")),
    })
}

fn value_set_value(v: &Value, path: &str) -> R<ValueSetValue> {
    let o = match v {
        Value::String(s) => return Ok(ValueSetValue::Object(ObjectValue::Iri(s.clone()))),
        Value::Object(o) => o,
        _ => return fail(path, "expected a value-set value"),
    };
    if o.contains_key("value") {
        return Ok(ValueSetValue::Object(ObjectValue::Literal(object_literal(
            o, path,
        )?)));
    }
    Ok(match type_of(o, path)? {
        "IriStem" => ValueSetValue::IriStem(stem_string(o, path)?),
        "LiteralStem" => ValueSetValue::LiteralStem(stem_string(o, path)?),
        "LanguageStem" => ValueSetValue::LanguageStem(stem_string(o, path)?),
        "Language" => match o.get("languageTag") {
            Some(t) => ValueSetValue::Language(string(t, &member(path, "languageTag"))?),
            None => return fail(path, "missing \"languageTag\""),
        },
        "IriStemRange" => ValueSetValue::IriStemRange {
            stem: stem(o, path)?,
            exclusions: exclusions(o, path, "IriStem")?,
        },
        "LiteralStemRange" => ValueSetValue::LiteralStemRange {
            stem: stem(o, path)?,
            exclusions: exclusions(o, path, "LiteralStem")?,
        },
        "LanguageStemRange" => ValueSetValue::LanguageStemRange {
            stem: stem(o, path)?,
            exclusions: exclusions(o, path, "LanguageStem")?,
        },
        t => {
            return fail(
                &member(path, "type"),
                format!("unknown value-set value type \"{t}\""),
            );
        }
    })
}

fn shape(o: &Map<String, Value>, path: &str) -> R<Shape> {
    no_2_2(o, path)?;
    Ok(Shape {
        closed: opt_bool(o, path, "closed")?,
        extra: list(o, path, "extra", string)?,
        expression: o
            .get("expression")
            .map(|e| triple_expr(e, &member(path, "expression")))
            .transpose()?,
        sem_acts: list(o, path, "semActs", sem_act)?,
        annotations: list(o, path, "annotations", annotation)?,
    })
}

/// `min` and `max` of a triple expression.
fn card(o: &Map<String, Value>, path: &str) -> R<(Option<u32>, Option<i64>)> {
    let min = match o.get("min") {
        None => None,
        Some(v) => match v.as_u64().and_then(|m| u32::try_from(m).ok()) {
            Some(m) => Some(m),
            None => return fail(&member(path, "min"), "expected a non-negative integer"),
        },
    };
    let max = match o.get("max") {
        None => None,
        Some(v) => match v.as_i64() {
            Some(m) if m >= -1 => Some(m),
            _ => {
                return fail(
                    &member(path, "max"),
                    "expected -1 or a non-negative integer",
                );
            }
        },
    };
    if let (Some(lo), Some(hi)) = (min, max)
        && hi >= 0
        && i64::from(lo) > hi
    {
        return fail(path, format!("min {lo} is greater than max {hi}"));
    }
    Ok((min, max))
}

fn opt_label(o: &Map<String, Value>, path: &str) -> R<Option<Label>> {
    o.get("id")
        .map(|v| label(v, &member(path, "id")))
        .transpose()
}

fn triple_expr(v: &Value, path: &str) -> R<TripleExpr> {
    let o = match v {
        Value::String(s) => return Ok(TripleExpr::Include(Label::from_shexj(s))),
        Value::Object(o) => o,
        _ => return fail(path, "expected a triple expression (a label or an object)"),
    };
    let (min, max) = card(o, path)?;
    let t = type_of(o, path)?;
    Ok(match t {
        "EachOf" | "OneOf" => {
            let exprs = list(o, path, "expressions", triple_expr)?;
            if exprs.len() < 2 {
                return fail(
                    &member(path, "expressions"),
                    "expected at least two triple expressions",
                );
            }
            let g = Group {
                id: opt_label(o, path)?,
                exprs,
                min,
                max,
                sem_acts: list(o, path, "semActs", sem_act)?,
                annotations: list(o, path, "annotations", annotation)?,
            };
            if t == "EachOf" {
                TripleExpr::EachOf(g)
            } else {
                TripleExpr::OneOf(g)
            }
        }
        "TripleConstraint" => TripleExpr::Tc(TripleConstraint {
            id: opt_label(o, path)?,
            inverse: opt_bool(o, path, "inverse")?,
            predicate: match o.get("predicate") {
                Some(p) => string(p, &member(path, "predicate"))?,
                None => return fail(path, "missing \"predicate\""),
            },
            value_expr: o
                .get("valueExpr")
                .map(|e| shape_expr(e, &member(path, "valueExpr")).map(Box::new))
                .transpose()?,
            min,
            max,
            sem_acts: list(o, path, "semActs", sem_act)?,
            annotations: list(o, path, "annotations", annotation)?,
        }),
        t => {
            return fail(
                &member(path, "type"),
                format!("unknown triple expression type \"{t}\""),
            );
        }
    })
}

fn sem_act(v: &Value, path: &str) -> R<SemAct> {
    let o = object(v, path)?;
    Ok(SemAct {
        name: match o.get("name") {
            Some(n) => string(n, &member(path, "name"))?,
            None => return fail(path, "missing \"name\""),
        },
        code: opt_string(o, path, "code")?,
    })
}

fn annotation(v: &Value, path: &str) -> R<Annotation> {
    let o = object(v, path)?;
    Ok(Annotation {
        predicate: match o.get("predicate") {
            Some(p) => string(p, &member(path, "predicate"))?,
            None => return fail(path, "missing \"predicate\""),
        },
        object: match o.get("object") {
            Some(x) => object_value(x, &member(path, "object"))?,
            None => return fail(path, "missing \"object\""),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_schemas() {
        assert!(is_shexj(
            r#"{"@context": "http://www.w3.org/ns/shex.jsonld", "type": "Schema"}"#
        ));
        assert!(!is_shexj(r#"{"schema": "<S> {}", "map": "<n>@<S>"}"#));
        assert!(!is_shexj("PREFIX ex: <http://ex.org/> ex:S {}"));
    }

    #[test]
    fn reads_a_schema() {
        let s = from_shexj(
            r#"{
              "@context": "http://www.w3.org/ns/shex.jsonld",
              "type": "Schema",
              "imports": ["http://ex.org/common"],
              "startActs": [{"type": "SemAct", "name": "http://ex.org/x"}],
              "start": "http://ex.org/S",
              "shapes": [
                {"id": "http://ex.org/S", "type": "Shape", "closed": true,
                 "extra": ["http://ex.org/b"],
                 "expression": {"type": "EachOf", "expressions": [
                   {"type": "TripleConstraint", "id": "_:e", "predicate": "http://ex.org/a",
                    "inverse": true, "min": 0, "max": -1,
                    "valueExpr": {"type": "NodeConstraint", "datatype":
                      "http://www.w3.org/2001/XMLSchema#integer", "mininclusive": 4.5,
                      "maxexclusive": 10}},
                   "_:f"
                 ], "semActs": [{"type": "SemAct", "name": "http://shex.io/extensions/Test/",
                   "code": " print(o) "}]},
                 "annotations": [{"type": "Annotation", "predicate": "http://ex.org/note",
                   "object": {"value": "hi", "language": "en"}}]},
                {"type": "ShapeDecl", "id": "http://ex.org/T", "shapeExpr":
                  {"type": "ShapeOr", "shapeExprs": ["http://ex.org/S",
                    {"type": "NodeConstraint", "values": [
                      "http://ex.org/v", {"value": "1", "type": "http://www.w3.org/2001/XMLSchema#integer"},
                      {"type": "IriStemRange", "stem": {"type": "Wildcard"},
                       "exclusions": ["http://ex.org/x", {"type": "IriStem", "stem": "http://ex.org/y"}]},
                      {"type": "Language", "languageTag": "en"},
                      {"type": "LanguageStem", "stem": ""}
                    ]}]}},
                {"id": "http://ex.org/E", "type": "ShapeExternal"}
              ]
            }"#,
        )
        .unwrap();
        assert_eq!(s.imports, ["http://ex.org/common"]);
        assert_eq!(
            s.start,
            Some(ShapeExpr::Ref(Label::Iri("http://ex.org/S".into())))
        );
        assert_eq!(s.start_acts[0].code, None);
        assert_eq!(s.shapes.len(), 3);
        let ShapeExpr::Shape(sh) = &s.shapes[0].expr else {
            panic!()
        };
        assert_eq!(sh.closed, Some(true));
        assert_eq!(
            sh.annotations[0].object,
            ObjectValue::Literal(ObjectLiteral {
                value: "hi".into(),
                language: Some("en".into()),
                datatype: None,
            })
        );
        let Some(TripleExpr::EachOf(g)) = &sh.expression else {
            panic!()
        };
        assert_eq!(g.sem_acts[0].code.as_deref(), Some(" print(o) "));
        assert_eq!(g.exprs[1], TripleExpr::Include(Label::BNode("f".into())));
        let TripleExpr::Tc(tc) = &g.exprs[0] else {
            panic!()
        };
        assert_eq!(
            (tc.id.clone(), tc.inverse, tc.min, tc.max),
            (
                Some(Label::BNode("e".into())),
                Some(true),
                Some(0),
                Some(-1)
            )
        );
        let Some(ShapeExpr::Nc(nc)) = tc.value_expr.as_deref() else {
            panic!()
        };
        assert_eq!(
            nc.min_inclusive,
            Some(NumericLiteral::Decimal("4.5".into()))
        );
        assert_eq!(nc.max_exclusive, Some(NumericLiteral::Integer("10".into())));
        let ShapeExpr::Or(v) = &s.shapes[1].expr else {
            panic!()
        };
        let ShapeExpr::Nc(nc) = &v[1] else { panic!() };
        let vals = nc.values.as_ref().unwrap();
        assert_eq!(vals.len(), 5);
        assert_eq!(
            vals[2],
            ValueSetValue::IriStemRange {
                stem: Stem::Wildcard,
                exclusions: vec![
                    Exclusion::Value("http://ex.org/x".into()),
                    Exclusion::Stem("http://ex.org/y".into())
                ]
            }
        );
        assert_eq!(vals[4], ValueSetValue::LanguageStem(String::new()));
        assert_eq!(s.shapes[2].expr, ShapeExpr::External);
    }

    #[test]
    fn errors() {
        let e = from_shexj("{\n \"type\": ").unwrap_err();
        assert_eq!(e.line, 2);
        let e = from_shexj(r#"{"type": "Schema", "shapes": [{"id": "http://ex.org/S", "type": "Shape",
            "expression": {"type": "TripleConstraint", "predicate": "http://ex.org/p", "min": -2}}]}"#)
            .unwrap_err();
        assert_eq!(
            e.message,
            "ShExJ: shapes[0].expression.min: expected a non-negative integer"
        );
        let e = from_shexj(
            r#"{"type": "Schema", "shapes": [{"id": "http://ex.org/S",
            "type": "NodeConstraint", "datatype": "http://ex.org/dt", "mininclusive": 1}]}"#,
        )
        .unwrap_err();
        assert!(
            e.message.contains("not an XSD numeric datatype"),
            "{}",
            e.message
        );
        let e = from_shexj(
            r#"{"type": "Schema", "shapes": [{"type": "ShapeDecl",
            "id": "http://ex.org/S", "abstract": true, "shapeExpr": {"type": "Shape"}}]}"#,
        )
        .unwrap_err();
        assert!(e.message.contains("ShEx 2.2"), "{}", e.message);
        assert!(from_shexj(r#"{"type": "Shape"}"#).is_err());
    }

    #[test]
    fn relative_iris() {
        let json = r#"{"type": "Schema", "imports": ["common"], "shapes": [{"id": "S1",
            "type": "Shape", "expression": {"type": "TripleConstraint", "predicate": "p1",
            "valueExpr": {"type": "NodeConstraint", "values": ["o1", {"value": "x",
            "type": "dt"}]}}}, {"id": "_:b", "type": "ShapeAnd", "shapeExprs": ["S1",
            "http://ex.org/T"]}, {"id": "http://ex.org/T", "type": "Shape"}]}"#;
        let s = from_shexj_with_base(json, Some("http://ex.org/dir/s.json")).unwrap();
        let s2 = crate::shexc::parser::parse(
            "IMPORT <common> <S1> { <p1> [<o1> \"x\"^^<dt>] } _:b @<S1> AND @<http://ex.org/T> \
             <http://ex.org/T> {}",
            Some("http://ex.org/dir/s.json"),
        )
        .unwrap();
        assert_eq!(s.shapes, s2.shapes);
        assert_eq!(s.imports, ["common"]);
        assert_eq!(s.base.as_deref(), Some("http://ex.org/dir/s.json"));
        // without a base they are kept as written
        let s = from_shexj(json).unwrap();
        assert_eq!(s.shapes[0].label, Label::Iri("S1".into()));
        assert!(from_shexj_with_base(json, Some("not a base")).is_err());
    }
}
