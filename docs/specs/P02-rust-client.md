# P02: A Rust client for Sparkles and other SPARQL servers

> **Status:** specified
>
> **User docs:** [Usage: Rust client](../USAGE.md#rust-client) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui) ·
> [Comparison with Jena](../COMPARISON.md)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This spec draws on the Sparkles code and its API reference, the OpenAPI description of
[X03](X03-openapi-and-completions.md), the W3C SPARQL 1.1 Protocol, Graph Store HTTP
Protocol and Query Results formats, RFC 9110, RFC 7617, RFC 6750, RFC 8187 and the IETF
draft on RateLimit header fields. The public documentation of Apache Jena's
`RDFConnection` and `RDFLink` (Apache-2.0) and of Oxigraph's Rust API and pyoxigraph (MIT
OR Apache-2.0) served as prior art for the shape of the API. The documentation of
progenitor and openapi-generator informed the choice in §2. No code was copied from any
of them. Fluree was not consulted.

## 1. Summary, goals, non-goals

A Rust program that talks to a Sparkles server today has two choices. It can embed the
engine with the `sparkles` crate, which does not reach a remote server, or it can build
HTTP requests by hand with the API reference open. The command line has its own client
code in the server crate for `--server`, `rsparql` and `rupdate`, which other programs
cannot use. This spec adds `crates/sparkles-client`, a library that speaks the SPARQL
1.1 Protocol to any server and the Sparkles API to a Sparkles server.

**Goals**

1. **Any SPARQL server.** Queries, updates and the Graph Store Protocol work against
   Fuseki, QLever, Oxigraph, Wikidata and Sparkles. Nothing Sparkles-specific is sent to
   a plain endpoint.
2. **Streaming results as RDF terms.** SELECT results arrive as an asynchronous stream
   of `sparesults::QuerySolution`, CONSTRUCT and Graph Store reads as streams of
   `oxrdf::Triple` or `oxrdf::Quad`. Nothing is collected unless the caller asks.
3. **Writes with receipts.** Every write returns the commit it produced, from the
   `Sparkles-Commit` header and the receipt body ([CI](CI-commit-identity.md)), along
   with dry-run reports ([C15](C15-write-previews.md)).
4. **The Sparkles API's most used parts.** These are point-in-time reads with `?at=`
   ([F06](F06-snapshots-and-point-in-time.md)), stored queries by name
   ([C16](C16-stored-queries.md)), uploads, commits, datasets, tasks, backups, statistics
   and the schema report ([C02](C02-schema-discovery.md)).
5. **Authentication as the server accepts it.** The client sends HTTP Basic, API tokens,
   OIDC access tokens and the token saved by `sparkles auth login`
   ([C09](C09-dataset-access-control.md)).
6. **Robust by default.** Requests are retried with backoff when the server says so, as
   `Retry-After` and the RateLimit fields of the rate limiter tell it to. A write is
   retried only when the server refused it before doing any work. Calls take a deadline
   and a cancellation token.
7. **Async first, with a blocking facade.** The core runs on tokio and reqwest. A
   `blocking` module wraps it for programs without a runtime.
8. **Light.** The crate depends on neither the server crate nor the engine. Every
   dependency is already in the workspace's lock file.

**Non-goals**

- Client-side SPARQL parsing. The client does not parse queries to find their form. It
  reads the form of the result from the response's media type.
- A generated client for all 174 operations. Operations outside goal 4 are reachable
  through a generic JSON call on the operation table (§4.2).
- The interactive logins of `sparkles auth login`. The client reads the saved token but
  does not run the browser or device-code flows.
- The MCP endpoint, the change feed's server-sent events and the web UI's session
  cookies.
- Multi-request transactions. Neither Sparkles nor the SPARQL Protocol has them (§5.6).

## 2. Generated or hand-written

X03 made the OpenAPI description usable by generators, so generation was the first
option.

