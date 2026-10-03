//! `Client`: the HTTP client, authentication, retries, deadlines and cancellation that
//! every call goes through.

use crate::body::{RdfBody, Source, UploadPart};
use crate::credentials::{self, Credentials};
use crate::error::{Error, Result, StatusError};
use crate::retry::{self, RetryPolicy};
use crate::routes::Op;
use bytes::Bytes;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, StatusCode, Url};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// A source of bearer tokens, for OIDC access tokens that expire. The client asks for a
/// token before each request (`refresh == false`) and once more with `refresh == true`
/// after a `401`, then retries the request with the new token.
pub trait TokenSource: Send + Sync + 'static {
    fn token(&self, refresh: bool) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>>;
}

#[derive(Clone)]
pub(crate) enum Auth {
    None,
    Basic(String),
    Bearer(String),
    Source(Arc<dyn TokenSource>),
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Auth::None => "None",
            Auth::Basic(_) => "Basic(…)",
            Auth::Bearer(_) => "Bearer(…)",
            Auth::Source(_) => "Source(…)",
        })
    }
}

/// The options every call takes: a deadline, a cancellation token, extra headers, and
/// whether to retry. They are part of each options type ([`QueryOptions`](crate::QueryOptions),
/// [`WriteOptions`](crate::WriteOptions), …) through its `deadline`, `cancel`, `header`
/// and `no_retry` methods.
#[derive(Clone, Debug, Default)]
pub struct CallOptions {
    pub(crate) deadline: Option<Duration>,
    pub(crate) cancel: Option<CancellationToken>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) no_retry: bool,
}

/// Adds the [`CallOptions`] builder methods to an options type with a `call` field.
macro_rules! call_options {
    ($t:ty) => {
        impl $t {
            /// A client-side limit on the whole call: the attempts, the waits between them
            /// and reading the response body.
            pub fn deadline(mut self, d: std::time::Duration) -> Self {
                self.call.deadline = Some(d);
                self
            }
            /// Ends the call, or the stream of its results, when the token is cancelled.
            pub fn cancel(mut self, token: tokio_util::sync::CancellationToken) -> Self {
                self.call.cancel = Some(token);
                self
            }
            /// An extra request header.
            pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
                self.call.headers.push((name.into(), value.into()));
                self
            }
            /// Do not retry this call, whatever the client's policy.
            pub fn no_retry(mut self) -> Self {
                self.call.no_retry = true;
                self
            }
        }
    };
}
pub(crate) use call_options;

/// A request body.
#[derive(Clone, Debug)]
pub(crate) enum ReqBody {
    None,
    Data {
        source: Source,
        content_type: String,
        encoding: Option<&'static str>,
    },
    Form(Vec<(String, String)>),
    Multipart(Vec<UploadPart>),
}

impl From<RdfBody> for ReqBody {
    fn from(b: RdfBody) -> ReqBody {
        ReqBody::Data {
            content_type: b.format.media_type().to_string(),
            source: b.source,
            encoding: b.encoding,
        }
    }
}

impl ReqBody {
    pub(crate) fn text(content_type: &str, s: impl Into<String>) -> ReqBody {
        ReqBody::Data {
            source: Source::Bytes(Bytes::from(s.into())),
            content_type: content_type.into(),
            encoding: None,
        }
    }

    pub(crate) fn json(v: &serde_json::Value) -> ReqBody {
        ReqBody::text("application/json", v.to_string())
    }
}

/// One call, before it is sent.
#[derive(Debug)]
pub(crate) struct Req {
    pub method: Method,
    pub url: Url,
    /// The Sparkles operation, which bounds the parameters and headers in debug builds.
    pub op: Option<&'static Op>,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub accept: Option<&'static str>,
    pub body: ReqBody,
    /// Reads, PUT, DELETE and queries: retried on any retryable failure.
    pub safe: bool,
    pub call: CallOptions,
}

impl Req {
    pub(crate) fn new(method: Method, url: Url) -> Req {
        let safe = matches!(
            method,
            Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::OPTIONS
        );
        Req {
            method,
            url,
            op: None,
            query: Vec::new(),
            headers: Vec::new(),
            accept: None,
            body: ReqBody::None,
            safe,
            call: CallOptions::default(),
        }
    }

