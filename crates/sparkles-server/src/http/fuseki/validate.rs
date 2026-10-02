//! Fuseki's validators: `/$/validate/query`, `update`, `iri`, `data` and `langtag`.
//! Each takes its input in a query parameter or a form body and answers in JSON when
//! the `Accept` header prefers `application/json` to `text/html`, else in HTML, as
//! Fuseki does. Fuseki's language tag validator answers in HTML only; Sparkles' also
//! answers in JSON.
//!
//! JSON members follow Fuseki: `input`, `formatted`, `algebra`, `errors`
//! (`[{parse-error, parse-error-line, parse-error-column}]`), `iris`
//! (`[{iri, errors, warning}]`). Fuseki's query validator also gives the algebra in
//! quad form and optimized. Sparkles gives only `algebra`, the SPARQL algebra in SSE.

use super::super::{AdminBody, ApiResult, Params, err, negotiate};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value as J, json};

/// Fuseki's base IRI for parsing what is validated.
const BASE: &str = "http://example/base/";

struct Request {
    params: Params,
    json: bool,
}

fn request(uri: &Uri, headers: &HeaderMap, body: &[u8]) -> Request {
    let mut params = Params::from_query(uri);
    if super::super::content_type(headers) == "application/x-www-form-urlencoded" {
        params.extend_form(body);
    }
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("*/*");
    let json = negotiate(accept, &["text/html", "application/json"]) == Some(1);
    Request { params, json }
}

impl Request {
    /// The one value of a required parameter: `400` when it is missing or repeated.
    fn one(&self, name: &str) -> ApiResult<String> {
        match self.params.all(name).as_slice() {
            [] => Err(err(
                StatusCode::BAD_REQUEST,
                format!("No parameter given: {name}"),
            )),
            [v] => Ok(v.clone()),
            many => Err(err(
                StatusCode::BAD_REQUEST,
                format!("Too many ({}) parameter values: {name}", many.len()),
            )),
        }
    }

    fn syntax(&self) -> Option<String> {
        self.params
            .get("languageSyntax")
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
}

/// The `errors` array of a parse error with its position, if the message has one.
fn parse_errors(msg: &str, line: Option<u64>, column: Option<u64>) -> J {
    let mut e = serde_json::Map::new();
    e.insert("parse-error".into(), msg.into());
    if let Some(l) = line {
        e.insert("parse-error-line".into(), l.into());
    }
    if let Some(c) = column {
        e.insert("parse-error-column".into(), c.into());
    }
    json!([e])
}

/// A SPARQL syntax error as Fuseki's `errors` array.
fn sparql_errors(e: &sparkles::Error) -> J {
    let msg = e.to_string();
    let body = super::super::syntax_error_body(&msg);
    parse_errors(&msg, body["line"].as_u64(), body["column"].as_u64())
}

fn sparql_syntax(r: &Request) -> ApiResult<()> {
    match r.syntax().as_deref() {
        None | Some("SPARQL" | "SPARQL11" | "SPARQL_11" | "SPARQL12" | "SPARQL_12" | "ARQ") => {
            Ok(())
        }
        Some(s) => Err(err(StatusCode::BAD_REQUEST, format!("Unknown syntax: {s}"))),
    }
}

/// The formatted text of a query or update: the formatter's output, else the parser's
/// serialization.
#[cfg_attr(not(feature = "fmt"), allow(unused_variables))]
fn formatted(text: &str, parsed: String) -> String {
    #[cfg(feature = "fmt")]
    if let Ok(f) = sparkles_fmt::format(
        text,
        sparkles_fmt::Language::Sparql,
        &sparkles_fmt::Options::default(),
    ) {
        return f.text;
    }
    parsed
}

/// `/$/validate/query?query=…[&languageSyntax=SPARQL]`
pub(super) async fn query(uri: Uri, headers: HeaderMap, AdminBody(body): AdminBody) -> ApiResult {
    let r = request(&uri, &headers, &body);
    let q = r.one("query")?;
    sparql_syntax(&r)?;
    let doc = super::super::blocking(move || {
        let mut doc = json!({ "input": q });
        match sparkles::sparql::parse_query(&q, Some(BASE), &[]) {
            Ok(parsed) => {
                doc["formatted"] = formatted(&q, parsed.to_string()).into();
                doc["algebra"] = parsed.to_sse().into();
            }
            Err(e) => doc["errors"] = sparql_errors(&e),
        }
        Ok(doc)
    })
    .await?;
    Ok(respond(&r, "SPARQL Query Validator", doc))
}

/// `/$/validate/update?update=…[&languageSyntax=SPARQL]`
pub(super) async fn update(uri: Uri, headers: HeaderMap, AdminBody(body): AdminBody) -> ApiResult {
    let r = request(&uri, &headers, &body);
    let u = r.one("update")?;
    sparql_syntax(&r)?;
    let doc = super::super::blocking(move || {
        let mut doc = json!({ "input": u });
        let parser = sparkles::sparql::aggext::register(spargebra::SparqlParser::new())
            .with_base_iri(BASE)
            .expect("a valid base IRI");
        match parser.parse_update(&u) {
            Ok(parsed) => doc["formatted"] = formatted(&u, parsed.to_string()).into(),
            Err(e) => doc["errors"] = sparql_errors(&sparkles::Error::from(e)),
        }
        Ok(doc)
    })
    .await?;
    Ok(respond(&r, "SPARQL Update Validator", doc))
}

/// `/$/validate/iri?iri=…` (repeatable): errors for an invalid IRI, a warning for a
/// relative one, and the scheme and normalization warnings of `sparkles iri` (spec G05
/// §4.1).
pub(super) async fn iri(uri: Uri, headers: HeaderMap, AdminBody(body): AdminBody) -> ApiResult {
    let r = request(&uri, &headers, &body);
    let iris = r.params.all("iri");
    if iris.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "No IRIs supplied"));
    }
    let report: Vec<J> = iris
        .iter()
        .map(|s| {
            let (mut errors, mut warnings) = (Vec::<String>::new(), Vec::<String>::new());
            match oxiri::Iri::parse(s.as_str()) {
                Ok(_) => {}
                Err(e) => match oxiri::IriRef::parse(s.as_str()) {
                    Ok(_) => warnings.push(format!("Relative IRI: {s}")),
                    Err(_) => errors.push(format!("Bad IRI: {e}")),
                },
            }
            if errors.is_empty() {
                let mut issues = Vec::new();
                crate::tools::terms::iri_warnings(s, &mut issues);
                warnings.extend(issues.into_iter().map(|i| i.message));
            }
            json!({ "iri": s, "errors": errors, "warning": warnings })
        })
        .collect();
    Ok(respond(&r, "IRI Validator", json!({ "iris": report })))
}

