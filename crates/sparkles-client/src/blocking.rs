//! The client without async: the same API on a private tokio runtime with one worker
//! thread. Results are `Iterator`s. Like reqwest's blocking client, it must not be used
//! from inside an async runtime, where blocking on a future panics.
//!
//! ```no_run
//! use sparkles_client::blocking::Client;
//!
//! let client = Client::new("http://localhost:3030")?;
//! let ds = client.dataset("ds");
//! ds.update("INSERT DATA { <urn:a> <urn:p> 1 }")?;
//! for s in ds.select("SELECT * { ?s ?p ?o }")? {
//!     println!("{:?}", s?);
//! }
//! # Ok::<(), sparkles_client::Error>(())
//! ```

use crate::error::{Error, Result};
use crate::{
    At, CommitList, CommitsOptions, DatasetInfo, DatasetType, Graph, QueryOptions, QuerySolution,
    RdfBody, ReadOptions, Receipt, ResponseMeta, ServerInfo, Task, UpdateOptions, UploadOptions,
    UploadPart, Whoami, WriteOptions,
};
use oxrdf::{Quad, Triple, Variable};
use serde_json::Value;
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;

fn runtime() -> Result<Arc<Runtime>> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("sparkles-client")
        .enable_all()
        .build()
        .map(Arc::new)
        .map_err(|e| Error::config(format!("starting the client's runtime: {e}")))
}

/// The blocking form of [`crate::Client`].
#[derive(Clone, Debug)]
pub struct Client {
    inner: crate::Client,
    rt: Arc<Runtime>,
}

impl Client {
    /// A client of the Sparkles server at `base_url`, without credentials.
    pub fn new(base_url: impl Into<String>) -> Result<Client> {
        Client::from_async(crate::Client::new(base_url)?)
    }

    /// The server and token the CLI's `--server` would use.
    pub fn from_env() -> Result<Client> {
        Client::from_async(crate::Client::from_env()?)
    }

    /// A blocking client over an async one, built with [`crate::ClientBuilder`].
    pub fn from_async(inner: crate::Client) -> Result<Client> {
        Ok(Client {
            inner,
            rt: runtime()?,
        })
    }

    /// The async client underneath.
    pub fn as_async(&self) -> &crate::Client {
        &self.inner
    }

    fn wait<T>(&self, f: impl Future<Output = T>) -> T {
        self.rt.block_on(f)
    }

    /// A dataset of the server.
    pub fn dataset(&self, name: impl Into<String>) -> Dataset {
        Dataset {
            endpoint: Endpoint {
                inner: self.inner.dataset(name).endpoint_owned(),
                rt: self.rt.clone(),
            },
        }
    }

    /// A plain SPARQL endpoint through this client.
    pub fn endpoint(&self, query_url: impl AsRef<str>) -> Result<Endpoint> {
        Ok(Endpoint {
            inner: self.inner.endpoint(query_url)?,
            rt: self.rt.clone(),
        })
    }

    pub fn ping(&self) -> Result<()> {
        self.wait(self.inner.ping())
    }
    pub fn server(&self) -> Result<ServerInfo> {
        self.wait(self.inner.server())
    }
    pub fn whoami(&self) -> Result<Whoami> {
        self.wait(self.inner.whoami())
    }
    pub fn datasets(&self) -> Result<Vec<DatasetInfo>> {
        self.wait(self.inner.datasets())
    }
    pub fn create_dataset(&self, name: &str, kind: DatasetType) -> Result<Dataset> {
        self.wait(self.inner.create_dataset(name, kind))?;
        Ok(self.dataset(name))
    }
    pub fn delete_dataset(&self, name: &str) -> Result<()> {
        self.wait(self.inner.delete_dataset(name))
    }
    pub fn tasks(&self) -> Result<Vec<Task>> {
        self.wait(self.inner.tasks())
    }
    pub fn task(&self, id: &str) -> Result<Task> {
        self.wait(self.inner.task(id))
    }
    pub fn cancel_task(&self, id: &str) -> Result<Task> {
        self.wait(self.inner.cancel_task(id))
    }
    pub fn wait_for_task(&self, id: &str, interval: Duration) -> Result<Task> {
        self.wait(self.inner.wait_for_task(id, interval))
    }
    pub fn backup_files(&self) -> Result<Value> {
        self.wait(self.inner.backup_files())
    }
    pub fn stats(&self) -> Result<Value> {
        self.wait(self.inner.stats())
    }
    pub fn call_json(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value> {
        self.wait(self.inner.call_json(method, path, query, body))
    }
}

/// The blocking form of [`crate::Endpoint`].
#[derive(Clone, Debug)]
pub struct Endpoint {
    inner: crate::Endpoint,
    rt: Arc<Runtime>,
}

impl Endpoint {
    /// A plain SPARQL endpoint with its own client.
    pub fn new(query_url: impl AsRef<str>) -> Result<Endpoint> {
        Ok(Endpoint {
            inner: crate::Endpoint::new(query_url)?,
            rt: runtime()?,
        })
    }