| Approach | For | Against |
|---|---|---|
| progenitor | Generates an idiomatic async reqwest client at build time, with typed builders. | It reads OpenAPI 3.0 through the `openapiv3` crate. The Sparkles document is 3.1 and uses 3.1 features such as type arrays (`["integer", "null"]`) and `const`, so it would need a downgrade step. Its responses are JSON or opaque byte streams, so the SPARQL results would still need the code this spec describes. |
| openapi-generator's `rust` target | Mature, and supports 3.1 input. | A Java tool at build time. It generates one model per schema and a function per operation, with no streaming. The 58 open admin schemas become untyped maps. Content negotiation across a dozen media types, which is most of the SPARQL API, becomes a string body. |
| **Hand-written, with the document as the contract** | The API follows Jena and Oxigraph rather than the route table. Results stream as RDF terms, and the code stays small. | Someone has to keep it in step with the server. |

The design takes the last row. The checked-in `docs/openapi.json` is the contract, and a
test in the client crate keeps the two in step:

1. The client builds every URL from an operation table (`routes.rs`). Each entry names
   the `operationId`, the method, the path template and the query parameters the client
   may send. A debug assertion refuses a parameter the entry does not list.
2. The test reads `docs/openapi.json` and checks each entry. The operation must exist
   with that method and path, and every parameter the client sends must be declared on
   it, in the same place (query or header).
3. For each typed response (`Commit`, `Receipt`, `Task`, `DatasetInfo`, `CommitList`,
   `ServerInfo`, `Whoami`), the test builds two JSON objects from the schema, one with the
   required members only and one with every member, and deserializes both. A required
   member the client misses, or a member whose type changed, fails the test.

A route renamed or removed on the server therefore fails the client's test as soon as
`mise run openapi` rewrites the checked-in copy, which the server's own test forces.

## 3. Crate and dependencies

```
crates/sparkles-client/
  Cargo.toml
  src/lib.rs           re-exports and the crate documentation
  src/client.rs        Client, ClientBuilder, authentication, sending and retries
  src/retry.rs         RetryPolicy, Retry-After and RateLimit parsing
  src/credentials.rs   the saved credentials of `sparkles auth login`
  src/endpoint.rs      Endpoint: the SPARQL Protocol and the Graph Store Protocol
  src/dataset.rs       Dataset: a Sparkles dataset's own operations
  src/admin.rs         server, datasets, tasks, backups, stats
  src/results.rs       QueryResults, Solutions, Triples, Quads
  src/body.rs          RdfBody and upload parts
  src/types.rs         Receipt, Commit, Task and the other typed bodies
  src/routes.rs        the operation table and the contract test
  src/blocking.rs      the blocking facade
```

Dependencies, all in the lock file already:

| Crate | Use |
|---|---|
| `reqwest` 0.13 without default features, with `rustls`, `stream` and `multipart` | HTTP |
| `tokio` (`rt`, `time`, `macros`, `fs`, and `rt-multi-thread` for `blocking`) | the runtime |
| `tokio-util` (`io`) | `StreamReader` over the response body, `CancellationToken` |
| `futures-util` | stream adapters |
| `oxrdf`, `oxrdfio` and `sparesults` with `async-tokio` | terms, RDF parsers and result parsers |
| `serde`, `serde_json`, `toml` | bodies and the credentials file |
| `thiserror`, `percent-encoding`, `httpdate`, `bytes` | errors, URLs, `Retry-After` dates |

Cargo features: `blocking` (on by default) adds the facade, and `gzip`, `zstd` and
`brotli` turn on reqwest's response decompression.

## 4. The API

### 4.1 Clients, endpoints and datasets

```rust
use sparkles_client::{Client, Endpoint, QueryResults};

// a Sparkles server
let client = Client::builder("https://sparql.example.org")
    .saved_credentials()?          // the token of `sparkles auth login`
    .build()?;
let ds = client.dataset("library");

// any SPARQL endpoint
let wikidata = Endpoint::new("https://query.wikidata.org/sparql")?;
```

