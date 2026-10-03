//! Embeddings computed on write (F08): a vector index can name the literals to embed
//! and an OpenAI-compatible embeddings endpoint, and a background worker keeps the
//! index's vectors in step with that text.
//!
//! * **Scheduling.** A commit through the log notes the (subject, graph) pairs whose
//!   selected text it touched, as vocabulary keys that survive compactions. Bulk
//!   commits, worker starts and `reembed` schedule a full pass instead, which compares
//!   every pair's inputs with the record of what was embedded ([`worker`]).
//! * **Embedding** runs in three steps, so that nothing holds the store while a request
//!   is out: [`Store::embed_prepare`](crate::store::Store::embed_prepare) picks a batch,
//!   [`Batch::run`] asks the provider ([`client`]), and
//!   [`Store::embed_apply`](crate::store::Store::embed_apply) writes the vectors in one
//!   commit of kind `embed`.
//! * **Egress.** Requests go only to an index's configured endpoint, through the
//!   process's [`Environment`]: its outbound policy and its named secrets.

pub mod client;
pub mod config;
#[doc(hidden)]
pub mod mock;
pub(crate) mod worker;

pub use config::{ApiKey, EmbeddingConfig};
pub use worker::{Batch, Embedded, Prepared};

use crate::outbound::OutboundPolicy;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Where a named secret comes from (`serve --embedding-secret NAME=env:VAR|file:PATH`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretSource {
    Env(String),
    File(PathBuf),
}

impl std::str::FromStr for SecretSource {
    type Err = String;
    fn from_str(s: &str) -> Result<SecretSource, String> {
        match s.split_once(':') {
            Some(("env", v)) if !v.is_empty() => Ok(SecretSource::Env(v.into())),
            Some(("file", p)) if !p.is_empty() => Ok(SecretSource::File(p.into())),
            _ => Err(format!("{s:?}: expected env:VARIABLE or file:PATH")),
        }
    }
}

/// What embedding requests may do in this process.
#[derive(Clone, Debug)]
pub struct Environment {
    /// `false`: no worker embeds and searches cannot pass text (`serve --no-embedding`)
    pub enabled: bool,
    /// where requests may connect, with their timeouts and response ceiling
    pub outbound: OutboundPolicy,
    /// the secrets `apiKey: {"secret": NAME}` names
    pub secrets: BTreeMap<String, SecretSource>,
}

impl Default for Environment {
    fn default() -> Environment {
        Environment {
            enabled: true,
            outbound: OutboundPolicy::default(),
            secrets: BTreeMap::new(),
        }
    }
}

static ENV: RwLock<Option<Arc<Environment>>> = RwLock::new(None);

/// Set the embedding environment of this process (a store can override it with
/// [`Store::set_embedding_environment`](crate::store::Store::set_embedding_environment)).
pub fn set_environment(e: Environment) {
    *ENV.write().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(e));
}

/// The process's embedding environment (the default refuses private destinations).
pub fn environment() -> Arc<Environment> {
    ENV.read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .unwrap_or_default()
}

/// 64-bit FNV-1a: stable across versions and platforms, so it can be persisted.
pub(crate) fn fnv(bytes: &[u8]) -> u64 {
    let mut h = Fnv::default();
    h.write(bytes);
    h.0
}

/// An FNV-1a hasher fed in pieces.
pub(crate) struct Fnv(pub u64);

