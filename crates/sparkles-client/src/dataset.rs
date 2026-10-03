//! `Dataset`: a dataset of a Sparkles server, with the SPARQL Protocol of its
//! [`Endpoint`] plus uploads, batches, stored queries, commits, statistics, the schema
//! report and backups.

use crate::body::UploadPart;
use crate::client::{CallOptions, Client, ReqBody, call_options, read_json, read_typed};
use crate::endpoint::{Endpoint, QueryOptions, UpdateOptions, WriteCommon, receipt};
use crate::error::{Error, Result};
use crate::results::QueryResults;
use crate::routes;
use crate::types::{At, Commit, CommitList, DatasetInfo, Receipt, Task};
use oxrdf::{GraphName, Quad};
use reqwest::Url;
use serde_json::Value;
use std::fmt::Write as _;
use std::ops::Deref;

/// A dataset of a Sparkles server. It dereferences to its [`Endpoint`], so `query`,
/// `select`, `update`, `get_graph`, `put_graph` and the other protocol methods work on it,
/// and they send Sparkles' parameters: every write asks for a receipt.
#[derive(Clone, Debug)]
pub struct Dataset {
    endpoint: Endpoint,
    name: String,
}

impl Deref for Dataset {
    type Target = Endpoint;
    fn deref(&self) -> &Endpoint {
        &self.endpoint
    }
}

impl Client {
    /// A dataset of the server. Nothing is sent until it is used.
    pub fn dataset(&self, name: impl Into<String>) -> Dataset {
        let name = name.into();
        let url = |op: &routes::Op| {
            self.op_url(op, &[("ds", &name)]).unwrap_or_else(|_| {
                // a client without a server URL: every call fails with a clear error
                Url::parse(&format!("sparkles:{}", op.path)).expect("a valid URL")
            })
        };
        Dataset {
            endpoint: Endpoint {
                client: self.clone(),
                query_url: url(&routes::SPARQL_GET),
                update_url: Some(url(&routes::UPDATE)),
                gsp_url: Some(url(&routes::GSP_GET)),
                dataset: Some(name.clone()),
            },
            name,
        }
    }
}

/// Options of an upload.
#[derive(Clone, Debug, Default)]
pub struct UploadOptions {
    graph: Option<String>,
    base: Option<String>,
    key: Option<String>,
    write: WriteCommon,
    call: CallOptions,
}

impl UploadOptions {
    pub fn new() -> Self {
        Self::default()
    }
    /// Load the files' triples into this named graph.
    pub fn graph(mut self, iri: impl Into<String>) -> Self {
        self.graph = Some(iri.into());
        self
    }
    /// The namespace of the default table mapping.
    pub fn base(mut self, iri: impl Into<String>) -> Self {
        self.base = Some(iri.into());
        self
    }
    /// The column that names each row in the default table mapping.
    pub fn key(mut self, column: impl Into<String>) -> Self {
        self.key = Some(column.into());
        self
    }
    pub fn message(mut self, m: impl Into<String>) -> Self {
        self.write.message = Some(m.into());
        self
    }
    pub fn dry_run(mut self) -> Self {
        self.write.dry_run = true;
        self
    }
}
call_options!(UploadOptions);

/// Which commits to list.
#[derive(Clone, Debug, Default)]
pub struct CommitsOptions {
    /// Page size (the server's default is 50, its maximum 1000).
    pub limit: Option<u32>,
    /// Commits before this one, newest first.
    pub before: Option<u64>,
    /// Commits after this one, oldest first.
    pub after: Option<u64>,
}

/// Updates collected on the client and sent as one request, which the server applies as
/// one commit: all of them or none.
#[derive(Clone, Debug)]
pub struct UpdateBatch {
    dataset: Dataset,
    text: String,
    count: usize,
    options: UpdateOptions,
}