`Client` holds the HTTP client, the authentication, the retry policy and the server's
base URL. It is cheap to clone and safe to share between tasks. `Endpoint` is a set of
SPARQL Protocol URLs: a query URL, and optionally an update URL and a Graph Store URL.
`Endpoint::fuseki(base, name)` builds Fuseki's (`/name/sparql`, `/name/update`,
`/name/data`). A `Dataset` is an `Endpoint` with Sparkles' URLs (`/{ds}/sparql`,
`/{ds}/update`, `/{ds}/data`) and the dataset's own operations. It dereferences to its
`Endpoint`, so every protocol method works on both. Only a `Dataset` sends Sparkles
parameters such as `receipt=true`.

The names follow Jena's `RDFLink` where Sparkles has the same concept, in Rust's case.

| Jena `RDFLink` | Client | Returns |
|---|---|---|
| `query` | `query`, `query_with` | `QueryResults` |
| `querySelect` | `select` | `Solutions` |
| `queryAsk` | `ask` | `bool` |
| `queryConstruct` | `construct` | `Triples` |
| `update` | `update`, `update_with` | `Receipt` |
| `fetch(graph)` | `get_graph` | `Triples` |
| `fetchDataset` | `get_dataset` | `Quads` |
| `load(graph, file)` | `load` | `Receipt` |
| `put(graph, data)` | `put_graph` | `Receipt` |
| `delete(graph)` | `delete_graph` | `Receipt` |
| `loadDataset`, `putDataset` | `post_dataset`, `put_dataset` | `Receipt` |

### 4.2 Queries

`query` sends the query by GET when the URL stays under 2,000 bytes, and as a form POST
otherwise, as `rsparql` does. `QueryOptions` can force either. The `Accept` header asks for
SPARQL JSON results first, then XML and TSV, then N-Triples and Turtle for graphs. The
response's `Content-Type` decides the variant of `QueryResults`:

```rust
pub enum QueryResults {
    Solutions(Solutions),   // SELECT
    Boolean(bool),          // ASK
    Graph(Triples),         // CONSTRUCT, DESCRIBE
}
```

`Solutions` wraps sparesults' async parser over the response body. `next().await` returns
the next `QuerySolution`, `variables()` the projection, and `into_stream()` a
`futures::Stream`. `Triples` and `Quads` wrap oxrdfio's async parser in the same way.
`collect()` gathers the rest into a `Vec`. Each of them carries `ResponseMeta` with the
status, `Sparkles-Commit`, `Sparkles-Dataset-Id`, `Sparkles-Head`, `Sparkles-At`, the
`ETag`, the request id and the RateLimit fields.

`QueryOptions` sets `at` (an `At`: head, a commit, a time or a snapshot), the server's
`timeout`, `reasoning`, `default-graph-uri`, `named-graph-uri`, and the common options of
§4.6. `construct_quads` asks for N-Quads and TriG, for CONSTRUCT templates with `GRAPH`
([G06](G06-arq-query-extensions.md)).

Operations the client has no method for are reachable with
`client.call_json(operation_id, path_params, query_params, body)`, which looks the
operation up in the table of §2 and returns the JSON body.

### 4.3 Updates and receipts

```rust
let r = ds.update("INSERT DATA { <urn:a> <urn:p> 1 }").await?;
println!("commit {}", r.commit_seq.unwrap());
```

`Receipt` has the response status, `commit_seq` and `dataset_id` from the headers, and for
a `Dataset` the parsed receipt (`committed` and the `Commit`). It also keeps the whole JSON
body, which holds an update's statistics, a Graph Store write's counts or a dry run's
report. `UpdateOptions` and `WriteOptions` set a commit message (sent as an RFC 8187
extended value when it is not ASCII, as the CLI does), `dry_run`, `validate`, the server's
`timeout`, `using-graph-uri` and `using-named-graph-uri` for updates, and `If-Match` and
`If-None-Match` for Graph Store writes. A plain endpoint's update returns a `Receipt`
with the status and the body only.