    pub(crate) fn param(&mut self, k: &str, v: impl Into<String>) -> &mut Self {
        if let Some(op) = self.op {
            debug_assert!(
                op.query.contains(&k),
                "{}: query parameter {k} is not in the operation table",
                op.id
            );
        }
        self.query.push((k.to_string(), v.into()));
        self
    }

    pub(crate) fn header(&mut self, k: &str, v: impl Into<String>) -> &mut Self {
        if let Some(op) = self.op {
            debug_assert!(
                op.headers.iter().any(|h| h.eq_ignore_ascii_case(k)),
                "{}: header {k} is not in the operation table",
                op.id
            );
        }
        self.headers.push((k.to_string(), v.into()));
        self
    }

    pub(crate) fn body(&mut self, b: ReqBody) -> &mut Self {
        if let (Some(op), ReqBody::Data { content_type, .. }) = (self.op, &b) {
            debug_assert!(
                op.body.contains(&content_type.as_str()),
                "{}: body {content_type} is not in the operation table",
                op.id
            );
        }
        self.body = b;
        self
    }
}

/// A response with a success status, and what the body stream needs.
pub(crate) struct Resp {
    pub response: reqwest::Response,
    pub cancel: Option<CancellationToken>,
    pub deadline: Option<Duration>,
}

pub(crate) struct Inner {
    pub http: reqwest::Client,
    pub base: Option<Url>,
    pub auth: Auth,
    pub retry: RetryPolicy,
    pub insecure_http: bool,
    pub timeout: Option<Duration>,
}

/// A client of a Sparkles server, or of plain SPARQL endpoints. It holds the HTTP
/// connection pool, the credentials and the retry policy; clones share them.
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.inner.base.as_ref().map(Url::as_str))
            .field("auth", &self.inner.auth)
            .field("retry", &self.inner.retry)
            .finish()
    }
}

/// Builds a [`Client`].
#[derive(Default)]
pub struct ClientBuilder {
    base: Option<String>,
    auth: Option<Auth>,
    saved: bool,
    retry: Option<RetryPolicy>,
    timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    insecure_http: bool,
    user_agent: Option<String>,
    headers: Vec<(String, String)>,
    http: Option<reqwest::Client>,
}

impl ClientBuilder {
    /// A builder without a server URL, for plain endpoints only.
    pub fn new() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// The Sparkles server's base URL, such as `https://sparql.example.org` or
    /// `http://localhost:3030`. A path is kept (`https://host/sparkles`).
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base = Some(url.into());
        self
    }

    /// HTTP Basic credentials (RFC 7617). An API token works as the password.
    pub fn basic_auth(mut self, user: impl AsRef<str>, password: impl AsRef<str>) -> Self {
        use base64_lite::encode;
        let creds = encode(format!("{}:{}", user.as_ref(), password.as_ref()).as_bytes());
        self.auth = Some(Auth::Basic(format!("Basic {creds}")));
        self
    }

    /// A bearer token (RFC 6750): an API token (`spk_…`) or an OIDC access token.
    pub fn bearer_token(mut self, token: impl Into<String>) -> Self {
        self.auth = Some(Auth::Bearer(token.into()));
        self
    }

    /// Bearer tokens from a source that can refresh them after a `401`.
    pub fn token_source(mut self, source: impl TokenSource) -> Self {
        self.auth = Some(Auth::Source(Arc::new(source)));
        self
    }

    /// The token `sparkles auth login` saved for the base URL, or `SPARKLES_TOKEN` when it
    /// is set. Without either, requests go without credentials.
    pub fn saved_credentials(mut self) -> Self {
        self.saved = true;
        self
    }

    /// The retry policy (default: [`RetryPolicy::default`]).
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = Some(policy);
        self
    }

    /// A default deadline for every call (default: none, as queries may run long).
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }

    /// How long to wait for a connection (default 30 s).
    pub fn connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = Some(d);
        self
    }

    /// Allow credentials over plain http to a host other than localhost.
    pub fn allow_insecure_http(mut self) -> Self {
        self.insecure_http = true;
        self
    }

    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    /// A header sent with every request.
    pub fn default_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Use this reqwest client (its proxy, TLS and pool settings) instead of building
    /// one. The builder's timeouts, user agent and default headers then do not apply.
    pub fn http_client(mut self, http: reqwest::Client) -> Self {
        self.http = Some(http);
        self
    }

    pub fn build(self) -> Result<Client> {
        let base = match &self.base {
            Some(b) => {
                let normalized = credentials::normalize(b, true)?;
                Some(
                    Url::parse(&format!("{normalized}/"))
                        .map_err(|e| Error::config(format!("invalid URL '{b}': {e}")))?,
                )
            }
            None => None,
        };
        let mut auth = self.auth.unwrap_or(Auth::None);
        if self.saved && matches!(auth, Auth::None) {
            let creds = Credentials::load()?;
            let key = match &self.base {
                Some(b) => Some(credentials::normalize(b, true)?),
                None => None,
            };
            let token = match key {
                Some(k) => creds.token_for(&k),
                None => std::env::var("SPARKLES_TOKEN")
                    .ok()
                    .filter(|t| !t.is_empty()),
            };
            if let Some(t) = token {
                auth = Auth::Bearer(t);
            }
        }
        if let Some(b) = &base {
            check_insecure(b, &auth, self.insecure_http)?;
        }
        let http = match self.http {
            Some(h) => h,
            None => {
                let mut headers = HeaderMap::new();
                for (k, v) in &self.headers {
                    headers.insert(
                        HeaderName::from_bytes(k.as_bytes())
                            .map_err(|e| Error::config(format!("header {k}: {e}")))?,
                        HeaderValue::from_str(v)
                            .map_err(|e| Error::config(format!("header {k}: {e}")))?,
                    );
                }
                reqwest::Client::builder()
                    .connect_timeout(self.connect_timeout.unwrap_or(Duration::from_secs(30)))
                    .user_agent(self.user_agent.unwrap_or_else(|| {
                        concat!("sparkles-client/", env!("CARGO_PKG_VERSION")).into()
                    }))
                    .default_headers(headers)
                    .build()
                    .map_err(|e| Error::config(format!("building the HTTP client: {e}")))?
            }
        };
        Ok(Client {
            inner: Arc::new(Inner {
                http,
                base,
                auth,
                retry: self.retry.unwrap_or_default(),
                insecure_http: self.insecure_http,
                timeout: self.timeout,
            }),
        })
    }
}

