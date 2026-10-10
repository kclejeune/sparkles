//! The prefixes the language server knows for a document (spec C20 §8): the well-known
//! ones, then a server dataset's that the config file's `[lsp]` table names, then the
//! config file's `[prefixes]` table, a later source overriding an earlier one.
//!
//! A server's prefixes are read once per server and dataset, on a thread of their own,
//! the first time a document's config file names them. Until the read finishes, and when
//! it fails, completion goes on without them. A failure is logged to standard error.

use crate::fmt::config::{Editor, ServerSource};
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// What became of a server's prefixes.
#[derive(Clone, Debug)]
enum Fetch {
    Pending,
    #[cfg_attr(not(feature = "auth"), allow(dead_code))]
    Done(BTreeMap<String, String>),
    Failed,
}

/// The servers' prefixes, shared with the threads that read them.
#[derive(Clone, Default)]
pub struct Sources {
    servers: Arc<Mutex<HashMap<ServerSource, Fetch>>>,
}

impl Sources {
    /// The prefixes known with the config file's tables `editor`. A server named there
    /// for the first time is read in the background.
    pub fn known(&self, editor: &Editor) -> BTreeMap<String, String> {
        let mut out = sparkles::io::well_known_prefixes();
        if let Some(src) = &editor.server
            && let Fetch::Done(p) = self.fetch(src)
        {
            out.extend(p);
        }
        out.extend(editor.prefixes.clone());
        out
    }

    /// The state of the read of `src`, starting it when it is new.
    fn fetch(&self, src: &ServerSource) -> Fetch {
        let mut m = self.servers.lock();
        if let Some(f) = m.get(src) {
            return f.clone();
        }
        m.insert(src.clone(), Fetch::Pending);
        drop(m);
        self.start(src.clone());
        Fetch::Pending
    }

    #[cfg(feature = "auth")]
    fn start(&self, src: ServerSource) {
        let servers = self.servers.clone();
        std::thread::spawn(move || {
            let f = match read(&src) {
                Ok(p) => {
                    eprintln!(
                        "sparkles lsp: {} prefixes from /{} at {}",
                        p.len(),
                        src.dataset,
                        src.server
                    );
                    Fetch::Done(p)
                }
                Err(e) => {
                    eprintln!(
                        "sparkles lsp: cannot read the prefixes of /{} at {}: {e:#}",
                        src.dataset, src.server
                    );
                    Fetch::Failed
                }
            };
            servers.lock().insert(src, f);
        });
    }

    #[cfg(not(feature = "auth"))]
    fn start(&self, src: ServerSource) {
        eprintln!(
            "sparkles lsp: this build has no `auth` feature, so the [lsp] table's server {} is ignored",
            src.server
        );
        self.servers.lock().insert(src, Fetch::Failed);
    }

    /// Wait until no read is pending, for the tests.
    #[cfg(test)]
    pub fn settle(&self) {
        for _ in 0..600 {
            if !self
                .servers
                .lock()
                .values()
                .any(|f| matches!(f, Fetch::Pending))
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// `GET {server}/$/prefixes/{dataset}` with the token of `SPARKLES_TOKEN` or the saved
/// login. Plain `http` is refused for hosts other than loopback, as in every remote
/// command.
#[cfg(feature = "auth")]
fn read(src: &ServerSource) -> anyhow::Result<BTreeMap<String, String>> {
    use anyhow::Context;
    let base = crate::remote::normalize(&src.server, false)?;
    let token = std::env::var("SPARKLES_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .or_else(|| {
            crate::remote::credentials::load()
                .ok()
                .and_then(|c| c.servers.get(&base).map(|c| c.token.clone()))
        });
    let http = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(concat!("sparkles-lsp/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let ds =
        percent_encoding::utf8_percent_encode(&src.dataset, percent_encoding::NON_ALPHANUMERIC);
    let mut req = http.get(format!("{base}/$/prefixes/{ds}"));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let r = req.send().with_context(|| format!("cannot reach {base}"))?;
    let status = r.status();
    if !status.is_success() {
        anyhow::bail!("{status}");
    }
    let body: serde_json::Value =
        serde_json::from_slice(&r.bytes()?).context("the server did not answer JSON")?;
    let p = body["prefixes"]
        .as_object()
        .context("the answer has no prefixes")?;
    Ok(p.iter()
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_overrides_the_well_known_prefixes() {
        let s = Sources::default();
        let mut e = Editor::default();
        e.prefixes
            .insert("kclj".into(), "https://kclj.io/sparkles/".into());
        e.prefixes
            .insert("mem".into(), "https://example.org/mem#".into());
        let k = s.known(&e);
        assert_eq!(k["kclj"], "https://kclj.io/sparkles/");
        assert_eq!(k["mem"], "https://example.org/mem#");
        assert_eq!(k["rdf"], "http://www.w3.org/1999/02/22-rdf-syntax-ns#");
    }

    #[test]
    fn an_unreachable_server_never_blocks() {
        let s = Sources::default();
        let e = Editor {
            prefixes: BTreeMap::new(),
            server: Some(ServerSource {
                // nothing listens on port 9 of loopback
                server: "http://127.0.0.1:9".into(),
                dataset: "ds".into(),
            }),
        };
        let start = std::time::Instant::now();
        let k = s.known(&e);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        assert!(k.contains_key("rdf"));
        s.settle();
        assert!(s.known(&e).contains_key("rdf"));
    }
}
