//! A client for [Sparkles](https://github.com/kclejeune/sparkles) servers and for any
//! SPARQL 1.1 Protocol endpoint, such as Fuseki, QLever, Oxigraph or Wikidata.
//!
//! The design is in `docs/specs/P02-rust-client.md`. Query results stream as `oxrdf`
//! terms; writes return receipts that name the commit they made.
//!
//! ```no_run
//! use sparkles_client::{Client, Endpoint, QueryOptions, At};
//!
//! # async fn run() -> sparkles_client::Result<()> {
//! let client = Client::builder("http://localhost:3030").build()?;
//! let ds = client.dataset("ds");
//!
//! let receipt = ds.update("INSERT DATA { <urn:a> <urn:p> 1 }").await?;
//! println!("commit {:?}", receipt.commit_seq);
//!
//! let mut solutions = ds.select("SELECT ?s ?o { ?s ?p ?o }").await?;
//! while let Some(s) = solutions.next().await {
//!     let s = s?;
//!     println!("{:?} {:?}", s.get("s"), s.get("o"));
//! }
//!
//! // the same query against the state after the first commit
//! let old = ds
//!     .query_with("SELECT * { ?s ?p ?o }", &QueryOptions::new().at(At::Commit(1)))
//!     .await?;
//!
//! // any SPARQL endpoint
//! let wikidata = Endpoint::new("https://query.wikidata.org/sparql")?;
//! let yes = wikidata.ask("ASK { wd:Q42 ?p ?o }").await?;
//! # Ok(()) }
//! ```
//!
//! # Retries
//!
//! A request is retried, up to [`RetryPolicy::max_retries`] times, when the connection
//! could not be made, or when the server answers `429`, or `503` with `Retry-After`: the
//! server refused it before doing any work. Reads, `PUT`, `DELETE` and queries are also
//! retried after other transport errors and on `502`, `503` and `504`. The wait is the
//! server's `Retry-After`, else the reset time of an exhausted `RateLimit` field, else
//! exponential backoff with jitter.
//!
//! # Cancellation
//!
//! Dropping a future or a result stream closes the connection, and Sparkles then
//! cancels the query or write. Each options type also takes a deadline and a
//! [`CancellationToken`].

mod admin;
mod body;
mod client;
pub mod credentials;
mod dataset;
mod endpoint;
mod error;
mod results;
mod retry;
mod routes;
mod types;

#[cfg(feature = "blocking")]
pub mod blocking;

pub use body::{RdfBody, UploadPart};
pub use client::{CallOptions, Client, ClientBuilder, TokenSource};
pub use dataset::{CommitsOptions, Dataset, UpdateBatch, UploadOptions};
pub use endpoint::{Endpoint, QueryMethod, QueryOptions, ReadOptions, UpdateOptions, WriteOptions};
pub use error::{Error, Result, StatusError};
pub use results::{Quads, QueryResults, ResponseMeta, Solutions, Triples};
pub use retry::{RateLimit, RetryPolicy};
pub use types::{
    At, Commit, CommitList, DatasetInfo, DatasetType, Graph, Principal, Receipt, ServerInfo, Task,
    Whoami,
};

pub use oxrdf;
pub use oxrdfio::RdfFormat;
pub use reqwest::Method;
pub use sparesults::QuerySolution;
pub use tokio_util::sync::CancellationToken;