/// Refuse credentials over plain http to a host other than loopback.
pub(crate) fn check_insecure(url: &Url, auth: &Auth, allowed: bool) -> Result<()> {
    let host = url.host_str().unwrap_or_default();
    if url.scheme() == "http"
        && !credentials::is_loopback(host)
        && !allowed
        && !matches!(auth, Auth::None)
    {
        return Err(Error::config(format!(
            "refusing to send credentials over plain http to {host} (use https, or allow_insecure_http)"
        )));
    }
    Ok(())
}

impl Client {
    /// A client of the Sparkles server at `base_url`, without credentials.
    pub fn new(base_url: impl Into<String>) -> Result<Client> {
        ClientBuilder::new().base_url(base_url).build()
    }

    /// A builder for a client of the Sparkles server at `base_url`.
    pub fn builder(base_url: impl Into<String>) -> ClientBuilder {
        ClientBuilder::new().base_url(base_url)
    }

    /// The server and token the CLI's `--server` would use: `SPARKLES_SERVER`, else the
    /// default server of the credentials file; `SPARKLES_TOKEN`, else the saved token.
    pub fn from_env() -> Result<Client> {
        let server = match std::env::var("SPARKLES_SERVER")
            .ok()
            .filter(|s| !s.is_empty())
        {
            Some(s) => s,
            None => Credentials::load()?.default_server.ok_or_else(|| {
                Error::config("no server: set SPARKLES_SERVER, or run sparkles auth login")
            })?,
        };
        ClientBuilder::new()
            .base_url(server)
            .saved_credentials()
            .build()
    }

    /// The server's base URL, with a trailing slash.
    pub fn base_url(&self) -> Option<&Url> {
        self.inner.base.as_ref()
    }

    /// The URL of a Sparkles operation with its path parameters.
    pub(crate) fn op_url(&self, op: &Op, params: &[(&str, &str)]) -> Result<Url> {
        let base = self.inner.base.as_ref().ok_or_else(|| {
            Error::config("this client has no server URL (use Client::builder(url))")
        })?;
        let path = op.fill(params).map_err(Error::Config)?;
        base.join(path.trim_start_matches('/'))
            .map_err(|e| Error::config(format!("{}: {e}", op.id)))
    }

    /// A request for a Sparkles operation.
    pub(crate) fn op_req(&self, op: &'static Op, params: &[(&str, &str)]) -> Result<Req> {
        let mut r = Req::new(op.method.clone(), self.op_url(op, params)?);
        r.op = Some(op);
        Ok(r)
    }