### 4.4 The Graph Store Protocol and uploads

`Graph` names the target: `Graph::Default`, `Graph::Named(NamedNode)` or, for reads,
`Graph::Union`. Reads ask for N-Triples first, and for N-Quads when the whole dataset is
read. `ReadOptions` sets `at` and `If-None-Match`. A `304` gives an empty stream whose
meta says so.

Bodies are `RdfBody` values. `RdfBody::file(path)` takes the syntax from the file name and
sends `.gz`, `.zst` and `.br` files compressed with the matching `Content-Encoding`, as
`sparkles load --server` does. `RdfBody::bytes(data, format)` sends a buffer, and
`RdfBody::triples` and `RdfBody::quads` serialize terms as N-Triples or N-Quads. A file
is opened again for each attempt, so file bodies can be retried.

`Dataset::upload` sends a multipart request to `/{ds}/upload`, with RDF files and CSV
or TSV tables ([C05](C05-tabular-imports.md)), in one commit.

### 4.5 Sparkles operations

| Method | Operation |
|---|---|
| `Dataset::run_stored(name, params)` | `POST /{ds}/queries/{name}` with a JSON body, so numbers and booleans keep their types |
| `stored_queries`, `stored_query`, `put_stored_query`, `delete_stored_query` | `/$/queries/{ds}…` |
| `commits(options)`, `commit(ref)` | `/$/commits/{ds}` and `/$/commits/{ds}/{ref}`, typed |
| `info`, `stats(at)`, `schema(options)` | `/$/datasets/{ds}`, `/$/stats/{ds}`, `/$/schema/{ds}` |
| `backups()`, `backup_nquads()` | the dataset's backups in the repositories, and Fuseki's N-Quads backup task |
| `Client::server`, `ping`, `whoami` | `/$/server`, `/$/ping`, `/$/whoami` |
| `Client::datasets`, `create_dataset`, `delete_dataset` | `/$/datasets` |
| `Client::tasks`, `task`, `cancel_task`, `wait_for_task` | `/$/tasks`; `wait_for_task` polls until the task ends |
| `Client::backup_files`, `stats` | Fuseki's `/$/backups-list` and `/$/stats` |

`Commit`, `CommitList`, `Task`, `DatasetInfo`, `ServerInfo` and `Whoami` are typed. Each
keeps members it does not know in a map, so a newer server does not break an older
client. The schema report, statistics, stored queries and backups are
`serde_json::Value`, because their schemas are open in the description.

### 4.6 Errors, deadlines and cancellation

`Error` separates a non-2xx response (`Error::Status`, with the status, the server's
`error`, `code`, `detail`, `line`, `column` and request id), a transport failure, a
deadline, a cancellation, a malformed result, a result of the wrong form (`select` on an
ASK) and configuration mistakes.

Every options type has the common options: `deadline` (a client-side limit on the whole
call, including retries and reading the body), `cancel` (a `CancellationToken`), extra
headers, and `no_retry`. The server's `timeout` parameter is a separate option, since
it limits the work on the server. Dropping a future or a result stream closes the
connection, and Sparkles then cancels the query or the write
([C01](C01-observability-and-budgets.md)). A cancelled token does the same for a call
already in flight, including a stream being read.

### 4.7 Authentication

| Builder method | Sends |
|---|---|
| `basic_auth(user, password)` | `Authorization: Basic …` (RFC 7617). A token as the password works too. |
| `bearer_token(token)` | `Authorization: Bearer …` (RFC 6750), for API tokens and OIDC access tokens. |
| `token_source(source)` | A bearer token from a callback, asked again once after a `401`, for OIDC tokens that expire. |
| `saved_credentials()` | The token `sparkles auth login` saved for this server, with `SPARKLES_TOKEN` taking precedence. |

