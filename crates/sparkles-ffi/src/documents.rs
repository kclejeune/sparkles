//! Structured administration documents are encoded once per request, rather than per
//! nested field. The Java surface uses Jena's JSON trees and typed outer records.
use crate::{ErrorKind, FfiDataset, FfiError, FfiOperation, FfiReadTxn, FfiResult, FfiWriteTxn};
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
/// The view and the budgets of a graph overview call, which are those of a query that bear
/// on the overview's own queries. The timeout and the cancellation come from the call's
/// operation.
#[derive(Clone, Debug, uniffi::Record)]
pub struct GroupsOpts {
    pub union_default_graph: Option<bool>,
    pub default_graph: Vec<String>,
    pub named_graphs: Vec<String>,
    pub include_inferred: bool,
    pub max_rows: Option<u64>,
    pub max_memory_bytes: Option<u64>,
    pub max_rows_produced: Option<u64>,
    pub no_cache: bool,
}
/// The overview request and the options of its queries. The dataset's defaults apply as they
/// do to its queries, but only a call that asks for the inferred graph reads it.
fn groups_args(
    ds: &sparkles::Dataset,
    request: &[u8],
    opts: GroupsOpts,
    operation: &FfiOperation,
) -> FfiResult<(
    sparkles::overview::GroupsRequest,
    sparkles::sparql::QueryOptions,
)> {
    let req = sparkles::overview::GroupsRequest::from_json(&decode(request)?)?;
    // the transactions answer at their own snapshot, past the check of `graph().groups`
    req.check_branch(&ds.store().branch_name())?;
    let mut q = sparkles::sparql::QueryOptions {
        union_default_graph: opts.union_default_graph,
        default_graph_uris: opts.default_graph,
        named_graph_uris: opts.named_graphs,
        max_rows: opts.max_rows.map(|m| m as usize),
        max_memory_bytes: opts.max_memory_bytes,
        max_rows_produced: opts.max_rows_produced,
        cancel: Some(operation.control.cancel.flag()),
        deadline: operation.control.deadline,
        no_cache: opts.no_cache,
        ..Default::default()
    };
    if opts.include_inferred {
        q.default_graph_extra = vec![crate::query::INFERRED_GRAPH.to_string()];
    }
    let mut q = ds.with_query_defaults(&q).into_owned();
    if !opts.include_inferred {
        q.default_graph_extra.clear();
    }
    Ok((req, q))
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
#[derive(Clone, Debug, uniffi::Record)]
pub struct MappingInputs {
    pub sources: std::collections::HashMap<String, String>,
    pub connections: std::collections::HashMap<String, String>,
    pub directory: Option<String>,
    pub base: Option<String>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct MappingRunRequest {
    pub version: Option<u64>,
    pub inputs: MappingInputs,
    pub replace: bool,
    pub author: Option<String>,
    pub message: Option<String>,
}
fn mapping_inputs(input: &MappingInputs) -> FfiResult<sparkles::mapping::Inputs> {
    use sparkles::mapping::{Connection, Inputs, NamedConnection};
    let connections = input
        .connections
        .iter()
        .map(|(name, url)| {
            let connection = Connection::from_url(url).map_err(|e| {
                FfiError::new(ErrorKind::Invalid, format!("connection {name}: {e}"))
            })?;
            Ok(NamedConnection {
                name: name.clone(),
                connection,
                jdbc_dsn: None,
            })
        })
        .collect::<FfiResult<Vec<_>>>()?;
    let mut files: Vec<_> = input
        .sources
        .iter()
        .map(|(name, path)| (name.clone(), path.into()))
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Inputs {
        files,
        dir: input.directory.as_ref().map(Into::into),
        connections,
    })
}
fn mapping_kind(kind: Option<&str>) -> FfiResult<Option<sparkles::mapping::MappingKind>> {
    kind.map(|k| {
        sparkles::mapping::MappingKind::parse(k).ok_or_else(|| {
            FfiError::new(
                ErrorKind::Invalid,
                format!("kind {k:?}: use rml, yarrrml, csvw or template"),
            )
        })
    })
    .transpose()
}
fn mapping_opts(input: &MappingInputs, operation: &FfiOperation) -> sparkles::mapping::RunOptions {
    let control = operation.control.clone();
    sparkles::mapping::RunOptions {
        base: input.base.clone(),
        check: Some(Arc::new(move || control.check())),
        ..Default::default()
    }
}
#[uniffi::export]
impl FfiDataset {
    pub fn mappings_list(&self) -> FfiResult<Vec<u8>> {
        encode(&self.inner.ds.mappings().list().into_iter().map(|(name, stored, last)| serde_json::json!({"name": name, "kind": stored.definition.kind, "description": stored.definition.description, "version": stored.version, "lastRun": last})).collect::<Vec<_>>())
    }
    pub fn mappings_get(&self, name: String, version: Option<u64>) -> FfiResult<Option<Vec<u8>>> {
        self.inner
            .ds
            .mappings()
            .get(&name, version)
            .as_ref()
            .map(encode)
            .transpose()
    }
    pub fn mappings_versions(&self, name: String) -> FfiResult<Option<Vec<u8>>> {
        self.inner
            .ds
            .mappings()
            .versions(&name)
            .as_ref()
            .map(encode)
            .transpose()
    }
    pub fn mappings_last_run(&self, name: String) -> FfiResult<Option<Vec<u8>>> {
        self.inner
            .ds
            .mappings()
            .last_run(&name)
            .as_ref()
            .map(encode)
            .transpose()
    }
    pub fn mappings_put(
        &self,
        name: String,
        text: String,
        kind: Option<String>,
        description: Option<String>,
        change: QueryChange,
    ) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        let kind = mapping_kind(kind.as_deref())?.or_else(|| sparkles::mapping::detect(&text)).ok_or_else(|| FfiError::new(ErrorKind::Invalid, "the text is not CSVW metadata, R2RML, RML, YARRRML or a CONSTRUCT template; give kind"))?;
        let def = sparkles::stored_mappings::Definition {
            kind: kind.name().to_string(),
            text,
            description,
        };
        let change = sparkles::stored_mappings::Change {
            author: change.author,
            message: change.message,
            if_version: change.if_version,
            dataset_commit: Some(self.inner.ds.head_commit().seq),
        };
        let r = self.inner.ds.mappings().put(&name, def, change)?;
        encode(
            &serde_json::json!({"stored": r.stored, "summary": r.summary, "created": r.created, "changed": r.changed}),
        )
    }
    pub fn mappings_delete(&self, name: String, if_version: Option<u64>) -> FfiResult<bool> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.mappings().delete(&name, if_version)?)
    }
    pub fn mappings_run(
        &self,
        name: String,
        request: MappingRunRequest,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        operation.control.check()?;
        let write = sparkles::guard::WriteOptions {
            author: request.author.map(Into::into),
            message: request.message.map(Into::into),
            cancel: Some(operation.control.cancel.flag()),
            deadline: operation.control.deadline,
            ..Default::default()
        };
        let r = self.inner.ds.mappings().run(
            &name,
            request.version,
            &mapping_inputs(&request.inputs)?,
            &mapping_opts(&request.inputs, &operation),
            request.replace,
            &write,
        )?;
        encode(&serde_json::json!({"receipt": r.receipt, "stats": r.stats}))
    }
    pub fn run_mapping(
        &self,
        text: String,
        kind: Option<String>,
        input: MappingInputs,
        graph: Option<String>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        operation.control.check()?;
        let r = self.inner.ds.run_mapping(
            &text,
            mapping_kind(kind.as_deref())?,
            &mapping_inputs(&input)?,
            &mapping_opts(&input, &operation),
            graph.as_deref(),
        )?;
        encode(&serde_json::json!({"receipt": r.receipt, "stats": r.stats}))
    }
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
    /// The answer of `ds.graph().groups` for a request in JSON. Every query of the overview
    /// shares the deadline of `operation`, which is one deadline for the whole call.
    pub fn graph_groups(
        &self,
        request: Vec<u8>,
        opts: GroupsOpts,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let (req, q) = groups_args(&self.inner.ds, &request, opts, &operation)?;
        encode(&self.inner.ds.graph().groups(&req, &q)?)
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
#[uniffi::export]
impl FfiReadTxn {
    /// As `FfiDataset::graph_groups`, over the snapshot of this transaction. The request's
    /// `at` is not read, because the snapshot is the point in history.
    pub fn graph_groups(
        &self,
        request: Vec<u8>,
        opts: GroupsOpts,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let (req, q) = groups_args(&self.ds.ds, &request, opts, &operation)?;
        encode(&sparkles::overview::groups_at(self.snap.clone(), &req, &q)?)
    }
}
#[uniffi::export]
impl FfiWriteTxn {
    /// As `FfiDataset::graph_groups`, over the transaction's view, which holds its changes.
    /// The request's `at` is not read.
    pub fn graph_groups(
        &self,
        request: Vec<u8>,
        opts: GroupsOpts,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let (req, q) = groups_args(&self.ds.ds, &request, opts, &operation)?;
        encode(&sparkles::overview::groups_at(self.view()?, &req, &q)?)
    }
}
