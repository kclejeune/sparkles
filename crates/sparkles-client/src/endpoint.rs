//! `Endpoint`: the SPARQL 1.1 Protocol and the Graph Store HTTP Protocol against any
//! server, and against a Sparkles dataset with its parameters and receipts.

use crate::body::RdfBody;
use crate::client::{
    CallOptions, Client, ClientBuilder, Req, ReqBody, Resp, call_options, read_bytes,
};
use crate::error::{Error, Result};
use crate::results::{Quads, QueryResults, ResponseMeta, Solutions, Triples};
use crate::routes::{self, Op};
use crate::types::{At, Graph, Receipt, ReceiptBody};
#[cfg(test)]
use reqwest::Method;
use reqwest::Url;
use std::path::Path;
use std::time::Duration;

/// The longest URL sent as a GET; a longer query goes as a POST form.
const MAX_GET_URL: usize = 2000;

/// What a query asks for: SPARQL results first, then graphs. CSV is never asked for,
/// because it cannot be read back into terms.
const QUERY_ACCEPT: &str = "application/sparql-results+json, application/sparql-results+xml;q=0.9, \
     text/tab-separated-values;q=0.8, application/n-triples;q=0.7, text/turtle;q=0.6, \
     application/rdf+xml;q=0.3, application/ld+json;q=0.2";
const QUADS_ACCEPT: &str = "application/n-quads, application/trig;q=0.9";
const GRAPH_ACCEPT: &str = "application/n-triples, text/turtle;q=0.9, application/rdf+xml;q=0.5, \
     application/ld+json;q=0.4";
const DATASET_ACCEPT: &str = "application/n-quads, application/trig;q=0.9";

/// How a query is sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QueryMethod {
    /// GET when the URL stays under 2,000 bytes, else a POST form.
    #[default]
    Auto,
    Get,
    /// A POST form (`application/x-www-form-urlencoded`).
    Post,
}

/// Options of a query.
#[derive(Clone, Debug, Default)]
pub struct QueryOptions {
    pub(crate) at: Option<At>,
    pub(crate) server_timeout: Option<Duration>,
    pub(crate) reasoning: Option<bool>,
    pub(crate) default_graphs: Vec<String>,
    pub(crate) named_graphs: Vec<String>,
    pub(crate) method: QueryMethod,
    pub(crate) call: CallOptions,
}

impl QueryOptions {
    pub fn new() -> Self {
        Self::default()
    }
    /// Read a past state (Sparkles only).
    pub fn at(mut self, at: impl Into<At>) -> Self {
        self.at = Some(at.into());
        self
    }
    /// The server's `timeout` parameter: how long the server may work on the query.
    pub fn server_timeout(mut self, d: Duration) -> Self {
        self.server_timeout = Some(d);
        self
    }
    /// Include (`true`) or exclude materialized inferences (Sparkles only).
    pub fn reasoning(mut self, on: bool) -> Self {
        self.reasoning = Some(on);
        self
    }
    /// The protocol's `default-graph-uri` (repeatable).
    pub fn default_graph(mut self, iri: impl Into<String>) -> Self {
        self.default_graphs.push(iri.into());
        self
    }
    /// The protocol's `named-graph-uri` (repeatable).
    pub fn named_graph(mut self, iri: impl Into<String>) -> Self {
        self.named_graphs.push(iri.into());
        self
    }
    pub fn method(mut self, m: QueryMethod) -> Self {
        self.method = m;
        self
    }
}
call_options!(QueryOptions);

/// Options of an update.
#[derive(Clone, Debug, Default)]
pub struct UpdateOptions {
    pub(crate) using_graphs: Vec<String>,
    pub(crate) using_named_graphs: Vec<String>,
    pub(crate) write: WriteCommon,
    pub(crate) call: CallOptions,
}

/// The Sparkles parameters of every write.
#[derive(Clone, Debug, Default)]
pub(crate) struct WriteCommon {
    pub message: Option<String>,
    pub dry_run: bool,
    pub validate: Option<bool>,
    pub server_timeout: Option<Duration>,
}

