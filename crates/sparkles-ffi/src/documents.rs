//! Structured administration documents are encoded once per request, rather than per
//! nested field. The Java surface uses Jena's JSON trees and typed outer records.
use crate::{ErrorKind, FfiDataset, FfiError, FfiOperation, FfiResult};
use std::sync::Arc;
pub(crate) fn encode<T: serde::Serialize>(value: &T) -> FfiResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(|e| FfiError::new(ErrorKind::Invalid, e.to_string()))
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> FfiResult<T> {
    serde_json::from_slice(bytes).map_err(|e| FfiError::new(ErrorKind::Invalid, e.to_string()))
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct QueryChange {
    pub author: Option<String>,
    pub message: Option<String>,
    pub if_version: Option<u64>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct BoundQuery {
    pub query: String,
    pub bindings: std::collections::HashMap<String, String>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct SchemaRequest {
    pub at: Option<String>,
    pub limit: u32,
    pub cursor: Option<String>,
}
fn schema_request(
    req: SchemaRequest,
    op: &FfiOperation,
) -> FfiResult<sparkles::handles::ReportRequest> {
    Ok(sparkles::handles::ReportRequest {
        at: req.at.map(|s| s.parse()).transpose()?,
        limit: req.limit as usize,
        cursor: req.cursor,
        options: sparkles::schema::SchemaOptions {
            cancel: Some(op.control.cancel.flag()),
            deadline: op.control.deadline,
            ..Default::default()
        },
        ..Default::default()
    })
}
#[uniffi::export]
impl FfiDataset {
    pub fn queries_list(&self) -> FfiResult<Vec<u8>> {
        encode(&self.inner.ds.queries().list())
    }
    pub fn queries_get(&self, name: String, version: Option<u64>) -> FfiResult<Option<Vec<u8>>> {
        self.inner
            .ds
            .queries()
            .get(&name, version)
            .as_ref()
            .map(encode)
            .transpose()
    }
    pub fn queries_versions(&self, name: String) -> FfiResult<Option<Vec<u8>>> {
        self.inner
            .ds
            .queries()
            .versions(&name)
            .as_ref()
            .map(encode)
            .transpose()
    }
    pub fn queries_put(
        &self,
        name: String,
        definition: Vec<u8>,
        change: QueryChange,
    ) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        let result = self.inner.ds.queries().put(
            &name,
            decode(&definition)?,
            sparkles::stored::Change {
                author: change.author,
                message: change.message,
                if_version: change.if_version,
                dataset_commit: Some(self.inner.ds.head_commit().seq),
            },
        )?;
        encode(&result.stored)
    }
    pub fn queries_delete(&self, name: String, if_version: Option<u64>) -> FfiResult<bool> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.queries().delete(&name, if_version)?)
    }
    pub fn queries_bind(
        &self,
        name: String,
        version: Option<u64>,
        parameters: Vec<u8>,
    ) -> FfiResult<BoundQuery> {
        let (stored, bindings) =
            self.inner
                .ds
                .queries()
                .bind(&name, version, &decode(&parameters)?)?;
        Ok(BoundQuery {
            query: stored.definition.query,
            bindings: bindings
                .into_iter()
                .map(|(name, term)| (name, term.to_string()))
                .collect(),
        })
    }
    pub fn schema_report(
        &self,
        request: SchemaRequest,
        list: Option<String>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let r = self
            .inner
            .ds
            .schema()
            .report(&schema_request(request, &operation)?)?;
        match list.as_deref() {
            None => encode(&r.summary(self.inner.ds.name().unwrap_or("jvm"))),
            Some("classes") => encode(&r.classes()),
            Some("predicates") => encode(&r.predicates()),
            _ => Err(FfiError::new(ErrorKind::Invalid, "unknown schema list")),
        }
    }
    pub fn schema_diff(
        &self,
        from: String,
        request: SchemaRequest,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        encode(
            &self
                .inner
                .ds
                .schema()
                .diff(&from.parse()?, &schema_request(request, &operation)?)?
                .0,
        )
    }
    pub fn schema_profiles(
        &self,
        at: Option<String>,
        classes: Vec<String>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let at = at.map(|s| s.parse()).transpose()?;
        let opts = sparkles::schema::profile::ProfileOptions {
            classes,
            schema: sparkles::schema::SchemaOptions {
                cancel: Some(operation.control.cancel.flag()),
                deadline: operation.control.deadline,
                ..Default::default()
            },
        };
        encode(&self.inner.ds.schema().profiles_at(&opts, at.as_ref())?)
    }
    pub fn schema_draft(
        &self,
        at: Option<String>,
        classes: Vec<String>,
        support: f64,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let at = at.map(|s| s.parse()).transpose()?;
        let opts = sparkles::schema::draft::DraftOptions {
            classes,
            support,
            dataset: self.inner.ds.name().unwrap_or("jvm").into(),
            schema: sparkles::schema::SchemaOptions {
                cancel: Some(operation.control.cancel.flag()),
                deadline: operation.control.deadline,
                ..Default::default()
            },
            ..Default::default()
        };
        encode(&self.inner.ds.schema().draft_shapes_at(&opts, at.as_ref())?)
    }
    pub fn schema_constraints(
        &self,
        at: Option<String>,
        shapes: Vec<String>,
    ) -> FfiResult<Vec<u8>> {
        let req = sparkles::handles::ConstraintsRequest {
            at: at.map(|s| s.parse()).transpose()?,
            shapes: sparkles::handles::ShapesRequest::from_values(&shapes)
                .map_err(|e| FfiError::new(ErrorKind::Invalid, e))?,
            timeout: None,
            graphs: None,
        };
        encode(&self.inner.ds.schema().constraints(&req)?)
    }
    pub fn graphql_get(&self, version: Option<u64>) -> FfiResult<Option<Vec<u8>>> {
        #[cfg(feature = "graphql")]
        {
            self.inner
                .ds
                .graphql()
                .get(version)
                .as_ref()
                .map(encode)
                .transpose()
        }
        #[cfg(not(feature = "graphql"))]
        {
            let _ = version;
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without graphql",
            ))
        }
    }
    pub fn graphql_versions(&self) -> FfiResult<Vec<u8>> {
        #[cfg(feature = "graphql")]
        {
            encode(&self.inner.ds.graphql().versions())
        }
        #[cfg(not(feature = "graphql"))]
        {
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without graphql",
            ))
        }
    }
    pub fn graphql_reset(&self, if_version: Option<u64>) -> FfiResult<bool> {
        self.inner.check_writable()?;
        #[cfg(feature = "graphql")]
        {
            Ok(self.inner.ds.graphql().reset(if_version)?)
        }
        #[cfg(not(feature = "graphql"))]
        {
            let _ = if_version;
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without graphql",
            ))
        }
    }
    pub fn graphql_put(&self, config: Vec<u8>, change: QueryChange) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        #[cfg(feature = "graphql")]
        {
            let (saved, warnings) = self.inner.ds.graphql().put(
                decode(&config)?,
                sparkles::handles::graphql::Change {
                    author: change.author,
                    message: change.message,
                    if_version: change.if_version,
                    dataset_commit: Some(self.inner.ds.head_commit().seq),
                },
            )?;
            encode(
                &serde_json::json!({"stored":saved.stored,"warnings":warnings,"changed":saved.changed,"created":saved.created}),
            )
        }
        #[cfg(not(feature = "graphql"))]
        {
            let _ = (config, change);
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without graphql",
            ))
        }
    }
    pub fn graphql_sdl(&self) -> FfiResult<Option<String>> {
        #[cfg(feature = "graphql")]
        {
            Ok(self.inner.ds.graphql().sdl()?)
        }
        #[cfg(not(feature = "graphql"))]
        {
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without graphql",
            ))
        }
    }
    pub fn graphql_execute(
        &self,
        query: String,
        operation_name: Option<String>,
        variables: Vec<u8>,
        at: Option<String>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        #[cfg(feature = "graphql")]
        {
            let req = sparkles::handles::graphql::Request {
                query,
                operation_name,
                variables: decode(&variables)?,
            };
            let opts = sparkles::handles::graphql::Options {
                at: at.map(|s| s.parse()).transpose()?,
                query: sparkles::sparql::QueryOptions {
                    cancel: Some(operation.control.cancel.flag()),
                    timeout: operation
                        .control
                        .deadline
                        .map(|d| d.saturating_duration_since(std::time::Instant::now())),
                    ..Default::default()
                },
                ..Default::default()
            };
            encode(&self.inner.ds.graphql().execute(&req, &opts)?.body)
        }
        #[cfg(not(feature = "graphql"))]
        {
            let _ = (query, operation_name, variables, at);
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without graphql",
            ))
        }
    }
    pub fn graphql_draft(&self, source: Option<String>) -> FfiResult<Vec<u8>> {
        #[cfg(feature = "graphql")]
        {
            let (draft, commit) = self.inner.ds.graphql().draft(
                self.inner.ds.name().unwrap_or("jvm"),
                sparkles::handles::graphql::DraftRequest {
                    source,
                    ..Default::default()
                },
            )?;
            encode(&serde_json::json!({"draft":draft,"commit":commit}))
        }
        #[cfg(not(feature = "graphql"))]
        {
            let _ = source;
            Err(FfiError::new(
                ErrorKind::Unsupported,
                "built without graphql",
            ))
        }
    }
}