    /// A Fuseki dataset: `{base}/{name}/sparql`, `/update` and `/data`.
    pub fn fuseki(base: impl AsRef<str>, name: &str) -> Result<Endpoint> {
        Ok(Endpoint {
            inner: crate::Endpoint::fuseki(base, name)?,
            rt: runtime()?,
        })
    }

    pub fn with_update_url(self, url: impl AsRef<str>) -> Result<Endpoint> {
        Ok(Endpoint {
            inner: self.inner.with_update_url(url)?,
            rt: self.rt,
        })
    }

    pub fn with_graph_store_url(self, url: impl AsRef<str>) -> Result<Endpoint> {
        Ok(Endpoint {
            inner: self.inner.with_graph_store_url(url)?,
            rt: self.rt,
        })
    }

    /// The async endpoint underneath.
    pub fn as_async(&self) -> &crate::Endpoint {
        &self.inner
    }

    fn wait<T>(&self, f: impl Future<Output = T>) -> T {
        self.rt.block_on(f)
    }

    pub fn query(&self, query: &str) -> Result<QueryResults> {
        self.query_with(query, &QueryOptions::default())
    }
    pub fn query_with(&self, query: &str, opts: &QueryOptions) -> Result<QueryResults> {
        let r = self.wait(self.inner.query_with(query, opts))?;
        Ok(QueryResults::wrap(r, &self.rt))
    }
    pub fn select(&self, query: &str) -> Result<Solutions> {
        self.query(query)?.into_solutions()
    }
    pub fn ask(&self, query: &str) -> Result<bool> {
        self.query(query)?.into_boolean()
    }
    pub fn construct(&self, query: &str) -> Result<Triples> {
        self.query(query)?.into_graph()
    }
    pub fn construct_quads(&self, query: &str, opts: &QueryOptions) -> Result<Quads> {
        let q = self.wait(self.inner.construct_quads(query, opts))?;
        Ok(Quads {
            inner: q,
            rt: self.rt.clone(),
        })
    }
    pub fn update(&self, update: &str) -> Result<Receipt> {
        self.wait(self.inner.update(update))
    }
    pub fn update_with(&self, update: &str, opts: &UpdateOptions) -> Result<Receipt> {
        self.wait(self.inner.update_with(update, opts))
    }
    pub fn get_graph(&self, graph: impl Into<Graph>) -> Result<Triples> {
        self.get_graph_with(graph, &ReadOptions::default())
    }
    pub fn get_graph_with(&self, graph: impl Into<Graph>, opts: &ReadOptions) -> Result<Triples> {
        let t = self.wait(self.inner.get_graph_with(graph, opts))?;
        Ok(Triples {
            inner: t,
            rt: self.rt.clone(),
        })
    }
    pub fn get_dataset(&self) -> Result<Quads> {
        self.get_dataset_with(&ReadOptions::default())
    }
    pub fn get_dataset_with(&self, opts: &ReadOptions) -> Result<Quads> {
        let q = self.wait(self.inner.get_dataset_with(opts))?;
        Ok(Quads {
            inner: q,
            rt: self.rt.clone(),
        })
    }
    pub fn put_graph(&self, graph: impl Into<Graph>, body: RdfBody) -> Result<Receipt> {
        self.wait(self.inner.put_graph(graph, body))
    }
    pub fn put_graph_with(
        &self,
        graph: impl Into<Graph>,
        body: RdfBody,
        opts: &WriteOptions,
    ) -> Result<Receipt> {
        self.wait(self.inner.put_graph_with(graph, body, opts))
    }
    pub fn post_graph(&self, graph: impl Into<Graph>, body: RdfBody) -> Result<Receipt> {
        self.wait(self.inner.post_graph(graph, body))
    }
    pub fn post_graph_with(
        &self,
        graph: impl Into<Graph>,
        body: RdfBody,
        opts: &WriteOptions,
    ) -> Result<Receipt> {
        self.wait(self.inner.post_graph_with(graph, body, opts))
    }
    pub fn delete_graph(&self, graph: impl Into<Graph>) -> Result<Receipt> {
        self.wait(self.inner.delete_graph(graph))
    }
    pub fn delete_graph_with(
        &self,
        graph: impl Into<Graph>,
        opts: &WriteOptions,
    ) -> Result<Receipt> {
        self.wait(self.inner.delete_graph_with(graph, opts))
    }
    pub fn put_dataset(&self, body: RdfBody, opts: &WriteOptions) -> Result<Receipt> {
        self.wait(self.inner.put_dataset(body, opts))
    }
    pub fn post_dataset(&self, body: RdfBody, opts: &WriteOptions) -> Result<Receipt> {
        self.wait(self.inner.post_dataset(body, opts))
    }
    pub fn load(&self, path: impl AsRef<Path>) -> Result<Receipt> {
        self.wait(self.inner.load(path))
    }
    pub fn load_into(&self, graph: impl Into<Graph>, path: impl AsRef<Path>) -> Result<Receipt> {
        self.wait(self.inner.load_into(graph, path))
    }
}

/// The blocking form of [`crate::Dataset`].
#[derive(Clone, Debug)]
pub struct Dataset {
    endpoint: Endpoint,
}

impl Deref for Dataset {
    type Target = Endpoint;
    fn deref(&self) -> &Endpoint {
        &self.endpoint
    }
}

impl Dataset {
    fn ds(&self) -> crate::Dataset {
        self.endpoint.inner.client().dataset(self.name())
    }

