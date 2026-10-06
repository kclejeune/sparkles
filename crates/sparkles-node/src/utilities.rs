//! Standalone engine helpers run on owned inputs outside the JavaScript thread.
use super::*;

fn encode<T: serde::Serialize>(value: T) -> sparkles::Result<Value> {
    serde_json::to_value(value).map_err(|e| EngineError::invalid(e.to_string()))
}
fn text<'a>(args: &'a Value, key: &str) -> sparkles::Result<&'a str> {
    args[key]
        .as_str()
        .ok_or_else(|| EngineError::invalid(format!("{key} must be a string")))
}
fn language(args: &Value) -> sparkles::Result<sparkles_fmt::Language> {
    sparkles_fmt::Language::from_name(text(args, "language")?)
        .ok_or_else(|| EngineError::invalid("unknown formatter language"))
}
fn deadline(args: &Value) -> sparkles::Result<Option<Instant>> {
    if args["timeout"].is_null() {
        return Ok(None);
    }
    let ms = args["timeout"]
        .as_u64()
        .ok_or_else(|| EngineError::invalid("timeout must be a nonnegative integer"))?;
    Instant::now()
        .checked_add(Duration::from_millis(ms))
        .map(Some)
        .ok_or_else(|| EngineError::invalid("timeout is too large"))
}
fn formatter_options(args: &Value) -> sparkles::Result<sparkles_fmt::Options> {
    use sparkles_fmt::options::{Value as OptionValue, kebab, set};
    let mut opts = sparkles_fmt::Options {
        deadline: deadline(args)?,
        ..Default::default()
    };
    if !args["options"].is_null() && !args["options"].is_object() {
        return Err(EngineError::invalid("formatter options must be an object"));
    }
    if let Some(options) = args["options"].as_object() {
        for (key, value) in options {
            match key.as_str() {
                "canonicalize" => {
                    opts.canonicalize = value
                        .as_bool()
                        .ok_or_else(|| EngineError::invalid("canonicalize must be a boolean"))?;
                    continue;
                }
                "cursor" => {
                    opts.cursor = Some(
                        value
                            .as_u64()
                            .and_then(|v| usize::try_from(v).ok())
                            .ok_or_else(|| {
                                EngineError::invalid("cursor must be an unsigned integer")
                            })?,
                    );
                    continue;
                }
                _ => {}
            }
            let key = kebab(key)
                .ok_or_else(|| EngineError::invalid(format!("unknown formatter option {key}")))?;
            let value =
                match value {
                    Value::Bool(v) => OptionValue::Bool(*v),
                    Value::String(v) => OptionValue::Str(v.clone()),
                    Value::Number(v) => OptionValue::Int(v.as_i64().ok_or_else(|| {
                        EngineError::invalid("formatter option must be an integer")
                    })?),
                    Value::Array(_) => OptionValue::Groups(
                        serde_json::from_value(value.clone())
                            .map_err(|e| EngineError::invalid(e.to_string()))?,
                    ),
                    _ => return Err(EngineError::invalid("invalid formatter option value")),
                };
            set(&mut opts, key, value).map_err(|e| EngineError::invalid(e.to_string()))?;
        }
    }
    Ok(opts)
}
#[napi]
pub async fn utility(op: String, args: String) -> napi::Result<String> {
    let args = parse(&args)?;
    blocking(move || {
        let value = match op.as_str() {
            "checkIri" => encode(sparkles::terms::check_iri(text(&args, "text")?))?,
            "checkLangtag" => encode(sparkles::terms::check_langtag(text(&args, "text")?))?,
            "checkData" => {
                let syntax = sparkles::io::DataSyntax::from_name(text(&args, "format")?).ok_or_else(|| EngineError::invalid("unknown RDF format"))?;
                let base = args["baseIri"].as_str();
                if let Some(base) = base { oxrdf::NamedNode::new(base).map_err(|e| EngineError::invalid(e.to_string()))?; }
                let issue = sparkles::io::check_data(syntax, text(&args, "text")?, base);
                // Positions are engine uint64 metadata, not user JSON.
                lossless(encode(issue)?)
            }
            "parseQuery" | "parseUpdate" => {
                let prefixes: std::collections::BTreeMap<String, String> = serde_json::from_value(if args["prefixes"].is_null() { json!({}) } else { args["prefixes"].clone() }).map_err(|e| EngineError::invalid(e.to_string()))?;
                let prefixes = prefixes.into_iter().collect::<Vec<_>>();
                let text = text(&args, "text")?;
                let base = args["baseIri"].as_str();
                let parsed = if op == "parseQuery" {
                    sparkles::sparql::parse_query(text, base, &prefixes)?.to_string()
                } else {
                    sparkles::sparql::update::parse_update(text, &QueryOptions { base_iri: base.map(String::from), prefixes, ..Default::default() })?.to_string()
                };
                Value::String(parsed)
            }
            "format" => {
                let lang = language(&args)?;
                let output = sparkles_fmt::format(text(&args, "text")?, lang, &formatter_options(&args)?).map_err(|e| match e {
                    sparkles_fmt::FormatError::Timeout => EngineError::Timeout,
                    sparkles_fmt::FormatError::UnsupportedLanguage { .. } => EngineError::unsupported(e.to_string()),
                    _ => EngineError::invalid(e.to_string()),
                })?;
                json!({"text":output.text,"changed":output.changed,"cursor":output.cursor,"language":output.language.name(),"warnings":output.warnings.into_iter().map(|w| json!({"code":w.code,"message":w.message})).collect::<Vec<_>>()})
            }
            "lint" => {
                let mut opts = sparkles_fmt::lint::LintOptions { deadline: deadline(&args)?, ..Default::default() };
                if let Some(levels) = args["levels"].as_object() { for (key, value) in levels { opts.set(key, value.as_str().ok_or_else(|| EngineError::invalid("lint severity must be a string"))?).map_err(EngineError::invalid)?; } }
                let output = sparkles_fmt::lint::lint(text(&args, "text")?, language(&args)?, &opts).map_err(|e| match e {
                    sparkles_fmt::lint::LintError::Timeout => EngineError::Timeout,
                    sparkles_fmt::lint::LintError::UnsupportedLanguage(_) => EngineError::unsupported(e.to_string()),
                    _ => EngineError::invalid(e.to_string()),
                })?;
                json!({"language":output.language.name(),"diagnostics":output.diagnostics.into_iter().map(|d| json!({"rule":d.rule,"severity":d.severity.name(),"message":d.message,"start":d.start,"end":d.end,"line":d.line,"column":d.column,"endLine":d.end_line,"endColumn":d.end_column,"fix":d.fix.map(|f| json!({"title":f.title,"edits":f.edits.into_iter().map(|e| json!({"start":e.start,"end":e.end,"insert":e.insert})).collect::<Vec<_>>()}))})).collect::<Vec<_>>()})
            }
            #[cfg(feature = "geo")]
            "convertGeometries" => {
                let items: Vec<sparkles::geo::convert::ConvertItem> = serde_json::from_value(args["items"].clone()).map_err(|e| EngineError::invalid(e.to_string()))?;
                encode(sparkles::geo::convert::convert(&items)?)?
            }
            #[cfg(feature = "backup")]
            "previewSchedule" => {
                use sparkles::backup::policy::{next_runs, parse_schedule, parse_timezone};
                let schedule = parse_schedule(text(&args, "schedule")?).map_err(sparkles::backup::error)?;
                let tz = parse_timezone(args["timezone"].as_str().unwrap_or("UTC")).map_err(sparkles::backup::error)?;
                let now = args["after"].as_str().map(String::from).unwrap_or_else(sparkles::backup::now_rfc3339);
                let after = now.parse().map_err(|e| EngineError::invalid(format!("invalid after time: {e}")))?;
                let n = args["count"].as_u64().unwrap_or(5);
                if n > 1000 { return Err(EngineError::invalid("count must be at most 1000")); }
                encode(next_runs(&schedule, tz, after, n as usize).into_iter().map(|t| t.to_rfc3339()).collect::<Vec<_>>())?
            }
            _ => return Err(EngineError::unsupported("unknown or disabled helper")),
        };
        Ok(value.to_string())
    }).await
}
