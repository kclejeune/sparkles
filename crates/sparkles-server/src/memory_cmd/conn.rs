//! The connection of a `sparkles memory` command: a server over HTTP with the caller's
//! token, or a database directory served in the process through the same router
//! (`--loc`, spec C18 §10.2).

use super::{Common, EXIT_ERROR, EXIT_UNREACHABLE, EXIT_USAGE};
use serde_json::{Value, json};
use sparkles::store::StoreOptions;
use std::sync::Arc;
use std::time::Duration;

/// A failed command: its exit status and the JSON error of §10.1.
#[derive(Debug)]
pub struct CmdError {
    pub exit: i32,
    pub code: String,
    pub message: String,
    pub detail: Option<Value>,
}

impl CmdError {
    pub fn new(exit: i32, code: &str, message: impl Into<String>) -> CmdError {
        CmdError {
            exit,
            code: code.into(),
            message: message.into(),
            detail: None,
        }
    }

    pub fn error(code: &str, message: impl Into<String>) -> CmdError {
        CmdError::new(EXIT_ERROR, code, message)
    }

    pub fn usage(message: impl Into<String>) -> CmdError {
        CmdError::new(EXIT_USAGE, "usage", message)
    }

    pub fn with_detail(mut self, d: Value) -> CmdError {
        self.detail = Some(d);
        self
    }
}

impl From<anyhow::Error> for CmdError {
    fn from(e: anyhow::Error) -> CmdError {
        let locked = e.chain().any(|c| {
            c.downcast_ref::<sparkles::Error>()
                .is_some_and(|x| matches!(x, sparkles::Error::Locked { .. }))
        });
        CmdError::error(if locked { "locked" } else { "error" }, format!("{e:#}"))
    }
}

impl From<std::io::Error> for CmdError {
    fn from(e: std::io::Error) -> CmdError {
        CmdError::error("io", e.to_string())
    }
}

/// A response: status, headers and body.
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json(&self) -> Result<Value, CmdError> {
        serde_json::from_slice(&self.body).map_err(|e| {
            CmdError::error(
                "bad-response",
                format!("the server did not answer JSON: {e}"),
            )
        })
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The response if it is a success; otherwise the server's `{error, code}` as an
    /// error.
    pub fn check(self) -> Result<Resp, CmdError> {
        if self.ok() {
            return Ok(self);
        }
        let v: Option<Value> = serde_json::from_slice(&self.body).ok();
        let message = v
            .as_ref()
            .and_then(|j| j["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| {
                let t = self.text();
                let t = t.trim();
                if t.is_empty() {
                    format!("HTTP {}", self.status)
                } else {
                    t.chars().take(300).collect()
                }
            });
        let code = v
            .as_ref()
            .and_then(|j| j["code"].as_str().map(str::to_string))
            .unwrap_or_else(|| {
                match self.status {
                    401 => "unauthorized",
                    403 => "forbidden",
                    404 => "not-found",
                    409 => "conflict",
                    _ => "error",
                }
                .to_string()
            });
        let mut e = CmdError::error(&code, format!("{} ({})", message, self.status));
        if let Some(j) = v {
            let mut d = j.clone();
            if let Some(o) = d.as_object_mut() {
                o.remove("error");
                o.remove("code");
                if !o.is_empty() {
                    e.detail = Some(d);
                }
            }
        }
        Err(e)
    }
}

enum Kind {
    Remote {
        base: String,
        token: Option<String>,
        http: reqwest::blocking::Client,
    },
    Local {
        rt: tokio::runtime::Runtime,
        router: axum::Router,
        loc: std::path::PathBuf,
    },
}

/// A server or a local database.
pub struct Conn {
    kind: Kind,
    pub dataset: String,
}

/// Percent-encode a query parameter value.
pub fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