impl Default for Fnv {
    fn default() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    pub fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// The embedding part of a vector index's status.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingStatus {
    /// `idle`, `scanning`, `embedding`, `backoff`, `paused` (no worker runs) or
    /// `disabled` (embedding is off in this process)
    pub state: String,
    pub model: String,
    /// the endpoint's URL without credentials or query
    pub endpoint: String,
    /// subjects (per graph) waiting to be embedded
    pub backlog: u64,
    /// a full pass in progress
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan: Option<EmbeddingScan>,
    /// every commit up to this one has its text embedded
    pub applied_seq: u64,
    pub head_seq: u64,
    /// vectors written, requests made and inputs failed since the store was opened
    pub embedded: u64,
    pub requests: u64,
    pub failed: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<EmbeddingError>,
    /// when the worker tries again (in `backoff`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_batch: Option<EmbeddingBatch>,
    /// the index's embedding configuration (it names secrets, never holds keys)
    #[serde(default)]
    pub config: EmbeddingConfig,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingScan {
    pub done: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingError {
    pub at: String,
    pub message: String,
    /// the subject whose input failed, when one did
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// Why embedding failed, for the metrics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// a batch's request failed after its retries (a network error, a timeout, `429`,
    /// `5xx`)
    Transient,
    /// `401` or `403`
    Auth,
    /// the outbound policy refused the endpoint
    Refused,
    /// a missing secret, or an answer that is not an embeddings response
    Fatal,
    /// an input the provider refused, or answered with a vector it cannot use
    Rejected,
    /// reading a pair's text from the store
    Read,
    /// writing a batch's vectors
    Write,
}

impl FailureKind {
    pub const ALL: [FailureKind; 7] = [
        FailureKind::Transient,
        FailureKind::Auth,
        FailureKind::Refused,
        FailureKind::Fatal,
        FailureKind::Rejected,
        FailureKind::Read,
        FailureKind::Write,
    ];

    pub fn name(self) -> &'static str {
        match self {
            FailureKind::Transient => "transient",
            FailureKind::Auth => "auth",
            FailureKind::Refused => "refused",
            FailureKind::Fatal => "fatal",
            FailureKind::Rejected => "rejected",
            FailureKind::Read => "read",
            FailureKind::Write => "write",
        }
    }
}

/// One index's embedding counters and gauges since the store was opened, for metrics
/// ([`Store::embedding_metrics`](crate::store::Store::embedding_metrics)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmbeddingMetrics {
    /// the vector index
    pub index: String,
    /// requests made, retries included
    pub requests: u64,
    /// inputs sent to the provider (cached inputs are not sent)
    pub inputs: u64,
    /// vectors written
    pub vectors: u64,
    /// failures by [`FailureKind::ALL`]'s order: batches for the kinds of a request,
    /// pairs for `read`, inputs for `rejected`, batches for `write`
    pub failures: [u64; 7],
    /// subjects (per graph) waiting to be embedded, as in the status
    pub backlog: u64,
    /// commits since the newest one whose text is all embedded
    pub lag: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingBatch {
    pub at: String,
    pub inputs: u64,
    pub ms: f64,
}

/// What [`run_worker`] calls to reach its store: `f` runs with the store, or the
/// function returns `false` when the store is gone.
pub type WithStore<'a> = &'a dyn Fn(&mut dyn FnMut(&crate::store::Store)) -> bool;

/// Run a store's embedding worker on this thread until the store closes or `with`
/// reports it gone. Nothing holds the store while a request is out. `shared` is
/// [`Store::embedder`](crate::store::Store::embedder).
pub fn run_worker(shared: Arc<worker::Embedder>, with: WithStore<'_>) {
    let _attached = shared.attach();
    loop {
        if shared.is_closed() {
            return;
        }
        let mut prepared = None;
        if !with(&mut |s| prepared = Some(s.embed_prepare())) {
            return;
        }
        match prepared.expect("set by the closure") {
            Prepared::Batch(b) => {
                let sh = shared.clone();
                let mut done = Some(b.run(&move |d| sh.sleep(d)));
                if !with(&mut |s| {
                    if let Some(d) = done.take() {
                        s.embed_apply(d)
                    }
                }) {
                    return;
                }
            }
            Prepared::Progress => {}
            Prepared::Idle => shared.wait(std::time::Duration::from_secs(30)),
            Prepared::Wait(d) => shared.wait(d),
        }
    }
}

pub use worker::Embedder;