impl UpdateBatch {
    fn push(&mut self, op: &str) {
        if self.count > 0 {
            self.text.push_str(" ;\n");
        }
        self.text.push_str(op.trim().trim_end_matches(';'));
        self.count += 1;
    }

    /// Add a SPARQL update operation (or several, separated by `;`).
    pub fn update(mut self, update: &str) -> Self {
        self.push(update);
        self
    }

    /// Add `INSERT DATA` for quads.
    pub fn insert<'a>(mut self, quads: impl IntoIterator<Item = &'a Quad>) -> Self {
        let block = data_block(quads);
        if !block.is_empty() {
            self.push(&format!("INSERT DATA {{\n{block}}}"));
        }
        self
    }

    /// Add `DELETE DATA` for quads (without blank nodes, which `DELETE DATA` refuses).
    pub fn delete<'a>(mut self, quads: impl IntoIterator<Item = &'a Quad>) -> Self {
        let block = data_block(quads);
        if !block.is_empty() {
            self.push(&format!("DELETE DATA {{\n{block}}}"));
        }
        self
    }

    /// The options of the update request.
    pub fn options(mut self, options: UpdateOptions) -> Self {
        self.options = options;
        self
    }

    /// The number of operations added.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The update request the batch sends.
    pub fn to_sparql(&self) -> &str {
        &self.text
    }

    /// Send the batch as one update.
    pub async fn commit(self) -> Result<Receipt> {
        if self.count == 0 {
            return Err(Error::config("the batch is empty"));
        }
        self.dataset.update_with(&self.text, &self.options).await
    }
}

/// The body of `INSERT DATA` or `DELETE DATA`: default-graph triples first, then one
/// `GRAPH` block per named graph, in order of first appearance.
fn data_block<'a>(quads: impl IntoIterator<Item = &'a Quad>) -> String {
    let mut default = String::new();
    let mut named: Vec<(String, String)> = Vec::new();
    for q in quads {
        let line = format!("  {} {} {} .\n", q.subject, q.predicate, q.object);
        match &q.graph_name {
            GraphName::DefaultGraph => default.push_str(&line),
            g => {
                let key = g.to_string();
                match named.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, b)) => b.push_str(&line),
                    None => named.push((key, line)),
                }
            }
        }
    }
    let mut out = default;
    for (g, body) in named {
        let _ = write!(out, "  GRAPH {g} {{\n{body}  }}\n");
    }
    out
}

impl Dataset {
    /// The dataset's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    #[cfg(feature = "blocking")]
    pub(crate) fn endpoint_owned(self) -> Endpoint {
        self.endpoint
    }

    fn params(&self) -> [(&str, &str); 1] {
        [("ds", self.name.as_str())]
    }

    /// Start a batch of updates that commit together.
    pub fn batch(&self) -> UpdateBatch {
        UpdateBatch {
            dataset: self.clone(),
            text: String::new(),
            count: 0,
            options: UpdateOptions::default(),
        }
    }

    /// Upload RDF files and CSV or TSV tables in one commit (`POST /{ds}/upload`).
    pub async fn upload(&self, parts: Vec<UploadPart>, opts: &UploadOptions) -> Result<Receipt> {
        let mut r = self.client.op_req(&routes::UPLOAD, &self.params())?;
        if let Some(g) = &opts.graph {
            r.param("graph", g.clone());
        }
        if let Some(b) = &opts.base {
            r.param("base", b.clone());
        }
        if let Some(k) = &opts.key {
            r.param("key", k.clone());
        }
        r.param("receipt", "true");
        if let Some(t) = opts.write.server_timeout {
            r.param("timeout", t.as_secs_f64().to_string());
        }
        if opts.write.dry_run {
            r.param("dryRun", "true");
        }
        if let Some(m) = &opts.write.message {
            r.header(
                "Sparkles-Commit-Message",
                crate::endpoint::message_header(m),
            );
        }
        r.body = ReqBody::Multipart(parts);
        r.accept = Some("application/json");
        r.call = opts.call.clone();
        receipt(self.client.send(r).await?).await
    }