impl Conn {
    /// Open the connection. A remote one is probed once with a 2-second timeout, and an
    /// unreachable server is exit status 75.
    pub fn open(
        common: &Common,
        server: Option<String>,
        dataset: String,
        opts: StoreOptions,
    ) -> Result<Conn, CmdError> {
        if let Some(loc) = &common.loc {
            return Conn::local(loc, dataset, opts);
        }
        let r = crate::remote::Remote::open(server.as_deref(), common.insecure_http)
            .map_err(|e| CmdError::usage(format!("{e:#}")))?;
        let probe = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| CmdError::error("error", e.to_string()))?;
        if let Err(e) = probe.get(format!("{}/$/ping", r.base)).send() {
            return Err(CmdError::new(
                EXIT_UNREACHABLE,
                "unreachable",
                format!("cannot reach {}: {e}", r.base),
            ));
        }
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .user_agent(concat!("sparkles/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| CmdError::error("error", e.to_string()))?;
        Ok(Conn {
            kind: Kind::Remote {
                base: r.base,
                token: r.token,
                http,
            },
            dataset,
        })
    }

    fn local(loc: &std::path::Path, dataset: String, opts: StoreOptions) -> Result<Conn, CmdError> {
        if !loc.is_dir() {
            return Err(CmdError::usage(format!(
                "--loc {}: not a database directory",
                loc.display()
            )));
        }
        let st = crate::state::AppState::standalone(opts, Duration::from_secs(300));
        let st = Arc::new(st);
        st.attach(&dataset, crate::state::DbType::Persistent, Some(loc))?;
        let router = crate::http::router(st);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        Ok(Conn {
            kind: Kind::Local {
                rt,
                router,
                loc: loc.to_path_buf(),
            },
            dataset,
        })
    }

    /// The server's base URL or the database directory, for the cache's key and the
    /// configuration printed by `setup`.
    pub fn label(&self) -> String {
        match &self.kind {
            Kind::Remote { base, .. } => base.clone(),
            Kind::Local { loc, .. } => format!(
                "file://{}",
                sparkles_memory_import::project::absolute(loc).display()
            ),
        }
    }

    /// Whether requests run in this process on a database directory, so a task the
    /// server starts ends with the command.
    pub fn is_local(&self) -> bool {
        matches!(self.kind, Kind::Local { .. })
    }

    /// One request. `path` starts with `/` and carries its encoded query string.
    pub fn send(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Result<Resp, CmdError> {
        match &self.kind {
            Kind::Remote {
                base, token, http, ..
            } => {
                let m = reqwest::Method::from_bytes(method.as_bytes())
                    .map_err(|e| CmdError::error("error", e.to_string()))?;
                let mut r = http.request(m, format!("{base}{path}"));
                if let Some(t) = token {
                    r = r.bearer_auth(t);
                }
                for (k, v) in headers {
                    r = r.header(*k, *v);
                }
                if !body.is_empty() {
                    r = r.body(body);
                }
                let resp = r.send().map_err(|e| {
                    if e.is_connect() || e.is_timeout() {
                        CmdError::new(
                            EXIT_UNREACHABLE,
                            "unreachable",
                            format!("cannot reach {base}: {e}"),
                        )
                    } else {
                        CmdError::error("error", format!("{base}: {e}"))
                    }
                })?;
                let status = resp.status().as_u16();
                let hs = resp
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                    .collect();
                let body = resp
                    .bytes()
                    .map_err(|e| CmdError::error("error", e.to_string()))?
                    .to_vec();
                Ok(Resp {
                    status,
                    headers: hs,
                    body,
                })
            }
            Kind::Local { rt, router, .. } => {
                use tower::ServiceExt;
                let mut b = axum::http::Request::builder().method(method).uri(path);
                for (k, v) in headers {
                    b = b.header(*k, *v);
                }
                let req = b
                    .body(axum::body::Body::from(body))
                    .map_err(|e| CmdError::error("error", e.to_string()))?;
                let router = router.clone();
                rt.block_on(async move {
                    let resp = router
                        .oneshot(req)
                        .await
                        .map_err(|e| CmdError::error("error", e.to_string()))?;
                    let status = resp.status().as_u16();
                    let hs = resp
                        .headers()
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                        .collect();
                    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
                        .await
                        .map_err(|e| CmdError::error("error", e.to_string()))?
                        .to_vec();
                    Ok(Resp {
                        status,
                        headers: hs,
                        body,
                    })
                })
            }
        }
    }

    pub fn get_json(&self, path: &str) -> Result<Value, CmdError> {
        self.send("GET", path, &[("accept", "application/json")], Vec::new())?
            .check()?
            .json()
    }

    pub fn post_json(&self, path: &str, body: &Value) -> Result<Resp, CmdError> {
        self.send(
            "POST",
            path,
            &[
                ("content-type", "application/json"),
                ("accept", "application/json"),
            ],
            serde_json::to_vec(body).unwrap_or_default(),
        )
    }

    pub fn put_json(&self, path: &str, body: &Value) -> Result<Value, CmdError> {
        let r = self
            .send(
                "PUT",
                path,
                &[
                    ("content-type", "application/json"),
                    ("accept", "application/json"),
                ],
                serde_json::to_vec(body).unwrap_or_default(),
            )?
            .check()?;
        Ok(if r.body.is_empty() {
            Value::Null
        } else {
            r.json().unwrap_or(Value::Null)
        })
    }

    /// A SPARQL query of the dataset with JSON results.
    pub fn select(&self, q: &str) -> Result<Sel, CmdError> {
        let r = self
            .send(
                "POST",
                &format!("/{}/sparql", enc(&self.dataset)),
                &[
                    ("content-type", "application/sparql-query"),
                    ("accept", "application/sparql-results+json"),
                ],
                q.as_bytes().to_vec(),
            )?
            .check()?;
        let head = r
            .header("sparkles-commit")
            .and_then(|h| h.parse::<u64>().ok());
        let dataset_id = r.header("sparkles-dataset-id").map(str::to_string);
        let j = r.json()?;
        let rows = j["results"]["bindings"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(Sel {
            rows,
            head,
            dataset_id,
        })
    }

    /// `GET /$/whoami`: the principal's kind and the name it acts as: a minted
    /// token's owner, a configured token's name without `cfg-`, else its name.
    pub fn whoami(&self) -> Result<(String, Option<String>), CmdError> {
        let j = self.get_json("/$/whoami")?;
        let p = &j["principal"];
        let kind = p["kind"].as_str().unwrap_or("").to_string();
        let name = match (kind.as_str(), p["owner"].as_str(), p["name"].as_str()) {
            ("token", Some(o), _) => Some(o.split_once(':').map_or(o, |(_, n)| n).to_string()),
            ("token", None, Some(n)) => Some(n.strip_prefix("cfg-").unwrap_or(n).to_string()),
            (_, _, n) => n.map(str::to_string),
        };
        Ok((kind, name))
    }

    /// `GET /$/memory/{ds}`.
    pub fn memory_settings(&self) -> Result<Value, CmdError> {
        self.get_json(&format!("/$/memory/{}", enc(&self.dataset)))
    }

    /// A tool route of `/{ds}/…` with JSON in and out.
    pub fn tool(&self, route: &str, args: &Value) -> Result<Value, CmdError> {
        self.post_json(&format!("/{}/{route}", enc(&self.dataset)), args)?
            .check()?
            .json()
    }
}

/// The rows of a SELECT, with the commit and dataset id it read.
pub struct Sel {
    pub rows: Vec<Value>,
    pub head: Option<u64>,
    pub dataset_id: Option<String>,
}

/// A binding's lexical value.
pub fn val(row: &Value, var: &str) -> Option<String> {
    row[var]["value"].as_str().map(str::to_string)
}

/// A binding as an import object.
pub fn obj(row: &Value, var: &str) -> Option<sparkles_memory_import::Obj> {
    use sparkles_memory_import::Obj;
    let b = &row[var];
    let v = b["value"].as_str()?.to_string();
    match b["type"].as_str()? {
        "uri" => Some(Obj::Iri(v)),
        "literal" | "typed-literal" => {
            let dt = b["datatype"]
                .as_str()
                .filter(|d| *d != "http://www.w3.org/2001/XMLSchema#string")
                .map(str::to_string);
            Some(Obj::Literal {
                value: v,
                datatype: dt,
            })
        }
        _ => None,
    }
}

/// `{error, code}` for JSON output of a partial failure.
pub fn err_json(e: &CmdError) -> Value {
    let mut j = json!({ "error": e.message, "code": e.code });
    if let Some(d) = &e.detail {
        j["detail"] = d.clone();
    }
    j
}