impl UpdateOptions {
    pub fn new() -> Self {
        Self::default()
    }
    /// The protocol's `using-graph-uri` (repeatable).
    pub fn using_graph(mut self, iri: impl Into<String>) -> Self {
        self.using_graphs.push(iri.into());
        self
    }
    /// The protocol's `using-named-graph-uri` (repeatable).
    pub fn using_named_graph(mut self, iri: impl Into<String>) -> Self {
        self.using_named_graphs.push(iri.into());
        self
    }
    /// The commit message (Sparkles only).
    pub fn message(mut self, m: impl Into<String>) -> Self {
        self.write.message = Some(m.into());
        self
    }
    /// Run the update as a dry run and report what it would commit (Sparkles only).
    pub fn dry_run(mut self) -> Self {
        self.write.dry_run = true;
        self
    }
    /// `validate=false` skips write-time validation where the caller may (Sparkles only).
    pub fn validate(mut self, on: bool) -> Self {
        self.write.validate = Some(on);
        self
    }
    /// The server's `timeout` parameter.
    pub fn server_timeout(mut self, d: Duration) -> Self {
        self.write.server_timeout = Some(d);
        self
    }
}
call_options!(UpdateOptions);

/// Options of a Graph Store write.
#[derive(Clone, Debug, Default)]
pub struct WriteOptions {
    pub(crate) if_match: Option<String>,
    pub(crate) if_none_match: Option<String>,
    pub(crate) write: WriteCommon,
    pub(crate) call: CallOptions,
}

impl WriteOptions {
    pub fn new() -> Self {
        Self::default()
    }
    /// Write only if the target is at this entity tag (`*`: only if it exists).
    pub fn if_match(mut self, etag: impl Into<String>) -> Self {
        self.if_match = Some(etag.into());
        self
    }
    /// Write only if the target is not at this entity tag (`*`: only if it is absent).
    pub fn if_none_match(mut self, etag: impl Into<String>) -> Self {
        self.if_none_match = Some(etag.into());
        self
    }
    /// The commit message (Sparkles only).
    pub fn message(mut self, m: impl Into<String>) -> Self {
        self.write.message = Some(m.into());
        self
    }
    /// A dry run (Sparkles only).
    pub fn dry_run(mut self) -> Self {
        self.write.dry_run = true;
        self
    }
    /// `validate=false` skips write-time validation where the caller may (Sparkles only).
    pub fn validate(mut self, on: bool) -> Self {
        self.write.validate = Some(on);
        self
    }
    /// The server's `timeout` parameter.
    pub fn server_timeout(mut self, d: Duration) -> Self {
        self.write.server_timeout = Some(d);
        self
    }
}
call_options!(WriteOptions);

/// Options of a Graph Store read.
#[derive(Clone, Debug, Default)]
pub struct ReadOptions {
    pub(crate) at: Option<At>,
    pub(crate) if_none_match: Option<String>,
    pub(crate) reasoning: Option<bool>,
    pub(crate) call: CallOptions,
}

impl ReadOptions {
    pub fn new() -> Self {
        Self::default()
    }
    /// Read a past state (Sparkles only).
    pub fn at(mut self, at: impl Into<At>) -> Self {
        self.at = Some(at.into());
        self
    }
    /// Answer `304` with an empty stream when the entity tag still matches.
    pub fn if_none_match(mut self, etag: impl Into<String>) -> Self {
        self.if_none_match = Some(etag.into());
        self
    }
    /// Include or exclude materialized inferences (Sparkles only).
    pub fn reasoning(mut self, on: bool) -> Self {
        self.reasoning = Some(on);
        self
    }
}
call_options!(ReadOptions);

/// A SPARQL endpoint: a query URL, and optionally an update URL and a Graph Store URL.
/// [`Dataset`](crate::Dataset) dereferences to the endpoint of a Sparkles dataset, which
/// also sends Sparkles' own parameters.
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub(crate) client: Client,
    pub(crate) query_url: Url,
    pub(crate) update_url: Option<Url>,
    pub(crate) gsp_url: Option<Url>,
    /// The Sparkles dataset's name, when this is a dataset's endpoint.
    pub(crate) dataset: Option<String>,
}