    // ---------------------------------------------------------- stored queries ----

    /// Run a stored query by name. `params` is a JSON object of parameter values; numbers
    /// and booleans keep their JSON types.
    pub async fn run_stored(&self, name: &str, params: &Value) -> Result<QueryResults> {
        self.run_stored_with(name, params, &QueryOptions::default(), None)
            .await
    }

    /// Run a stored query with options, at a given version of its definition.
    pub async fn run_stored_with(
        &self,
        name: &str,
        params: &Value,
        opts: &QueryOptions,
        version: Option<u64>,
    ) -> Result<QueryResults> {
        let mut r = self
            .client
            .op_req(&routes::RUN_STORED, &[("ds", &self.name), ("name", name)])?;
        if let Some(at) = &opts.at {
            r.param("at", at.to_string());
        }
        if let Some(on) = opts.reasoning {
            r.param("reasoning", on.to_string());
        }
        if let Some(t) = opts.server_timeout {
            r.param("timeout", t.as_secs_f64().to_string());
        }
        if let Some(v) = version {
            r.param("version", v.to_string());
        }
        if !opts.default_graphs.is_empty() || !opts.named_graphs.is_empty() {
            return Err(Error::config(
                "a stored query declares its own dataset; default and named graphs do not apply",
            ));
        }
        let body = if params.is_null() {
            Value::Object(Default::default())
        } else {
            params.clone()
        };
        r.body(ReqBody::json(&body));
        r.safe = true;
        r.accept = Some(
            "application/sparql-results+json, application/sparql-results+xml;q=0.9, \
             text/tab-separated-values;q=0.8, application/n-triples;q=0.7, text/turtle;q=0.6",
        );
        r.call = opts.call.clone();
        QueryResults::from_response(self.client.send(r).await?).await
    }

    /// The stored queries, without their text (`GET /$/queries/{ds}`).
    pub async fn stored_queries(&self) -> Result<Value> {
        let r = self.client.op_req(&routes::LIST_STORED, &self.params())?;
        read_json(self.client.send(r).await?).await
    }

    /// A stored query's definition (`GET /$/queries/{ds}/{name}`).
    pub async fn stored_query(&self, name: &str) -> Result<Value> {
        let r = self
            .client
            .op_req(&routes::GET_STORED, &[("ds", &self.name), ("name", name)])?;
        read_json(self.client.send(r).await?).await
    }

    /// Store a query's definition as its next version (`PUT /$/queries/{ds}/{name}`).
    pub async fn put_stored_query(&self, name: &str, definition: &Value) -> Result<Value> {
        let mut r = self
            .client
            .op_req(&routes::PUT_STORED, &[("ds", &self.name), ("name", name)])?;
        r.body(ReqBody::json(definition));
        read_json(self.client.send(r).await?).await
    }

    /// Remove a stored query and its versions.
    pub async fn delete_stored_query(&self, name: &str) -> Result<()> {
        let r = self.client.op_req(
            &routes::DELETE_STORED,
            &[("ds", &self.name), ("name", name)],
        )?;
        self.client.send(r).await?;
        Ok(())
    }

    // ------------------------------------------------------------ commits ----

    /// A page of the dataset's commits.
    pub async fn commits(&self, opts: &CommitsOptions) -> Result<CommitList> {
        let mut r = self.client.op_req(&routes::LIST_COMMITS, &self.params())?;
        if let Some(l) = opts.limit {
            r.param("limit", l.to_string());
        }
        if let Some(b) = opts.before {
            r.param("before", b.to_string());
        }
        if let Some(a) = opts.after {
            r.param("after", a.to_string());
        }
        read_typed(self.client.send(r).await?).await
    }

