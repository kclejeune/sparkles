//! The command line as a client of a remote server: `sparkles auth login|logout|status|
//! token …` and `query`/`update`/`load --server URL`.

use anyhow::{Context, Result, bail};
use serde_json::Value as J;

pub mod client;
pub mod credentials;
pub mod login;

/// `scheme://host[:port][/path]`, lowercase host, no trailing slash: the key of the
/// credentials file, normalized by the Rust client's rules so that both find the same
/// login. Plain `http` is refused for hosts other than loopback unless `insecure` (tokens
/// are bearer secrets).
pub fn normalize(url: &str, insecure: bool) -> Result<String> {
    let s = sparkles_client::credentials::normalize(url, true)?;
    let u = reqwest::Url::parse(&s).with_context(|| format!("invalid URL '{url}'"))?;
    let host = u.host_str().unwrap_or_default();
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]");
    if u.scheme() == "http" && !loopback && !insecure {
        bail!(
            "refusing plain http to {host}: tokens would travel in clear text (use https, or --insecure-http)"
        );
    }
    Ok(s)
}

/// A server, the token to use with it, and an HTTP client.
pub struct Remote {
    pub base: String,
    pub token: Option<String>,
    pub http: reqwest::blocking::Client,
}

impl Remote {
    /// The server from `--server` (or `SPARKLES_SERVER`), else the saved default; the
    /// token from `SPARKLES_TOKEN`, else the credentials file.
    pub fn open(server: Option<&str>, insecure: bool) -> Result<Remote> {
        let creds = credentials::load()?;
        let raw = match server {
            Some(s) => s.to_string(),
            None => creds
                .default_server
                .clone()
                .context("no server given (use --server URL, or sparkles auth login first)")?,
        };
        let base = normalize(&raw, insecure)?;
        let token = std::env::var("SPARKLES_TOKEN")
            .ok()
            .filter(|t| !t.is_empty())
            .or_else(|| creds.servers.get(&base).map(|c| c.token.clone()));
        Ok(Remote {
            base,
            token,
            http: client()?,
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub fn req(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let r = self.http.request(method, self.url(path));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    /// The response if it is a success; otherwise an error with the server's message.
    pub fn check(
        &self,
        r: reqwest::Result<reqwest::blocking::Response>,
        dataset: Option<&str>,
    ) -> Result<reqwest::blocking::Response> {
        let r = r.with_context(|| format!("cannot reach {}", self.base))?;
        let status = r.status();
        if status.is_success() {
            return Ok(r);
        }
        let body = r.text().unwrap_or_default();
        let msg = serde_json::from_str::<J>(&body)
            .ok()
            .and_then(|j| j["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| body.trim().chars().take(300).collect());
        match (status.as_u16(), dataset) {
            (401, _) => bail!(
                "not logged in to {} (run: sparkles auth login --server {})",
                self.base,
                self.base
            ),
            (404, Some(ds)) => bail!("no such dataset: /{ds} (or no access)"),
            _ => bail!("{status}: {msg}"),
        }
    }

    pub fn get_json(&self, path: &str) -> Result<J> {
        let r = self.check(self.req(reqwest::Method::GET, path).send(), None)?;
        r.json_value()
    }
}

/// A blocking client: 30 s to connect, no overall timeout (queries may run long).
pub fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(None)
        .user_agent(concat!("sparkles/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// JSON bodies without reqwest's `json` feature.
pub trait JsonBody {
    fn json_value(self) -> Result<J>;
}

impl JsonBody for reqwest::blocking::Response {
    fn json_value(self) -> Result<J> {
        let bytes = self.bytes()?;
        serde_json::from_slice(&bytes).context("the server did not answer JSON")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls() {
        assert_eq!(
            normalize("HTTPS://Sparql.Example.org/", false).unwrap(),
            "https://sparql.example.org"
        );
        assert_eq!(
            normalize("sparql.example.org:8443/base/", false).unwrap(),
            "https://sparql.example.org:8443/base"
        );
        assert_eq!(
            normalize("http://127.0.0.1:3030", false).unwrap(),
            "http://127.0.0.1:3030"
        );
        assert!(normalize("http://example.org", false).is_err());
        assert_eq!(
            normalize("http://example.org", true).unwrap(),
            "http://example.org"
        );
        assert!(normalize("ftp://example.org", true).is_err());
    }
}