/// `/$/validate/data?data=…[&languageSyntax=N-Quads]`: parses the data and reports its
/// first syntax error. Syntax names are Jena's (`Turtle`, `TTL`, `N-Triples`, `NT`,
/// `N-Quads`, `NQ`, `TriG`, `RDF/XML`, `JSON-LD`, `N3`, `RDF/JSON`).
pub(super) async fn data(uri: Uri, headers: HeaderMap, AdminBody(body): AdminBody) -> ApiResult {
    let r = request(&uri, &headers, &body);
    let syntax = r.syntax().unwrap_or_else(|| "N-Quads".into());
    let format = data_syntax(&syntax)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("Unknown syntax: {syntax}")))?;
    let data = r.one("data")?;
    let doc = super::super::blocking(move || {
        let mut doc = json!({ "input": data });
        if let Some(e) = parse_data(format, &data) {
            doc["errors"] = e;
        }
        Ok(doc)
    })
    .await?;
    Ok(respond(&r, "RDF Data Validator", doc))
}

enum DataSyntax {
    Rdf(oxrdfio::RdfFormat),
    RdfJson,
}

fn data_syntax(name: &str) -> Option<DataSyntax> {
    use oxrdfio::RdfFormat;
    let n = name.to_ascii_lowercase();
    Some(DataSyntax::Rdf(match n.as_str() {
        "turtle" | "ttl" => RdfFormat::Turtle,
        "n-triples" | "ntriples" | "n-triple" | "nt" => RdfFormat::NTriples,
        "n-quads" | "nquads" | "nq" => RdfFormat::NQuads,
        "trig" => RdfFormat::TriG,
        "rdf/xml" | "rdfxml" | "rdf" => RdfFormat::RdfXml,
        "json-ld" | "jsonld" => RdfFormat::JsonLd {
            profile: oxrdfio::JsonLdProfileSet::empty(),
        },
        "n3" => RdfFormat::N3,
        "rdf/json" | "rdfjson" | "rj" => return Some(DataSyntax::RdfJson),
        _ => return None,
    }))
}