    /// One commit: `head`, `42` or `commit:42`.
    pub async fn commit(&self, reference: &str) -> Result<Commit> {
        let r = self.client.op_req(
            &routes::GET_COMMIT,
            &[("ds", &self.name), ("reference", reference)],
        )?;
        let c: crate::types::CommitResponse = read_typed(self.client.send(r).await?).await?;
        Ok(c.commit)
    }

    // ------------------------------------------------------ admin of one dataset ----

    /// The dataset's description (`GET /$/datasets/{ds}`).
    pub async fn info(&self) -> Result<DatasetInfo> {
        let r = self.client.op_req(&routes::GET_DATASET, &self.params())?;
        read_typed(self.client.send(r).await?).await
    }

    /// The dataset's statistics (`GET /$/stats/{ds}`), at a past state if given.
    pub async fn stats(&self, at: Option<At>) -> Result<Value> {
        let mut r = self.client.op_req(&routes::DATASET_STATS, &self.params())?;
        if let Some(a) = at {
            r.param("at", a.to_string());
        }
        read_json(self.client.send(r).await?).await
    }

    /// The schema report: classes and predicates with their counts and declarations
    /// (`GET /$/schema/{ds}`).
    pub async fn schema(&self, at: Option<At>) -> Result<Value> {
        let mut r = self.client.op_req(&routes::SCHEMA, &self.params())?;
        if let Some(a) = at {
            r.param("at", a.to_string());
        }
        r.accept = Some("application/json");
        read_json(self.client.send(r).await?).await
    }

    /// The dataset's backups in the backup repositories (`GET /$/backups/{ds}`).
    pub async fn backups(&self) -> Result<Value> {
        let r = self
            .client
            .op_req(&routes::DATASET_BACKUPS, &self.params())?;
        read_json(self.client.send(r).await?).await
    }

    /// Start Fuseki's N-Quads backup into the server's `backups` directory
    /// (`POST /$/backup/{ds}`), and return its task.
    pub async fn backup_nquads(&self) -> Result<Task> {
        let r = self.client.op_req(&routes::BACKUP_NQUADS, &self.params())?;
        read_typed(self.client.send(r).await?).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{Literal, NamedNode};

    #[test]
    fn batches_group_quads_by_graph() {
        let s = NamedNode::new("http://e/s").unwrap();
        let p = NamedNode::new("http://e/p").unwrap();
        let g = NamedNode::new("http://e/g").unwrap();
        let qs = [
            Quad::new(
                s.clone(),
                p.clone(),
                Literal::from(1),
                GraphName::DefaultGraph,
            ),
            Quad::new(s.clone(), p.clone(), Literal::from("a"), g.clone()),
            Quad::new(s.clone(), p.clone(), Literal::from("b"), g),
        ];
        let c = Client::new("http://localhost:1").unwrap();
        let b = c
            .dataset("ds")
            .batch()
            .insert(&qs)
            .update("DELETE WHERE { ?s <http://e/q> ?o };")
            .delete(&qs[..1]);
        assert_eq!(b.len(), 3);
        assert_eq!(
            b.to_sparql(),
            "INSERT DATA {\n  <http://e/s> <http://e/p> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n  \
             GRAPH <http://e/g> {\n  <http://e/s> <http://e/p> \"a\" .\n  <http://e/s> <http://e/p> \"b\" .\n  }\n} ;\n\
             DELETE WHERE { ?s <http://e/q> ?o } ;\n\
             DELETE DATA {\n  <http://e/s> <http://e/p> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n}"
        );
        assert!(c.dataset("ds").batch().is_empty());
    }

    #[test]
    fn dataset_urls() {
        let c = Client::new("http://localhost:3030/base/").unwrap();
        let ds = c.dataset("my ds");
        assert_eq!(
            ds.query_url().as_str(),
            "http://localhost:3030/base/my%20ds/sparql"
        );
        let none = crate::ClientBuilder::new().build().unwrap();
        assert!(
            none.dataset("ds")
                .query_url()
                .as_str()
                .starts_with("sparkles:")
        );
    }
}