`Client::from_env()` takes the server from `SPARKLES_SERVER` or the file's default, as the
CLI's `--server` does. The file is `$XDG_CONFIG_HOME/sparkles/credentials.toml`, by
default `~/.config/sparkles/credentials.toml`. Servers are keyed by their normalized URL,
and the client normalizes the same way as the CLI.

Credentials are never sent over plain http to a host other than localhost unless the
builder allows it with `allow_insecure_http()`. The rule is the CLI's.

### 4.8 Retries

`RetryPolicy` defaults to three retries. A response is retried when it is one of these:

| Response | Safe requests | Writes |
|---|---|---|
| Connection refused, or failed before the request was sent | yes | yes |
| Other transport errors | yes | no |
| `429` | yes | yes (the rate limiter refuses before any work) |
| `503` with `Retry-After` | yes | yes (a concurrency cap or a restore refuses before any work) |
| `503` without it, `502`, `504` | yes | no (the write may have run) |

Safe requests are GET, HEAD, PUT and DELETE, and queries sent by POST. The delay is
`Retry-After` (seconds or an HTTP date) when present, else the `t` of the `RateLimit`
field when it reports no requests left, else exponential backoff from 250 ms with full
jitter, capped at 30 s. A `Retry-After` beyond `max_retry_after` (60 s by default) is not
waited for, and the error is returned. Neither is a delay that would pass the call's
deadline.

### 4.9 Blocking

`sparkles_client::blocking::Client` owns a tokio runtime with one worker thread and
mirrors the async API. Its `Solutions`, `Triples` and `Quads` are `Iterator`s. Like
reqwest's blocking client, it must not be used from inside an async runtime.

## 5. Behaviour details

### 5.1 Plain endpoints

An `Endpoint` built with `Endpoint::new` or `Client::endpoint` sends only protocol
parameters: `query`, `update`, `default-graph-uri`, `named-graph-uri`, `using-graph-uri`,
`using-named-graph-uri`, `graph` and `default`. Options that only Sparkles understands
(`at`, `dry_run`, `validate`, the commit message) are refused with a configuration error
on a plain endpoint, so they never get silently ignored.

### 5.2 Content types

Result media types are matched without parameters and case-insensitively, through
`QueryResultsFormat::from_media_type` and `RdfFormat::from_media_type`. CSV results
cannot be parsed back into terms, so the client never asks for them. A response in a
media type the client cannot parse is an error that names it.

### 5.3 URLs

The base URL may have a path (`https://host/sparkles`). Path parameters are
percent-encoded, except that a Graph Store graph path keeps its slashes. Dataset names are
encoded like the CLI encodes them.

### 5.4 Commit messages

A message is sent as it is when it is ASCII and does not itself look like an extended
value, and as `UTF-8''` followed by its percent-encoding otherwise.

### 5.5 Rate limits

`ResponseMeta::rate_limit` exposes the `RateLimit` field's remaining requests and reset
time, so a caller can pace itself. The client does not delay requests ahead of time,
because it cannot tell which limit class the next request belongs to.

### 5.6 Transactions

Jena's remote connections are transactional only on the client side. Their `begin` and
`commit` take a local lock and send nothing. Sparkles has no multi-request transactions
either. The client offers the two guarantees the server does have:
- `Dataset::batch()` collects updates and `INSERT DATA`/`DELETE DATA` operations and
  sends them as one update request. The server runs one request as one commit, so the
  batch applies entirely or not at all.
- Graph Store writes take `If-Match` with an entity tag from an earlier read, which the
  server checks under the writer lock. Together they give optimistic concurrency per
  graph or dataset.

## 6. The command line

The CLI's `--server` commands and `rsparql` write response bodies to standard output as
they arrive, in whatever format the user asked for. They do not parse results. Moving
them onto the client would replace a few lines of reqwest calls with the same number of
client calls and change their error messages. The part that is duplicated, and that must
agree between the two, is the credentials file and the URL normalization that keys it. The
CLI should use the client's `credentials` module for both, and keep its own request
code.

## 7. Testing

