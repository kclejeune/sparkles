//! Typed reasoning, validation and index administration records.
use crate::{ErrorKind, FfiDataset, FfiError, FfiOperation, FfiResult};
use std::sync::Arc;

#[derive(Clone, Debug, uniffi::Record)]
pub struct VectorSettings {
    pub predicate: String,
    pub dimension: u32,
    pub metric: String,
    pub model: Option<String>,
    pub approximate: bool,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct VectorInfo {
    pub name: String,
    pub dimension: u32,
    pub metric: String,
    pub state: String,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct GeoSettings {
    pub wgs84: bool,
    pub query_rewrite: bool,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct GeoInfo {
    pub enabled: bool,
    pub state: String,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct ReasonSettings {
    pub profile: String,
    pub rules: Option<String>,
    pub incremental: bool,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct ReasonInfo {
    pub profile: String,
    pub inferred: u64,
    pub iterations: u64,
    pub millis: u64,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct ShaclResult {
    pub focus_node: String,
    pub path: Option<String>,
    pub value: Option<String>,
    pub source_shape: String,
    pub severity: String,
    pub messages: Vec<String>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct ShaclReport {
    pub conforms: bool,
    pub results: Vec<ShaclResult>,
    pub turtle: String,
}
#[allow(dead_code)]
fn unsupported(feature: &str) -> FfiError {
    FfiError::new(
        ErrorKind::Unsupported,
        format!("this native library was built without {feature}"),
    )
}
fn bad(e: impl ToString) -> FfiError {
    FfiError::new(ErrorKind::Invalid, e.to_string())
}

#[uniffi::export]
impl FfiDataset {
    pub fn vector_list(&self) -> Vec<VectorInfo> {
        self.inner
            .ds
            .indexes()
            .vector()
            .list()
            .into_iter()
            .map(vector_info)
            .collect()
    }
    pub fn vector_get(&self, name: String) -> Option<VectorInfo> {
        self.inner.ds.indexes().vector().get(&name).map(vector_info)
    }
    pub fn vector_put(&self, name: String, s: VectorSettings) -> FfiResult<bool> {
        self.inner.check_writable()?;
        let mut cfg = sparkles::vector::VectorIndexConfig::new(&s.predicate, s.dimension as usize);
        cfg.metric = sparkles::vector::Metric::parse(&s.metric)
            .ok_or_else(|| bad("unknown vector metric"))?;
        cfg.model = s.model;
        if !s.approximate {
            cfg.hnsw = None;
        }
        Ok(self.inner.ds.indexes().vector().put(&name, cfg)?)
    }
    pub fn vector_delete(&self, name: String) -> FfiResult<()> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.indexes().vector().drop(&name)?)
    }
    pub fn vector_rebuild(&self, name: String) -> FfiResult<()> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.indexes().vector().rebuild(&name)?)
    }
    pub fn vector_wait(&self, name: String) -> Option<VectorInfo> {
        self.inner
            .ds
            .indexes()
            .vector()
            .wait(&name)
            .map(vector_info)
    }
    pub fn geo_status(&self) -> Option<GeoInfo> {
        self.inner.ds.indexes().geo().status().map(geo_info)
    }
    pub fn geo_enable(&self, s: GeoSettings) -> FfiResult<GeoInfo> {
        self.inner.check_writable()?;
        Ok(geo_info(self.inner.ds.indexes().geo().enable(
            sparkles::geo::GeoConfig {
                wgs84: s.wgs84,
                query_rewrite: s.query_rewrite,
                ..Default::default()
            },
        )?))
    }
    pub fn geo_disable(&self) -> FfiResult<()> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.indexes().geo().disable()?)
    }
    pub fn geo_rebuild(&self) -> FfiResult<GeoInfo> {
        self.inner.check_writable()?;
        Ok(geo_info(self.inner.ds.indexes().geo().rebuild()?))
    }
    pub fn geo_wait(&self) -> Option<GeoInfo> {
        self.inner.ds.indexes().geo().wait().map(geo_info)
    }

    pub fn rdfs_set_graph(&self, graph: String) -> FfiResult<()> {
        self.inner.check_writable()?;
        Ok(self
            .inner
            .ds
            .reasoning()
            .rdfs()
            .set(sparkles::reasoning::rdfs::NewSchema::Graph(graph))?)
    }
    pub fn rdfs_enabled(&self) -> bool {
        self.inner.ds.reasoning().rdfs().get().is_some()
    }
    pub fn rdfs_reset(&self) -> FfiResult<()> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.reasoning().rdfs().reset()?)
    }

    pub fn reason_run(
        &self,
        s: ReasonSettings,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<ReasonInfo> {
        self.inner.check_writable()?;
        operation.control.check()?;
        #[cfg(feature = "reasoning")]
        {
            let profile = if s.profile == "rules" {
                sparkles_reasoner::Profile::Rules(
                    s.rules
                        .ok_or_else(|| bad("rules profile requires rule text"))?,
                )
            } else {
                s.profile.parse().map_err(bad)?
            };
            let request = sparkles::reasoning::ReasonRequest {
                profile,
                incremental: s.incremental,
                ..Default::default()
            };
            let r = self
                .inner
                .ds
                .reasoning()
                .run_with(&request, &operation.control)?
                .report;
            Ok(ReasonInfo {
                profile: r.profile,
                inferred: r.inferred,
                iterations: r.iterations as u64,
                millis: r.millis,
                warnings: r.warnings,
            })
        }
        #[cfg(not(feature = "reasoning"))]
        {
            let _ = s;
            Err(unsupported("reasoning"))
        }
    }
    pub fn reason_clear(&self) -> FfiResult<u64> {
        self.inner.check_writable()?;
        #[cfg(feature = "reasoning")]
        {
            Ok(self.inner.ds.reasoning().clear()?)
        }
        #[cfg(not(feature = "reasoning"))]
        {
            Err(unsupported("reasoning"))
        }
    }
    pub fn validate_shacl(
        &self,
        shapes: String,
        format: String,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<ShaclReport> {
        operation.control.check()?;
        #[cfg(feature = "shacl")]
        {
            let syntax = if format == "shaclc" {
                sparkles_shacl::ShapesSyntax::Compact
            } else {
                oxrdfio::RdfFormat::from_media_type(&format)
                    .or_else(|| oxrdfio::RdfFormat::from_extension(&format))
                    .ok_or_else(|| bad("unknown shapes format"))?
                    .into()
            };
            let shapes = sparkles_shacl::Shapes::parse(&shapes, syntax, None).map_err(bad)?;
            let opts = sparkles_shacl::ValidateOptions {
                cancel: Some(operation.control.cancel.flag()),
                timeout: operation
                    .control
                    .deadline
                    .map(|d| d.saturating_duration_since(std::time::Instant::now())),
                ..Default::default()
            };
            let report = self.inner.ds.validation().shacl(&shapes, &opts)?;
            let turtle = report.to_turtle();
            let results = report
                .results
                .into_iter()
                .map(|r| ShaclResult {
                    focus_node: r.focus_node.to_string(),
                    path: r.result_path.map(|p| p.to_string()),
                    value: r.value.map(|t| t.to_string()),
                    source_shape: r.source_shape.to_string(),
                    severity: r.severity.as_str().into(),
                    messages: r.messages.into_iter().map(|m| m.value().into()).collect(),
                })
                .collect();
            Ok(ShaclReport {
                conforms: report.conforms,
                results,
                turtle,
            })
        }
        #[cfg(not(feature = "shacl"))]
        {
            let _ = (shapes, format);
            Err(unsupported("shacl"))
        }
    }
}
fn vector_info(s: sparkles::vector::VectorIndexStatus) -> VectorInfo {
    VectorInfo {
        name: s.name,
        dimension: s.dimension as u32,
        metric: s.metric.name().into(),
        state: s.state,
    }
}
fn geo_info(s: sparkles::geo::GeoStatus) -> GeoInfo {
    GeoInfo {
        enabled: s.enabled,
        state: s.state.to_string(),
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ShexResult {
    pub node: String,
    pub shape: String,
    pub conformant: bool,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct ShexReport {
    pub conforms: bool,
    pub results: Vec<ShexResult>,
    pub warnings: Vec<String>,
    pub millis: u64,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct GuardSettings {
    pub mode: String,
    pub shapes: String,
    pub format: String,
    pub include_inferred: bool,
    pub report_limit: u32,
    pub timeout_seconds: f64,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct GuardInfo {
    pub status: String,
    pub conforms: Option<bool>,
    pub blocking: u64,
}

#[uniffi::export]
impl FfiDataset {
    pub fn validate_shex(
        &self,
        schema: String,
        shape_map: String,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<ShexReport> {
        operation.control.check()?;
        #[cfg(feature = "shex")]
        {
            let parsed = sparkles_shex::parse_schema(&schema, None, None).map_err(bad)?;
            let compiled =
                sparkles_shex::compile(&parsed, &sparkles_shex::NoImports).map_err(bad)?;
            let map = if shape_map.trim_start().starts_with('[') {
                sparkles_shex::ShapeMap::from_json(&shape_map)
            } else {
                sparkles_shex::ShapeMap::parse(&shape_map, compiled.prefixes(), compiled.base())
            }
            .map_err(bad)?;
            let opts = sparkles_shex::ValidateOptions {
                cancel: Some(operation.control.cancel.flag()),
                timeout: operation
                    .control
                    .deadline
                    .map(|d| d.saturating_duration_since(std::time::Instant::now())),
                ..Default::default()
            };
            let report = self.inner.ds.validation().shex(&compiled, &map, &opts)?;
            let results = report
                .results
                .into_iter()
                .map(|r| ShexResult {
                    node: r.node.to_string(),
                    shape: match r.shape {
                        sparkles_shex::ShapeLabel::Iri(s) => s,
                        sparkles_shex::ShapeLabel::BNode(s) => format!("_:{s}"),
                        sparkles_shex::ShapeLabel::Start => "START".into(),
                    },
                    conformant: r.status == sparkles_shex::Status::Conformant,
                    reason: r.reason,
                })
                .collect();
            Ok(ShexReport {
                conforms: report.conforms,
                results,
                warnings: report.warnings,
                millis: report.millis,
            })
        }
        #[cfg(not(feature = "shex"))]
        {
            let _ = (schema, shape_map);
            Err(unsupported("shex"))
        }
    }
    pub fn guard_status(&self) -> Option<GuardInfo> {
        self.inner.ds.validation().guard().get().map(|guard| {
            let json = guard.json();
            GuardInfo {
                status: json["config"]["mode"].as_str().unwrap_or("unknown").into(),
                conforms: json["status"]["conforms"].as_bool(),
                blocking: json["status"]["blocking"].as_u64().unwrap_or(0),
            }
        })
    }
    pub fn guard_set_shacl(&self, s: GuardSettings) -> FfiResult<GuardInfo> {
        self.inner.check_writable()?;
        #[cfg(feature = "shacl")]
        {
            let mode = match s.mode.as_str() {
                "reject" => sparkles::guard::GuardMode::Reject,
                "warn" => sparkles::guard::GuardMode::Warn,
                "off" => sparkles::guard::GuardMode::Off,
                _ => return Err(bad("unknown validation guard mode")),
            };
            let cfg = sparkles_shacl::guard::ValidationConfig {
                format: 2,
                language: Some(sparkles::guard::GuardLanguage::Shacl),
                mode,
                shapes: sparkles_shacl::guard::ShapesSource {
                    inline: Some(s.shapes),
                    format: Some(s.format),
                    ..Default::default()
                },
                data_graph: Default::default(),
                include_inferences: s.include_inferred,
                threshold: sparkles::guard::Severity::Violation,
                baseline: Default::default(),
                timeout_seconds: s.timeout_seconds,
                report_limit: s.report_limit as usize,
                updated: None,
            };
            use sparkles::handles::GuardOutcome;
            Ok(match self.inner.ds.validation().guard().set_shacl(cfg)? {
                GuardOutcome::Installed(summary) => GuardInfo {
                    status: "installed".into(),
                    conforms: Some(summary.conforms),
                    blocking: summary.blocking,
                },
                GuardOutcome::NotConforming(summary) => GuardInfo {
                    status: "not-conforming".into(),
                    conforms: Some(summary.conforms),
                    blocking: summary.blocking,
                },
                GuardOutcome::Removed => GuardInfo {
                    status: "removed".into(),
                    conforms: None,
                    blocking: 0,
                },
                _ => return Err(bad("unknown guard outcome")),
            })
        }
        #[cfg(not(feature = "shacl"))]
        {
            let _ = s;
            Err(unsupported("shacl"))
        }
    }
    pub fn guard_reset(&self) -> FfiResult<()> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.validation().guard().reset()?)
    }
}
