//! Additional facade helpers; complex documents cross the boundary once as JSON.
use crate::documents::encode;
use crate::{ErrorKind, FfiDataset, FfiError, FfiOperation, FfiResult};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
fn bad(e: impl ToString) -> FfiError {
    FfiError::new(ErrorKind::Invalid, e.to_string())
}
#[allow(dead_code)]
fn unsupported(name: &str) -> FfiError {
    FfiError::new(ErrorKind::Unsupported, format!("built without {name}"))
}
#[uniffi::export]
pub fn check_iri(iri: String) -> FfiResult<Vec<u8>> {
    let c = sparkles::terms::check_iri(&iri);
    encode(&json!({"errors":c.errors,"warnings":c.warnings}))
}
#[uniffi::export]
pub fn check_langtag(tag: String) -> FfiResult<Vec<u8>> {
    let c = sparkles::terms::check_langtag(&tag);
    encode(
        &json!({"error":c.error,"canonical":c.canonical,"language":c.language,"script":c.script,"region":c.region,"variant":c.variant,"extension":c.extension,"privateUse":c.private_use}),
    )
}
#[uniffi::export]
pub fn check_data(text: String, format: String, base_iri: Option<String>) -> FfiResult<Vec<u8>> {
    let syntax =
        sparkles::io::DataSyntax::from_name(&format).ok_or_else(|| bad("unknown RDF format"))?;
    if let Some(base) = &base_iri {
        oxrdf::NamedNode::new(base).map_err(bad)?;
    }
    encode(&sparkles::io::check_data(
        syntax,
        &text,
        base_iri.as_deref(),
    ))
}
#[uniffi::export]
pub fn convert_geometries(items: Vec<u8>) -> FfiResult<Vec<u8>> {
    #[cfg(feature = "geo")]
    {
        encode(&sparkles::geo::convert::convert(
            &crate::documents::decode::<Vec<sparkles::geo::convert::ConvertItem>>(&items)?,
        )?)
    }
    #[cfg(not(feature = "geo"))]
    {
        let _ = items;
        Err(unsupported("geo"))
    }
}
#[uniffi::export]
pub fn preview_schedule(
    schedule: String,
    timezone: String,
    count: u32,
    after: Option<String>,
) -> FfiResult<Vec<String>> {
    if count > 1000 {
        return Err(bad("count must be at most 1000"));
    }
    #[cfg(feature = "backup")]
    {
        use sparkles::backup::policy::{next_runs, parse_schedule, parse_timezone};
        let schedule = parse_schedule(&schedule).map_err(bad)?;
        let tz = parse_timezone(&timezone).map_err(bad)?;
        let after = after
            .unwrap_or_else(sparkles::backup::now_rfc3339)
            .parse()
            .map_err(bad)?;
        Ok(next_runs(&schedule, tz, after, count as usize)
            .into_iter()
            .map(|t| t.to_rfc3339())
            .collect())
    }
    #[cfg(not(feature = "backup"))]
    {
        let _ = (schedule, timezone, after);
        Err(unsupported("backup"))
    }
}
#[uniffi::export]
pub fn format_document(
    text: String,
    language: String,
    options: Vec<u8>,
    operation: Arc<FfiOperation>,
) -> FfiResult<Vec<u8>> {
    operation.control.check()?;
    #[cfg(feature = "fmt")]
    {
        let lang =
            sparkles_fmt::Language::from_name(&language).ok_or_else(|| bad("unknown language"))?;
        let mut opts = sparkles_fmt::Options {
            deadline: operation.control.deadline,
            ..Default::default()
        };
        let values: serde_json::Map<String, serde_json::Value> =
            crate::documents::decode(&options)?;
        for (key, v) in values {
            use sparkles_fmt::options::Value;
            if key == "canonicalize" {
                opts.canonicalize = v
                    .as_bool()
                    .ok_or_else(|| bad("canonicalize must be boolean"))?;
                continue;
            }
            if key == "cursor" {
                opts.cursor = if v.is_null() {
                    None
                } else {
                    Some(
                        usize::try_from(
                            v.as_u64()
                                .ok_or_else(|| bad("cursor must be nonnegative integer"))?,
                        )
                        .map_err(bad)?,
                    )
                };
                continue;
            }
            let name = sparkles_fmt::options::kebab(&key)
                .ok_or_else(|| bad(format!("unknown format option {key}")))?;
            let value = match v {
                serde_json::Value::Bool(b) => Value::Bool(b),
                serde_json::Value::String(s) => Value::Str(s),
                serde_json::Value::Number(n) => Value::Int(
                    n.as_i64()
                        .ok_or_else(|| bad("format integer out of range"))?,
                ),
                serde_json::Value::Array(a) => {
                    Value::Groups(serde_json::from_value(serde_json::Value::Array(a)).map_err(bad)?)
                }
                _ => return Err(bad("invalid format option type")),
            };
            sparkles_fmt::options::set(&mut opts, name, value).map_err(bad)?;
        }
        let r = sparkles_fmt::format(&text, lang, &opts).map_err(|e| {
            FfiError::new(
                match e {
                    sparkles_fmt::FormatError::Timeout => ErrorKind::Timeout,
                    sparkles_fmt::FormatError::UnsupportedLanguage { .. }
                    | sparkles_fmt::FormatError::Unsupported { .. } => ErrorKind::Unsupported,
                    sparkles_fmt::FormatError::Syntax { .. }
                        if lang == sparkles_fmt::Language::Sparql =>
                    {
                        ErrorKind::SparqlSyntax
                    }
                    sparkles_fmt::FormatError::Syntax { .. } => ErrorKind::RdfParse,
                    _ => ErrorKind::Invalid,
                },
                e.to_string(),
            )
        })?;
        operation.control.check()?;
        encode(
            &json!({"text":r.text,"changed":r.changed,"cursor":r.cursor,"language":r.language.name(),"warnings":r.warnings.iter().map(|w|json!({"code":w.code,"message":w.message,"line":w.line,"column":w.column})).collect::<Vec<_>>()}),
        )
    }
    #[cfg(not(feature = "fmt"))]
    {
        let _ = (text, language, options);
        Err(unsupported("fmt"))
    }
}
#[uniffi::export]
pub fn lint_document(
    text: String,
    language: String,
    levels: HashMap<String, String>,
    operation: Arc<FfiOperation>,
) -> FfiResult<Vec<u8>> {
    operation.control.check()?;
    #[cfg(feature = "fmt")]
    {
        let lang =
            sparkles_fmt::Language::from_name(&language).ok_or_else(|| bad("unknown language"))?;
        let mut opts = sparkles_fmt::lint::LintOptions {
            deadline: operation.control.deadline,
            ..Default::default()
        };
        for (rule, level) in levels {
            opts.set(&rule, &level).map_err(bad)?;
        }
        let r = sparkles_fmt::lint::lint(&text, lang, &opts).map_err(|e| {
            FfiError::new(
                match e {
                    sparkles_fmt::lint::LintError::Timeout => ErrorKind::Timeout,
                    sparkles_fmt::lint::LintError::UnsupportedLanguage(_) => ErrorKind::Unsupported,
                    _ => ErrorKind::Invalid,
                },
                e.to_string(),
            )
        })?;
        operation.control.check()?;
        encode(
            &json!({"language":r.language.name(),"diagnostics":r.diagnostics.iter().map(|d|json!({"rule":d.rule,"severity":d.severity.name(),"message":d.message,"start":d.start,"end":d.end,"line":d.line,"column":d.column,"endLine":d.end_line,"endColumn":d.end_column,"fix":d.fix.as_ref().map(|f|json!({"title":f.title,"edits":f.edits.iter().map(|e|json!({"start":e.start,"end":e.end,"insert":e.insert})).collect::<Vec<_>>() }))})).collect::<Vec<_>>()}),
        )
    }
    #[cfg(not(feature = "fmt"))]
    {
        let _ = (text, language, levels);
        Err(unsupported("fmt"))
    }
}
#[derive(uniffi::Record)]
pub struct TextSearchRequest {
    pub query: String,
    pub predicates: Vec<String>,
    pub lang: Option<String>,
    pub graph: Option<String>,
    pub limit: u32,
    pub highlight: bool,
}
#[derive(uniffi::Record)]
pub struct GeoFeaturesRequest {
    pub bbox: Vec<f64>,
    pub graph: Option<String>,
    pub predicate: Option<String>,
    pub limit: u32,
    pub tolerance: Option<f64>,
}
#[derive(uniffi::Record)]
pub struct RecallRequest {
    pub samples: u32,
    pub k: u32,
    pub ef: Option<u32>,
}
#[derive(uniffi::Record)]
pub struct DiagnosticsRequest {
    pub checks: Vec<String>,
    pub limit: u32,
    pub inferences: bool,
    pub graphs: Vec<String>,
    pub closure: String,
}
#[derive(uniffi::Record)]
pub struct EmbeddingEnvironment {
    pub enabled: bool,
    pub allow_private: bool,
    pub secrets: HashMap<String, String>,
}
#[uniffi::export]
impl FfiDataset {
    pub fn clear_cache(&self) {
        self.inner.ds.clear_cache();
    }
    pub fn clone_to_memory(&self, operation: Arc<FfiOperation>) -> FfiResult<Arc<FfiDataset>> {
        let c =
            self.inner
                .ds
                .clone_to_memory_with("jvm", &Default::default(), &operation.control)?;
        let ds = sparkles::Dataset::from_store_with(
            c.store,
            sparkles::DatasetOptions {
                origin: Some(c.origin),
                ..Default::default()
            },
        );
        ds.state().set_reasoning(c.reasoning)?;
        Ok(FfiDataset::new(ds, self.inner.opts.clone()))
    }
    pub fn explain(&self, query: String, operation: Arc<FfiOperation>) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let opts = sparkles::sparql::QueryOptions {
            cancel: Some(operation.control.cancel.flag()),
            timeout: operation
                .control
                .deadline
                .map(|d| d.saturating_duration_since(Instant::now())),
            ..Default::default()
        };
        let (text, plan) = self.inner.ds.explain(&query, &opts)?;
        operation.control.check()?;
        encode(&json!({"text":text,"plan":plan}))
    }
    pub fn commit_graph(
        &self,
        branches: Option<Vec<String>>,
        before: Option<String>,
        limit: u32,
    ) -> FfiResult<Vec<u8>> {
        let r = self
            .inner
            .ds
            .commit_graph(&sparkles::store::CommitGraphOptions {
                branches,
                before: before.map(|b| b.parse()).transpose()?,
                limit: limit as usize,
            })?;
        let branches=r.branches.iter().map(|b|json!({"name":b.name,"id":b.id,"ordinal":b.ordinal,"head":b.head,"from":b.from,"upstream":b.upstream,"created":sparkles::commit::rfc3339_ms(b.created_ms)})).collect::<Vec<_>>();
        let commits = r
            .commits
            .iter()
            .map(|c| {
                let mut j = json!(sparkles::commit::AnnotatedCommit {
                    commit: &c.commit,
                    annotation: c.annotation.as_ref()
                });
                j["branch"] = json!(c.branch);
                j["branchId"] = json!(c.branch_id);
                j["parents"] = json!(c.parents);
                j["mergedFrom"] = json!(c.merged_from);
                j["replayedFrom"] = json!(c.replayed_from);
                j
            })
            .collect::<Vec<_>>();
        encode(&json!({"branches":branches,"commits":commits,"next":r.next.map(|c|c.to_string())}))
    }
    pub fn text_search(
        &self,
        req: TextSearchRequest,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let req = sparkles::handles::TextSearch {
            query: req.query,
            predicates: req
                .predicates
                .into_iter()
                .map(oxrdf::NamedNode::new)
                .collect::<Result<Vec<_>, _>>()
                .map_err(bad)?,
            lang: req.lang,
            graph: req
                .graph
                .map(oxrdf::NamedNode::new)
                .transpose()
                .map_err(bad)?,
            limit: req.limit as usize,
            highlight: req.highlight,
            options: sparkles::sparql::QueryOptions {
                cancel: Some(operation.control.cancel.flag()),
                timeout: operation
                    .control
                    .deadline
                    .map(|d| d.saturating_duration_since(Instant::now())),
                ..Default::default()
            },
        };
        let r = self.inner.ds.indexes().text().search(&req)?;
        operation.control.check()?;
        encode(&r)
    }
    pub fn geo_features(&self, req: GeoFeaturesRequest) -> FfiResult<Vec<u8>> {
        #[cfg(feature = "geo")]
        {
            let bbox = req
                .bbox
                .try_into()
                .map_err(|_| bad("bbox must contain four coordinates"))?;
            encode(&self.inner.ds.indexes().geo().features(
                &sparkles::geo::map::BoxQuery {
                    bbox,
                    graph: req.graph,
                    predicate: req.predicate,
                    limit: req.limit as usize,
                    tolerance: req.tolerance,
                },
                None,
            )?)
        }
        #[cfg(not(feature = "geo"))]
        {
            let _ = req;
            Err(unsupported("geo"))
        }
    }
    pub fn vector_recall(
        &self,
        name: String,
        req: RecallRequest,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let r = self.inner.ds.indexes().vector().recall(
            &name,
            &sparkles::handles::RecallOptions {
                samples: req.samples as usize,
                k: req.k as usize,
                ef: req.ef.map(|e| e as usize),
            },
        )?;
        operation.control.check()?;
        encode(&r)
    }
    pub fn vector_reembed(&self, name: String) -> FfiResult<()> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.indexes().vector().reembed(&name)?)
    }
    pub fn vector_embedding_status(&self, name: String) -> FfiResult<Option<Vec<u8>>> {
        self.inner
            .ds
            .indexes()
            .vector()
            .embedding_status(&name)
            .as_ref()
            .map(encode)
            .transpose()
    }
    pub fn vector_embedding_environment(&self, env: Option<EmbeddingEnvironment>) -> FfiResult<()> {
        self.inner.check_writable()?;
        let env = env
            .map(|e| -> FfiResult<_> {
                let mut environment = sparkles::vector::embed::Environment {
                    enabled: e.enabled,
                    outbound: sparkles::outbound::OutboundPolicy {
                        allow_private: e.allow_private,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                for (k, v) in e.secrets {
                    environment.secrets.insert(k, v.parse().map_err(bad)?);
                }
                Ok(environment)
            })
            .transpose()?;
        self.inner
            .ds
            .indexes()
            .vector()
            .set_embedding_environment(env);
        Ok(())
    }
    pub fn vector_embed_until_idle(&self, operation: Arc<FfiOperation>) -> FfiResult<()> {
        self.inner.check_writable()?;
        loop {
            operation.control.check()?;
            match self
                .inner
                .ds
                .indexes()
                .vector()
                .embed_until_idle(Duration::from_millis(20))
            {
                Ok(()) => return Ok(()),
                Err(sparkles::Error::Timeout) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    pub fn reason_status(&self) -> FfiResult<Option<Vec<u8>>> {
        self.inner
            .ds
            .reasoning()
            .status()
            .map(|s| encode(&json!({"record":s.record,"head":s.head,"freshness":s.freshness})))
            .transpose()
    }
    pub fn reason_diagnostics(
        &self,
        req: DiagnosticsRequest,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        #[cfg(feature = "reasoning")]
        {
            let opts = sparkles_reasoner::diagnostics::DiagnoseOptions {
                checks: req.checks,
                limit: req.limit as usize,
                inferences: req.inferences,
                graphs: sparkles_reasoner::diagnostics::parse_graphs(&req.graphs).map_err(bad)?,
                closure: sparkles_reasoner::diagnostics::Closure::parse(&req.closure)
                    .ok_or_else(|| bad("unknown closure"))?,
                timeout: operation
                    .control
                    .deadline
                    .map(|d| d.saturating_duration_since(Instant::now())),
                ..Default::default()
            };
            let r = self.inner.ds.reasoning().diagnostics(&opts)?;
            operation.control.check()?;
            encode(
                &json!({"report":r.report.to_json(),"commit":r.commit,"inferences":r.inferences}),
            )
        }
        #[cfg(not(feature = "reasoning"))]
        {
            let _ = req;
            Err(unsupported("reasoning"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn syntax_documents_and_invalid_base() {
        let invalid: serde_json::Value =
            serde_json::from_slice(&check_iri("http://example/a b".into()).unwrap()).unwrap();
        assert!(!invalid["errors"].as_array().unwrap().is_empty());
        assert_eq!(
            check_data("".into(), "turtle".into(), Some("bad base".into()))
                .unwrap_err()
                .kind(),
            ErrorKind::Invalid
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &check_data("<urn:s> <urn:p> 1 .".into(), "turtle".into(), None).unwrap()
            )
            .unwrap(),
            serde_json::Value::Null
        );
    }
    #[test]
    fn cancelled_capture_does_not_publish_clone() {
        let ds = FfiDataset::memory(crate::DatasetOptions {
            blank_node_labels: crate::BlankNodeMode::Dataset,
            read_only: false,
            term_cache_size: 1000,
        });
        let op = FfiOperation::new(None);
        op.cancel();
        assert_eq!(
            ds.clone_to_memory(op.clone()).err().unwrap().kind(),
            ErrorKind::Cancelled
        );
        assert_eq!(
            ds.explain("ASK {}".into(), op).unwrap_err().kind(),
            ErrorKind::Cancelled
        );
    }
    #[cfg(feature = "fmt")]
    #[test]
    fn formatter_checks_options_and_returns_syntax_kinds() {
        assert_eq!(
            format_document(
                "SELECT".into(),
                "sparql".into(),
                b"{}".to_vec(),
                FfiOperation::new(None)
            )
            .unwrap_err()
            .kind(),
            ErrorKind::SparqlSyntax
        );
        assert_eq!(
            format_document(
                "ASK {}".into(),
                "sparql".into(),
                br#"{"bogus":true}"#.to_vec(),
                FfiOperation::new(None)
            )
            .unwrap_err()
            .kind(),
            ErrorKind::Invalid
        );
        let out: serde_json::Value = serde_json::from_slice(
            &format_document(
                "# retained\nASK{}".into(),
                "sparql".into(),
                b"{}".to_vec(),
                FfiOperation::new(None),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out["text"].as_str().unwrap().contains("# retained"));
    }
}