- **Contract.** The test of §2 against `docs/openapi.json`.
- **Against a server.** `crates/sparkles-server/tests/rust_client.rs` starts `sparkles
  serve` on a port in 5540–5559, as `cli_tools.rs` does, and runs the acceptance
  examples below through the async API and the blocking facade. It lives in the server
  crate because that is where the binary is built. The client crate is only a
  dev-dependency there.
- **Retries.** Tests in the client crate run a small axum server that answers `429`,
  `503` with and without `Retry-After`, and `502`, and count the attempts it receives. They
  check that writes are not retried where §4.8 says not to, that a long `Retry-After`
  ends the retries, that a deadline stops the backoff, and that a cancelled token stops a
  stream.
- **Units.** URL normalization against the CLI's cases, the credentials file, the
  `Retry-After` and `RateLimit` parsers, and the commit-message encoding.

## 8. Acceptance examples

- **A1.** `ds.update("INSERT DATA {…}")` returns a `Receipt` whose `commit_seq` is the
  server's next commit and whose `commit.inserted` is the number of triples inserted.
- **A2.** `ds.select("SELECT ?s ?o {…}")` streams `QuerySolution`s whose terms are
  `oxrdf` terms equal to the inserted ones, including a language-tagged literal and a
  typed literal.
- **A3.** `ds.ask("ASK {…}")` is `true`, and `ds.select` on an ASK is
  `Error::UnexpectedResults`.
- **A4.** `ds.construct("CONSTRUCT WHERE {…}")` streams the triples.
- **A5.** After a second update, a query with `at(At::Commit(n))` sees the state of the
  first commit, and `meta().at` is `commit:n`.
- **A6.** `put_graph`, `get_graph`, `post_graph` and `delete_graph` round-trip a named
  graph. A `put_graph` with `If-Match` set to a stale entity tag fails with status `412`
  and code `precondition-failed`.
- **A7.** A stored query saved with `put_stored_query` runs by name with a typed
  parameter, and `meta().query_version` is set.
- **A8.** `client.datasets()` lists the dataset, `create_dataset` and `delete_dataset`
  work, `backup_nquads` returns a task, and `wait_for_task` sees it finish.
- **A9.** Against a server with an auth configuration, a call without credentials fails
  with status `401`, the same call with `bearer_token` succeeds, and `whoami` names the
  token's owner.
- **A10.** `Endpoint::new(".../ds/sparql")` with a separate update URL, used as a plain
  endpoint, queries and updates the server without sending any Sparkles parameter.
- **A11.** The mock answers `429` with `Retry-After: 1` twice and then `200`. The query
  succeeds after three attempts and about two seconds. An update against `502` is
  attempted once.
- **A12.** The blocking facade runs A1 to A4 from a plain `#[test]`.

## 9. Sources

- W3C SPARQL 1.1 Protocol, SPARQL 1.1 Graph Store HTTP Protocol, and SPARQL 1.1 Query
  Results JSON, XML, CSV and TSV Formats.
- RFC 9110 (HTTP semantics, `Retry-After`, conditional requests), RFC 7617 (Basic),
  RFC 6750 (Bearer), RFC 8187 (extended header values), and
  draft-ietf-httpapi-ratelimit-headers.
- The OpenAPI Specification 3.1.1, and `docs/openapi.json` (X03).
- Apache Jena's documentation of `RDFConnection` and `RDFLink` (Apache-2.0), for the
  operations and their names.
- Oxigraph's Rust API documentation and pyoxigraph's documentation (MIT OR Apache-2.0),
  for `QueryResults` and streaming solutions.
- The documentation of progenitor (MPL-2.0) and openapi-generator (Apache-2.0), for §2.
- The docs.rs documentation of reqwest, tokio, tokio-util, sparesults and oxrdfio.
- The Sparkles code (`crates/sparkles-server/src/remote/`, `src/tools/endpoint.rs`) and
  `docs/API.md`.

## Outcome

To be written when the implementation lands.