fn parse_url(u: &str) -> Result<Url> {
    let url = Url::parse(u).map_err(|e| Error::config(format!("'{u}' is not a URL: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(Error::config(format!("'{u}' is not an http(s) URL")));
    }
    Ok(url)
}

impl Endpoint {
    /// A plain SPARQL endpoint with its own default client, such as
    /// `https://query.wikidata.org/sparql`.
    pub fn new(query_url: impl AsRef<str>) -> Result<Endpoint> {
        ClientBuilder::new().build()?.endpoint(query_url)
    }

    /// A Fuseki dataset: `{base}/{name}/sparql`, `/update` and `/data`.
    pub fn fuseki(base: impl AsRef<str>, name: &str) -> Result<Endpoint> {
        let b = base.as_ref().trim_end_matches('/');
        Endpoint::new(format!("{b}/{name}/sparql"))?
            .with_update_url(format!("{b}/{name}/update"))?
            .with_graph_store_url(format!("{b}/{name}/data"))
    }

    /// The URL updates are sent to.
    pub fn with_update_url(mut self, url: impl AsRef<str>) -> Result<Endpoint> {
        let u = parse_url(url.as_ref())?;
        self.client.check_endpoint(&u)?;
        self.update_url = Some(u);
        Ok(self)
    }

    /// The Graph Store Protocol URL (`?default` and `?graph=` are added to it).
    pub fn with_graph_store_url(mut self, url: impl AsRef<str>) -> Result<Endpoint> {
        let u = parse_url(url.as_ref())?;
        self.client.check_endpoint(&u)?;
        self.gsp_url = Some(u);
        Ok(self)
    }

    /// The query URL.
    pub fn query_url(&self) -> &Url {
        &self.query_url
    }

    /// The client the endpoint sends through.
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn sparkles(&self) -> Option<&str> {
        self.dataset.as_deref()
    }

    /// A request to `url`, or for a Sparkles dataset to the operation `op`.
    fn req(&self, op: &'static Op, url: &Url) -> Result<Req> {
        match self.sparkles() {
            Some(ds) => self.client.op_req(op, &[("ds", ds)]),
            None => Ok(Req::new(op.method.clone(), url.clone())),
        }
    }

    fn sparkles_only(&self, what: &str) -> Result<()> {
        if self.sparkles().is_none() {
            return Err(Error::config(format!(
                "{what} is a Sparkles option; {} is a plain SPARQL endpoint",
                self.query_url
            )));
        }
        Ok(())
    }

    // ---------------------------------------------------------------- queries ----

    /// Run a query. The form of the result comes from the response's media type.
    pub async fn query(&self, query: &str) -> Result<QueryResults> {
        self.query_with(query, &QueryOptions::default()).await
    }

    /// Run a query with options.
    pub async fn query_with(&self, query: &str, opts: &QueryOptions) -> Result<QueryResults> {
        let r = self.query_req(query, opts, QUERY_ACCEPT)?;
        let resp = self.client.send(r).await?;
        QueryResults::from_response(resp).await
    }

    /// Run a SELECT query (Jena's `querySelect`).
    pub async fn select(&self, query: &str) -> Result<Solutions> {
        self.query(query).await?.into_solutions()
    }

    /// Run an ASK query (Jena's `queryAsk`).
    pub async fn ask(&self, query: &str) -> Result<bool> {
        self.query(query).await?.into_boolean()
    }

    /// Run a CONSTRUCT or DESCRIBE query (Jena's `queryConstruct`).
    pub async fn construct(&self, query: &str) -> Result<Triples> {
        self.query(query).await?.into_graph()
    }

    /// Run a CONSTRUCT query whose template has `GRAPH` blocks, as quads.
    pub async fn construct_quads(&self, query: &str, opts: &QueryOptions) -> Result<Quads> {
        let r = self.query_req(query, opts, QUADS_ACCEPT)?;
        Quads::from_response(self.client.send(r).await?)
    }

    fn query_req(&self, query: &str, opts: &QueryOptions, accept: &'static str) -> Result<Req> {
        // the protocol's parameters, and Sparkles' (in the URL either way)
        let mut protocol: Vec<(&str, String)> = vec![("query", query.to_string())];
        protocol.extend(
            opts.default_graphs
                .iter()
                .map(|g| ("default-graph-uri", g.clone())),
        );
        protocol.extend(
            opts.named_graphs
                .iter()
                .map(|g| ("named-graph-uri", g.clone())),
        );
        let mut extra: Vec<(&str, String)> = Vec::new();
        if let Some(at) = &opts.at {
            self.sparkles_only("at")?;
            extra.push(("at", at.to_string()));
        }
        if let Some(r) = opts.reasoning {
            self.sparkles_only("reasoning")?;
            extra.push(("reasoning", r.to_string()));
        }
        if let Some(t) = opts.server_timeout {
            extra.push(("timeout", secs(t)));
        }
        let mut get_url = self.query_url.clone();
        {
            let mut q = get_url.query_pairs_mut();
            for (k, v) in protocol.iter().chain(&extra) {
                q.append_pair(k, v);
            }
        }
        let use_get = match opts.method {
            QueryMethod::Get => true,
            QueryMethod::Post => false,
            QueryMethod::Auto => get_url.as_str().len() <= MAX_GET_URL,
        };
        let mut r = if use_get {
            let mut r = self.req(&routes::SPARQL_GET, &self.query_url)?;
            for (k, v) in protocol.iter().chain(&extra) {
                r.param(k, v.clone());
            }
            r
        } else {
            let mut r = self.req(&routes::SPARQL_POST, &self.query_url)?;
            for (k, v) in &extra {
                r.param(k, v.clone());
            }
            r.body = ReqBody::Form(
                protocol
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            );
            r
        };
        // a query changes nothing, so it may be retried even when sent by POST
        r.safe = true;
        r.accept = Some(accept);
        r.call = opts.call.clone();
        Ok(r)
    }

    // ---------------------------------------------------------------- updates ----

    /// Run an update. On a Sparkles dataset the receipt names the commit.
    pub async fn update(&self, update: &str) -> Result<Receipt> {
        self.update_with(update, &UpdateOptions::default()).await
    }

    /// Run an update with options.
    pub async fn update_with(&self, update: &str, opts: &UpdateOptions) -> Result<Receipt> {
        let url = self
            .update_url
            .as_ref()
            .ok_or_else(|| Error::config(format!("{} has no update URL", self.query_url)))?;
        let mut r = self.req(&routes::UPDATE, url)?;
        for g in &opts.using_graphs {
            r.param("using-graph-uri", g.clone());
        }
        for g in &opts.using_named_graphs {
            r.param("using-named-graph-uri", g.clone());
        }
        self.write_params(&mut r, &opts.write)?;
        r.body(ReqBody::text("application/sparql-update", update));
        r.accept = Some("application/json, */*;q=0.5");
        r.call = opts.call.clone();
        receipt(self.client.send(r).await?).await
    }

    fn write_params(&self, r: &mut Req, w: &WriteCommon) -> Result<()> {
        if let Some(t) = w.server_timeout {
            r.param("timeout", secs(t));
        }
        if self.sparkles().is_some() {
            r.param("receipt", "true");
        }
        if w.dry_run {
            self.sparkles_only("dry_run")?;
            r.param("dryRun", "true");
        }
        if let Some(v) = w.validate {
            self.sparkles_only("validate")?;
            r.param("validate", v.to_string());
        }
        if let Some(m) = &w.message {
            self.sparkles_only("message")?;
            r.header("Sparkles-Commit-Message", message_header(m));
        }
        Ok(())
    }

    // ------------------------------------------------------------ graph store ----

    fn gsp_url(&self) -> Result<&Url> {
        self.gsp_url
            .as_ref()
            .ok_or_else(|| Error::config(format!("{} has no Graph Store URL", self.query_url)))
    }

    fn gsp_req(&self, op: &'static Op, graph: Option<&Graph>) -> Result<Req> {
        let url = self.gsp_url()?.clone();
        let mut r = self.req(op, &url)?;
        if let Some(g) = graph {
            let (k, v) = g.param();
            r.param(k, v);
        }
        Ok(r)
    }

    fn read_req(
        &self,
        graph: Option<&Graph>,
        opts: &ReadOptions,
        accept: &'static str,
    ) -> Result<Req> {
        let mut r = self.gsp_req(&routes::GSP_GET, graph)?;
        if let Some(at) = &opts.at {
            self.sparkles_only("at")?;
            r.param("at", at.to_string());
        }
        if let Some(on) = opts.reasoning {
            self.sparkles_only("reasoning")?;
            r.param("reasoning", on.to_string());
        }
        if let Some(t) = &opts.if_none_match {
            r.header("If-None-Match", t.clone());
        }
        r.accept = Some(accept);
        r.call = opts.call.clone();
        Ok(r)
    }

    /// Read one graph (Jena's `fetch`).
    pub async fn get_graph(&self, graph: impl Into<Graph>) -> Result<Triples> {
        self.get_graph_with(graph, &ReadOptions::default()).await
    }

    /// Read one graph with options.
    pub async fn get_graph_with(
        &self,
        graph: impl Into<Graph>,
        opts: &ReadOptions,
    ) -> Result<Triples> {
        let r = self.read_req(Some(&graph.into()), opts, GRAPH_ACCEPT)?;
        Triples::from_response(self.client.send(r).await?)
    }

    /// Read the whole dataset as quads (Jena's `fetchDataset`).
    pub async fn get_dataset(&self) -> Result<Quads> {
        self.get_dataset_with(&ReadOptions::default()).await
    }

    /// Read the whole dataset with options.
    pub async fn get_dataset_with(&self, opts: &ReadOptions) -> Result<Quads> {
        let r = self.read_req(None, opts, DATASET_ACCEPT)?;
        Quads::from_response(self.client.send(r).await?)
    }

    async fn write(
        &self,
        op: &'static Op,
        graph: Option<&Graph>,
        body: Option<RdfBody>,
        opts: &WriteOptions,
    ) -> Result<Receipt> {
        let mut r = self.gsp_req(op, graph)?;
        self.write_params(&mut r, &opts.write)?;
        if let Some(t) = &opts.if_match {
            r.header("If-Match", t.clone());
        }
        if let Some(t) = &opts.if_none_match {
            r.header("If-None-Match", t.clone());
        }
        if let Some(b) = body {
            r.body(b.into());
        }
        r.accept = Some("application/json, */*;q=0.5");
        r.call = opts.call.clone();
        receipt(self.client.send(r).await?).await
    }

    /// Replace a graph's content (Jena's `put`).
    pub async fn put_graph(&self, graph: impl Into<Graph>, body: RdfBody) -> Result<Receipt> {
        self.put_graph_with(graph, body, &WriteOptions::default())
            .await
    }

    pub async fn put_graph_with(
        &self,
        graph: impl Into<Graph>,
        body: RdfBody,
        opts: &WriteOptions,
    ) -> Result<Receipt> {
        self.write(&routes::GSP_PUT, Some(&graph.into()), Some(body), opts)
            .await
    }

    /// Add to a graph (Jena's `load` of a graph).
    pub async fn post_graph(&self, graph: impl Into<Graph>, body: RdfBody) -> Result<Receipt> {
        self.post_graph_with(graph, body, &WriteOptions::default())
            .await
    }

    pub async fn post_graph_with(
        &self,
        graph: impl Into<Graph>,
        body: RdfBody,
        opts: &WriteOptions,
    ) -> Result<Receipt> {
        self.write(&routes::GSP_POST, Some(&graph.into()), Some(body), opts)
            .await
    }

    /// Remove a graph (Jena's `delete`).
    pub async fn delete_graph(&self, graph: impl Into<Graph>) -> Result<Receipt> {
        self.delete_graph_with(graph, &WriteOptions::default())
            .await
    }

    pub async fn delete_graph_with(
        &self,
        graph: impl Into<Graph>,
        opts: &WriteOptions,
    ) -> Result<Receipt> {
        self.write(&routes::GSP_DELETE, Some(&graph.into()), None, opts)
            .await
    }

    /// Replace the whole dataset with quads (Jena's `putDataset`).
    pub async fn put_dataset(&self, body: RdfBody, opts: &WriteOptions) -> Result<Receipt> {
        self.write(&routes::GSP_PUT, None, Some(body), opts).await
    }

    /// Add quads to the dataset (Jena's `loadDataset`).
    pub async fn post_dataset(&self, body: RdfBody, opts: &WriteOptions) -> Result<Receipt> {
        self.write(&routes::GSP_POST, None, Some(body), opts).await
    }

    /// Load a file (Jena's `load`): a triple syntax into the default graph, a quad syntax
    /// into the dataset. The syntax and compression come from the file name.
    pub async fn load(&self, path: impl AsRef<Path>) -> Result<Receipt> {
        let body = RdfBody::file(path)?;
        if body.format.supports_datasets() {
            self.post_dataset(body, &WriteOptions::default()).await
        } else {
            self.post_graph(Graph::Default, body).await
        }
    }

    /// Load a file of triples into a named graph.
    pub async fn load_into(
        &self,
        graph: impl Into<Graph>,
        path: impl AsRef<Path>,
    ) -> Result<Receipt> {
        self.post_graph(graph, RdfBody::file(path)?).await
    }
}

impl Client {
    /// A plain SPARQL endpoint that shares this client's connections, credentials and
    /// retry policy.
    pub fn endpoint(&self, query_url: impl AsRef<str>) -> Result<Endpoint> {
        let u = parse_url(query_url.as_ref())?;
        self.check_endpoint(&u)?;
        Ok(Endpoint {
            client: self.clone(),
            query_url: u,
            update_url: None,
            gsp_url: None,
            dataset: None,
        })
    }

    pub(crate) fn check_endpoint(&self, u: &Url) -> Result<()> {
        crate::client::check_insecure(u, &self.inner.auth, self.inner.insecure_http)
    }
}

/// Seconds as the server's `timeout` parameter reads them.
fn secs(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s.fract() == 0.0 {
        format!("{}", s as u64)
    } else {
        format!("{s}")
    }
}

/// The `Sparkles-Commit-Message` value: as it is when it is ASCII, else an RFC 8187
/// extended value (`UTF-8''…`), as the CLI sends it.
pub(crate) fn message_header(m: &str) -> String {
    let ext = m
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("utf-8''"));
    if m.is_ascii() && !ext && !m.contains(['\r', '\n']) {
        m.to_string()
    } else {
        format!(
            "UTF-8''{}",
            percent_encoding::utf8_percent_encode(m, percent_encoding::NON_ALPHANUMERIC)
        )
    }
}

/// The receipt of a write response.
pub(crate) async fn receipt(resp: Resp) -> Result<Receipt> {
    let meta = ResponseMeta::of(&resp.response);
    let dry_run = resp
        .response
        .headers()
        .get("sparkles-dry-run")
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"true"));
    let is_json = meta
        .content_type
        .as_deref()
        .is_some_and(|c| c.ends_with("json"));
    let bytes = read_bytes(resp).await?;
    let body: serde_json::Value = if is_json {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    };
    let parsed: Option<ReceiptBody> = serde_json::from_value(body.clone()).ok();
    let dry_commit = if dry_run {
        body.get("commit")
            .and_then(|c| serde_json::from_value(c.clone()).ok())
    } else {
        None
    };
    Ok(Receipt {
        status: meta.status,
        commit_seq: meta.commit,
        dataset_id: meta.dataset_id,
        committed: parsed
            .as_ref()
            .map(|p| p.committed)
            .or(dry_run.then_some(false)),
        commit: parsed.map(|p| p.commit).or(dry_commit),
        dry_run,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ascii_messages_travel_as_extended_values() {
        assert_eq!(message_header("fix labels"), "fix labels");
        assert_eq!(message_header("café"), "UTF-8''caf%C3%A9");
        assert_eq!(message_header("utf-8''x"), "UTF-8''utf%2D8%27%27x");
    }

    #[test]
    fn timeouts_in_seconds() {
        assert_eq!(secs(Duration::from_secs(30)), "30");
        assert_eq!(secs(Duration::from_millis(1500)), "1.5");
    }

    #[test]
    fn sparkles_options_are_refused_on_plain_endpoints() {
        let ep = Endpoint::new("https://query.wikidata.org/sparql").unwrap();
        let e = ep
            .query_req("ASK {}", &QueryOptions::new().at(3), QUERY_ACCEPT)
            .unwrap_err();
        assert!(e.to_string().contains("Sparkles option"), "{e}");
        let r = ep
            .query_req(
                "ASK {}",
                &QueryOptions::new().default_graph("http://g"),
                QUERY_ACCEPT,
            )
            .unwrap();
        assert_eq!(r.method, Method::GET);
        assert!(r.query.iter().any(|(k, _)| k == "default-graph-uri"));
        let long = "#".repeat(3000);
        let r = ep
            .query_req(&long, &QueryOptions::new(), QUERY_ACCEPT)
            .unwrap();
        assert_eq!(r.method, Method::POST);
        assert!(r.query.is_empty());
    }

    #[test]
    fn credentials_never_go_over_plain_http() {
        let c = ClientBuilder::new().bearer_token("t").build().unwrap();
        assert!(c.endpoint("http://example.org/sparql").is_err());
        assert!(c.endpoint("http://localhost:3030/ds/sparql").is_ok());
        assert!(c.endpoint("https://example.org/sparql").is_ok());
        let c = ClientBuilder::new()
            .bearer_token("t")
            .allow_insecure_http()
            .build()
            .unwrap();
        assert!(c.endpoint("http://example.org/sparql").is_ok());
        assert!(
            Client::builder("http://example.org")
                .bearer_token("t")
                .build()
                .is_err()
        );
        assert!(Client::new("http://example.org").is_ok());
    }
}