    async fn auth_header(&self, refresh: bool) -> Result<Option<String>> {
        Ok(match &self.inner.auth {
            Auth::None => None,
            Auth::Basic(h) => Some(h.clone()),
            Auth::Bearer(t) => Some(format!("Bearer {t}")),
            Auth::Source(s) => Some(format!("Bearer {}", s.token(refresh).await?)),
        })
    }

    async fn build(
        &self,
        r: &Req,
        auth: Option<&str>,
        remaining: Option<Duration>,
    ) -> Result<reqwest::Request> {
        let mut url = r.url.clone();
        if !r.query.is_empty() {
            url.query_pairs_mut().extend_pairs(&r.query);
        }
        let mut b = self.inner.http.request(r.method.clone(), url);
        if let Some(a) = auth {
            b = b.header(reqwest::header::AUTHORIZATION, a);
        }
        if let Some(a) = r.accept {
            b = b.header(reqwest::header::ACCEPT, a);
        }
        for (k, v) in r.headers.iter().chain(&r.call.headers) {
            b = b.header(k.as_str(), v.as_str());
        }
        if let Some(t) = remaining {
            b = b.timeout(t);
        }
        b = match &r.body {
            ReqBody::None => b,
            ReqBody::Data {
                source,
                content_type,
                encoding,
            } => {
                let mut b = b
                    .header(reqwest::header::CONTENT_TYPE, content_type.as_str())
                    .body(source.clone().into_body().await?);
                if let Some(e) = encoding {
                    b = b.header(reqwest::header::CONTENT_ENCODING, *e);
                }
                b
            }
            ReqBody::Form(pairs) => {
                let body = url::form_urlencoded_serialize(pairs);
                b.header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(body)
            }
            ReqBody::Multipart(parts) => {
                let mut form = reqwest::multipart::Form::new();
                for p in parts {
                    let field = p.field.clone();
                    form = form.part(field, p.clone().into_part().await?);
                }
                b.multipart(form)
            }
        };
        b.build().map_err(|e| Error::Transport {
            url: r.url.to_string(),
            source: e,
        })
    }

    /// Send a request, retrying as the policy and the response allow. Returns the
    /// response when its status is 2xx or 304.
    pub(crate) async fn send(&self, r: Req) -> Result<Resp> {
        let deadline = r.call.deadline.or(self.inner.timeout);
        let cancel = r.call.cancel.clone();
        let fut = self.send_inner(&r, deadline);
        let res = match &cancel {
            Some(c) => tokio::select! {
                biased;
                _ = c.cancelled() => return Err(Error::Cancelled),
                res = fut => res,
            },
            None => fut.await,
        };
        let response = res?;
        Ok(Resp {
            response,
            cancel,
            deadline,
        })
    }

    async fn send_inner(&self, r: &Req, deadline: Option<Duration>) -> Result<reqwest::Response> {
        let start = Instant::now();
        let policy = &self.inner.retry;
        let max = if r.call.no_retry {
            0
        } else {
            policy.max_retries
        };
        let mut attempt = 0u32;
        let mut refreshed = false;
        let mut auth = self.auth_header(false).await?;
        loop {
            let remaining = match deadline {
                Some(d) => {
                    let left = d.checked_sub(start.elapsed()).ok_or(Error::Deadline(d))?;
                    Some(left)
                }
                None => None,
            };
            let req = self.build(r, auth.as_deref(), remaining).await?;
            let wait = match self.inner.http.execute(req).await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() || status == StatusCode::NOT_MODIFIED {
                        return Ok(resp);
                    }
                    if status == StatusCode::UNAUTHORIZED
                        && !refreshed
                        && matches!(self.inner.auth, Auth::Source(_))
                    {
                        refreshed = true;
                        auth = self.auth_header(true).await?;
                        continue;
                    }
                    let headers = resp.headers().clone();
                    if attempt >= max || !retry::retryable_status(status, &headers, r.safe) {
                        return Err(status_error(&r.method, resp).await);
                    }
                    match retry::server_delay(&headers) {
                        Some(d) if d > policy.max_retry_after => {
                            return Err(status_error(&r.method, resp).await);
                        }
                        Some(d) => d,
                        None => policy.backoff(attempt),
                    }
                }
                Err(e) => {
                    if e.is_timeout() {
                        return Err(Error::Deadline(deadline.unwrap_or_default()));
                    }
                    // a connection that was never made carried no request; other
                    // failures may have reached the server
                    let retry = e.is_connect() || r.safe;
                    if attempt >= max || !retry {
                        return Err(Error::Transport {
                            url: r.url.to_string(),
                            source: e,
                        });
                    }
                    policy.backoff(attempt)
                }
            };
            if let Some(d) = deadline
                && start.elapsed() + wait >= d
            {
                return Err(Error::Deadline(d));
            }
            tokio::time::sleep(wait).await;
            attempt += 1;
        }
    }

    /// Call a Sparkles operation by its `operationId` and return its JSON body (`Null`
    /// when the body is empty). Only the operations of the client's operation table are
    /// known; their path parameters are given by name.
    pub async fn call_json(
        &self,
        operation_id: &str,
        path_params: &[(&str, &str)],
        query: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let op = crate::routes::ALL
            .iter()
            .find(|o| o.id == operation_id)
            .ok_or_else(|| Error::config(format!("unknown operation {operation_id}")))?;
        let mut r = self.op_req(op, path_params)?;
        for (k, v) in query {
            r.query.push((k.to_string(), v.to_string()));
        }
        if let Some(b) = body {
            r.body = ReqBody::json(b);
        }
        r.accept = Some("application/json");
        let resp = self.send(r).await?;
        read_json(resp).await
    }
}