/// The `errors` array of the data's first syntax error, if any.
fn parse_data(format: DataSyntax, data: &str) -> Option<J> {
    match format {
        DataSyntax::Rdf(f) => {
            let parser = oxrdfio::RdfParser::from_format(f)
                .with_base_iri(BASE)
                .ok()?;
            for q in parser.for_slice(data.as_bytes()) {
                if let Err(e) = q {
                    let at = e.location().map(|l| l.start);
                    return Some(parse_errors(
                        &e.to_string(),
                        at.map(|p| p.line + 1),
                        at.map(|p| p.column + 1),
                    ));
                }
            }
            None
        }
        DataSyntax::RdfJson => super::super::jena_formats::transcode(
            super::super::jena_formats::JenaFormat::RdfJson,
            data.as_bytes(),
            true,
            std::io::sink(),
        )
        .err()
        .map(|e| parse_errors(&e.to_string(), None, None)),
    }
}

/// `/$/validate/langtag?langtag=…` (also `lang=`, repeatable): BCP 47 well-formedness,
/// the canonical form, and the subtags.
pub(super) async fn langtag(uri: Uri, headers: HeaderMap, AdminBody(body): AdminBody) -> ApiResult {
    let r = request(&uri, &headers, &body);
    let mut tags = r.params.all("lang");
    tags.extend(r.params.all("langtag"));
    if tags.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "No ?lang= parameter"));
    }
    let report: Vec<J> = tags.iter().map(|t| langtag_report(t)).collect();
    Ok(respond(
        &r,
        "LangTag Validator",
        json!({ "langtags": report }),
    ))
}

fn langtag_report(t: &str) -> J {
    let mut o = serde_json::Map::new();
    o.insert("input".into(), t.into());
    let problem = if t.is_empty() {
        Some("Empty string for language tag".to_string())
    } else if t.chars().any(char::is_whitespace) {
        Some("Language tag contains white space".to_string())
    } else {
        oxilangtag::LanguageTag::parse(t.to_string())
            .err()
            .map(|e| format!("Invalid language tag: {e}"))
    };
    match problem {
        Some(p) => {
            o.insert("errors".into(), json!([p]));
        }
        None => {
            let tag = oxilangtag::LanguageTag::parse(t.to_string()).expect("checked");
            o.insert("errors".into(), json!([]));
            o.insert(
                "formatted".into(),
                crate::tools::terms::canonical_case(t)
                    .unwrap_or_else(|| t.to_string())
                    .into(),
            );
            o.insert("language".into(), tag.primary_language().into());
            let mut put = |k: &str, v: Option<&str>| {
                if let Some(v) = v.filter(|v| !v.is_empty()) {
                    o.insert(k.into(), v.into());
                }
            };
            put("script", tag.script());
            put("region", tag.region());
            put("variant", tag.variant());
            put("extension", tag.extension());
            put("privateuse", tag.private_use());
        }
    }
    J::Object(o)
}

/// The report as JSON, or as Fuseki's plain HTML page.
fn respond(r: &Request, title: &str, doc: J) -> Response {
    if r.json {
        return ([(header::VARY, "Accept")], axum::Json(doc)).into_response();
    }
    let pretty = serde_json::to_string_pretty(&doc).unwrap_or_default();
    let html = format!(
        "<!DOCTYPE html>\n<html>\n<head><meta charset=\"utf-8\"><title>{t}</title></head>\n\
         <body>\n<h1>{t}</h1>\n<pre>{body}</pre>\n</body>\n</html>\n",
        t = escape(title),
        body = escape(&pretty),
    );
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::VARY, "Accept"),
        ],
        html,
    )
        .into_response()
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_tags() {
        let canonical_case = |t: &str| crate::tools::terms::canonical_case(t).unwrap();
        assert_eq!(canonical_case("EN-us"), "en-US");
        assert_eq!(canonical_case("zh-hant-tw"), "zh-Hant-TW");
        assert_eq!(canonical_case("en-x-AB-CD"), "en-x-ab-cd");
        assert_eq!(langtag_report("en-US")["errors"], json!([]));
        assert_eq!(
            langtag_report("en US")["errors"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            langtag_report("a--b")["errors"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn data_errors_have_positions() {
        let e = parse_data(data_syntax("Turtle").unwrap(), "<a> <b> <c> .\n<a> <b> .").unwrap();
        assert_eq!(e[0]["parse-error-line"], 2);
        assert!(
            parse_data(
                data_syntax("N-Triples").unwrap(),
                "<http://a/> <http://b/> \"c\" ."
            )
            .is_none()
        );
        assert!(data_syntax("nonsense").is_none());
    }

    #[test]
    fn html_is_escaped() {
        assert_eq!(escape("<a href='x'>&"), "&lt;a href=&#39;x&#39;&gt;&amp;");
    }
}