    pub fn name(&self) -> &str {
        self.endpoint.inner.dataset.as_deref().unwrap_or_default()
    }

    /// Start a batch of updates that commit together.
    pub fn batch(&self) -> UpdateBatch {
        UpdateBatch {
            inner: self.ds().batch(),
            rt: self.rt.clone(),
        }
    }

    pub fn upload(&self, parts: Vec<UploadPart>, opts: &UploadOptions) -> Result<Receipt> {
        self.wait(self.ds().upload(parts, opts))
    }
    pub fn run_stored(&self, name: &str, params: &Value) -> Result<QueryResults> {
        self.run_stored_with(name, params, &QueryOptions::default(), None)
    }
    pub fn run_stored_with(
        &self,
        name: &str,
        params: &Value,
        opts: &QueryOptions,
        version: Option<u64>,
    ) -> Result<QueryResults> {
        let r = self.wait(self.ds().run_stored_with(name, params, opts, version))?;
        Ok(QueryResults::wrap(r, &self.rt))
    }
    pub fn stored_queries(&self) -> Result<Value> {
        self.wait(self.ds().stored_queries())
    }
    pub fn stored_query(&self, name: &str) -> Result<Value> {
        self.wait(self.ds().stored_query(name))
    }
    pub fn put_stored_query(&self, name: &str, definition: &Value) -> Result<Value> {
        self.wait(self.ds().put_stored_query(name, definition))
    }
    pub fn delete_stored_query(&self, name: &str) -> Result<()> {
        self.wait(self.ds().delete_stored_query(name))
    }
    pub fn commits(&self, opts: &CommitsOptions) -> Result<CommitList> {
        self.wait(self.ds().commits(opts))
    }
    pub fn commit(&self, reference: &str) -> Result<crate::Commit> {
        self.wait(self.ds().commit(reference))
    }
    pub fn info(&self) -> Result<DatasetInfo> {
        self.wait(self.ds().info())
    }
    pub fn stats(&self, at: Option<At>) -> Result<Value> {
        self.wait(self.ds().stats(at))
    }
    pub fn schema(&self, at: Option<At>) -> Result<Value> {
        self.wait(self.ds().schema(at))
    }
    pub fn backups(&self) -> Result<Value> {
        self.wait(self.ds().backups())
    }
    pub fn backup_nquads(&self) -> Result<Task> {
        self.wait(self.ds().backup_nquads())
    }
}

/// The blocking form of [`crate::UpdateBatch`].
#[derive(Clone, Debug)]
pub struct UpdateBatch {
    inner: crate::UpdateBatch,
    rt: Arc<Runtime>,
}

impl UpdateBatch {
    pub fn update(self, update: &str) -> Self {
        UpdateBatch {
            inner: self.inner.update(update),
            rt: self.rt,
        }
    }
    pub fn insert<'a>(self, quads: impl IntoIterator<Item = &'a Quad>) -> Self {
        UpdateBatch {
            inner: self.inner.insert(quads),
            rt: self.rt,
        }
    }
    pub fn delete<'a>(self, quads: impl IntoIterator<Item = &'a Quad>) -> Self {
        UpdateBatch {
            inner: self.inner.delete(quads),
            rt: self.rt,
        }
    }
    pub fn options(self, options: UpdateOptions) -> Self {
        UpdateBatch {
            inner: self.inner.options(options),
            rt: self.rt,
        }
    }
    pub fn to_sparql(&self) -> &str {
        self.inner.to_sparql()
    }
    pub fn commit(self) -> Result<Receipt> {
        self.rt.block_on(self.inner.commit())
    }
}