/// The JSON body of a response, `Null` when empty.
pub(crate) async fn read_json(resp: Resp) -> Result<serde_json::Value> {
    let url = resp.response.url().to_string();
    let bytes = read_bytes(resp).await.map_err(|e| match e {
        Error::Transport { source, .. } => Error::Transport { url, source },
        e => e,
    })?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|e| Error::parse("JSON", e))
}

/// A typed JSON body.
pub(crate) async fn read_typed<T: serde::de::DeserializeOwned>(resp: Resp) -> Result<T> {
    let v = read_json(resp).await?;
    serde_json::from_value(v).map_err(|e| Error::parse("JSON", e))
}

/// The whole body, under the call's cancellation token.
pub(crate) async fn read_bytes(resp: Resp) -> Result<Bytes> {
    let url = resp.response.url().to_string();
    let deadline = resp.deadline;
    let fut = resp.response.bytes();
    let res = match &resp.cancel {
        Some(c) => tokio::select! {
            biased;
            _ = c.cancelled() => return Err(Error::Cancelled),
            r = fut => r,
        },
        None => fut.await,
    };
    res.map_err(|e| {
        if e.is_timeout() {
            Error::Deadline(deadline.unwrap_or_default())
        } else {
            Error::Transport { url, source: e }
        }
    })
}

/// The error of a non-2xx response, with the server's JSON error body when it has one.
pub(crate) async fn status_error(method: &Method, resp: reqwest::Response) -> Error {
    let status = resp.status().as_u16();
    let url = resp.url().to_string();
    let headers = resp.headers().clone();
    let text = resp.text().await.unwrap_or_default();
    let body: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    let s = |k: &str| body.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let n = |k: &str| body.get(k).and_then(serde_json::Value::as_u64);
    let message = s("error")
        .or_else(|| s("error_description"))
        .unwrap_or_else(|| {
            let t: String = text.trim().chars().take(500).collect();
            if t.is_empty() {
                StatusCode::from_u16(status)
                    .ok()
                    .and_then(|c| c.canonical_reason())
                    .unwrap_or("error")
                    .to_string()
            } else {
                t
            }
        });
    Error::Status(Box::new(StatusError {
        status,
        method: method.to_string(),
        url,
        message,
        code: s("code"),
        detail: s("detail"),
        line: n("line"),
        column: n("column"),
        request_id: s("requestId").or_else(|| {
            headers
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        }),
        retry_after: retry::retry_after(&headers),
        body,
    }))
}

/// Base64 for the Basic header, without a dependency.
mod base64_lite {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(input: &[u8]) -> String {
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[cfg(test)]
    #[test]
    fn rfc4648_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(i.as_bytes()), o);
        }
    }
}

/// `application/x-www-form-urlencoded` without a dependency.
mod url {
    pub fn form_urlencoded_serialize(pairs: &[(String, String)]) -> String {
        let mut s = reqwest::Url::parse("x:/").expect("a valid URL");
        s.query_pairs_mut().extend_pairs(pairs);
        s.query().unwrap_or_default().to_string()
    }
}