/// The blocking form of [`crate::QueryResults`].
#[derive(Debug)]
pub enum QueryResults {
    Solutions(Solutions),
    Boolean(bool, ResponseMeta),
    Graph(Triples),
}

impl QueryResults {
    fn wrap(r: crate::QueryResults, rt: &Arc<Runtime>) -> QueryResults {
        match r {
            crate::QueryResults::Solutions(s) => QueryResults::Solutions(Solutions {
                inner: s,
                rt: rt.clone(),
            }),
            crate::QueryResults::Boolean(b, m) => QueryResults::Boolean(b, m),
            crate::QueryResults::Graph(t) => QueryResults::Graph(Triples {
                inner: t,
                rt: rt.clone(),
            }),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            QueryResults::Solutions(_) => "solutions",
            QueryResults::Boolean(..) => "a boolean",
            QueryResults::Graph(_) => "a graph",
        }
    }

    pub fn meta(&self) -> &ResponseMeta {
        match self {
            QueryResults::Solutions(s) => s.meta(),
            QueryResults::Boolean(_, m) => m,
            QueryResults::Graph(t) => t.meta(),
        }
    }

    pub fn into_solutions(self) -> Result<Solutions> {
        match self {
            QueryResults::Solutions(s) => Ok(s),
            o => Err(Error::UnexpectedResults {
                expected: "solutions",
                got: o.kind(),
            }),
        }
    }

    pub fn into_boolean(self) -> Result<bool> {
        match self {
            QueryResults::Boolean(b, _) => Ok(b),
            o => Err(Error::UnexpectedResults {
                expected: "a boolean",
                got: o.kind(),
            }),
        }
    }

    pub fn into_graph(self) -> Result<Triples> {
        match self {
            QueryResults::Graph(t) => Ok(t),
            o => Err(Error::UnexpectedResults {
                expected: "a graph",
                got: o.kind(),
            }),
        }
    }
}

/// Solutions as an `Iterator`.
#[derive(Debug)]
pub struct Solutions {
    inner: crate::Solutions,
    rt: Arc<Runtime>,
}

impl Solutions {
    pub fn variables(&self) -> &[Variable] {
        self.inner.variables()
    }
    pub fn meta(&self) -> &ResponseMeta {
        self.inner.meta()
    }
}

impl Iterator for Solutions {
    type Item = Result<QuerySolution>;
    fn next(&mut self) -> Option<Self::Item> {
        self.rt.block_on(self.inner.next())
    }
}

/// Triples as an `Iterator`.
#[derive(Debug)]
pub struct Triples {
    inner: crate::Triples,
    rt: Arc<Runtime>,
}

impl Triples {
    pub fn meta(&self) -> &ResponseMeta {
        self.inner.meta()
    }
}

impl Iterator for Triples {
    type Item = Result<Triple>;
    fn next(&mut self) -> Option<Self::Item> {
        self.rt.block_on(self.inner.next())
    }
}

/// Quads as an `Iterator`.
#[derive(Debug)]
pub struct Quads {
    inner: crate::Quads,
    rt: Arc<Runtime>,
}

impl Quads {
    pub fn meta(&self) -> &ResponseMeta {
        self.inner.meta()
    }
}

impl Iterator for Quads {
    type Item = Result<Quad>;
    fn next(&mut self) -> Option<Self::Item> {
        self.rt.block_on(self.inner.next())
    }
}
