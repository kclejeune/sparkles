# Libraries and integrations capabilities

This page describes the available capabilities and their current limits. The
[capability matrix](../FEATURES.md#capability-status) records delivery status within
each stated scope.

## Libraries and bindings

The engine is a Rust library, and the Python, JVM and Node.js packages embed the same
engine in process. Thread destruction preserves query callback family checks and
the visibility limits of captured callback views.

Store shutdown finishes embedding record output even if a caller retains the
Embedder. If a clean-close validation callback panics, shutdown first revokes
retained writers and finishes pending output, then propagates the original unwind.

Stores in one persistent opening and their BranchSet share ownership of the dataset
root's process lock. Dropping main leaves that lock held while an embedded caller retains
a branch Store or BranchSet. Removed and retired branch stores keep the same lease
until their resources are destroyed. Release those owners before reopening the
dataset directory. In-memory branches share runtime ownership without a file lock.

Catalog deletion, persistent rename and backup replacement also prove exclusive runtime
ownership before publishing directory changes. Raw Strong or Weak aliases to a participating
DatasetState, branch Store or BranchSet conservatively make disposal busy. Refusal preserves
the original allocations and files. Release those aliases and retry. Built-in worker and
branch references use revocable links, so idle cached branches can close without changing
the public StoreHandle variants. Close callbacks run after catalog lookup exclusion ends.
Callbacks may look up datasets but must defer catalog mutations until disposal returns.
Members closed during unwinding can skip checkpoints that require a clean close.
The runtime retains root exclusion through required resource shutdown.

After runtime close commits, an I/O failure can reopen the existing dataset through a
fresh runtime. Recovery verifies its original UUID and existing CURRENT before opening.
If a delete cannot synchronize its rollback, the catalog leaves the dataset unavailable
and reports the retained data path. Unresolved moved bytes stay in
`databases/.kept-<name>-<token>` and catalog startup does not remove them. Startup refuses
only a registered dataset whose directory or CURRENT file is missing. It serves the other
datasets, keeps the refused one registered and names its expected path in the refusal.
Moving the retained directory back to that path and restarting recovers the dataset.
A close callback that panics after the runtime close commits leaves the dataset
registered and closed, and the next catalog open reopens it. Restore publication
verifies the restored identity separately from the original identity required for
rollback.

`sparkles::managed_source::ResidentSourceOwner` retains the catalog's actual Dataset and
an exact resident branch Store. Core `ArcSourceOwner` supports direct Arc-backed callers.
Their getters borrow existing openings and do not open cold branches. These leases provide
lifetime and identity only. Read authorization, authenticated managed writes and durable
source holds remain separate requirements. Runtime ownership checks do not authorize
removal of source bytes required by a durable consumer.

* The embedded Rust API is `sparkles::Dataset`, with a fluent query builder in
  `sparkles::querybuilder` ([USAGE.md](../USAGE.md#embedding-the-library),
  [P06](../specs/P06-library-admin-api.md)). Opening a dataset installs its write-time
  validation, RDFS on read, stored queries and GraphQL schema as the server does. Handles
  on `Dataset` manage snapshots and history, indexes, settings, schema reports, stored
  queries, reasoning, validation, GraphQL and backups, with one `Control` for
  cancellation and progress. A catalog manages datasets, restores and clones. The
  reasoner, validators, backups and GraphQL are cargo features of the `sparkles` crate,
  off by default and all on with `full`.
* The Python package `sparkles` in `crates/sparkles-py` ships abi3 wheels for CPython 3.10
  and later on Linux and macOS ([USAGE.md](../USAGE.md#python),
  [P01](../specs/P01-python-bindings.md)). It covers datasets, loads, SPARQL, transactions,
  history, search indexes, reasoning and validation, and has an rdflib store plugin and
  the query builder. Calls release the GIL, and Ctrl-C stops a running query.
  `sparkles.aio` provides async queries, bounded cursors and transactions, with owned
  cancellation and explicit async cleanup.
* `sparkles-jena` is a Jena `DatasetGraph` backed by Sparkles for JVM programs
  ([USAGE.md](../USAGE.md#jvm-apache-jena)). It has Jena's transactions, query and update
  engines that run in Sparkles with ARQ as the fallback, bulk loads, historical reads,
  dumps, typed administration, a Fuseki assembler and an import of TDB2 databases. It
  passes Jena's contract tests for datasets, graphs, transactions, updates and prefixes.
* `@sparkles-rdf/engine` embeds the database in Node.js through Node-API, with RDF/JS
  terms, bounded async results, streams, transactions, administration and a Comunica
  source ([USAGE.md](../USAGE.md#javascript-and-typescript)). `@sparkles-rdf/client` gives
  streaming SPARQL, Graph Store operations and typed administration over Fetch. Both
  share terms, result types and errors through `@sparkles-rdf/common`.
  Repeated quad operations reuse term handles, whose native kind checks borrow the
  cached terms. Result rows decode their RDF/JS terms lazily.
* `crates/sparkles-client` is a Rust client of remote servers, async on tokio with a
  blocking form ([USAGE.md](../USAGE.md#rust-client), [P02](../specs/P02-rust-client.md)). It
  speaks the SPARQL 1.1 Protocol and the Graph Store Protocol to Sparkles, Fuseki,
  QLever, Oxigraph or Wikidata and streams results as `oxrdf` terms. On a Sparkles server
  it adds point-in-time reads, stored queries, uploads, batches, commits, datasets, tasks,
  backups, statistics and the schema report. The `*_typed` methods read generated models
  for schema reports, server and dataset statistics, stored queries and backup listings.
  Typed history methods read snapshot and branch metadata and bounded JSON diffs, and
  create and delete snapshots and branches. Both client forms support per-call
  cancellation and deadlines. Branch deletion accepts force and reparent options.
  The existing JSON methods keep their signatures. It retries as `Retry-After` and
  `RateLimit` direct, and never repeats a write whose outcome is unknown.

The JVM `DatasetGraphSparkles.algorithms()` handle lists, gets, stores and deletes
named projections. It also builds query text, runs queries, returns statistics and writes
results. Callers close the returned query execution. Writes use batched commits, so a
later failure can leave earlier batches committed. Other algorithm surfaces remain under
roadmap item 8.

The JVM `DatasetGraphSparkles.mappings()` and Node.js `Dataset.mappings` handles list,
version, store, delete and run mappings. Their `runMapping` methods run mapping text.
Both return the commit receipt and mapping statistics. The default JVM and Node.js
packages do not enable SQLite or Parquet mapping sources. Their direct and stored runs
carry cancellation and deadlines through conversion and the final load.

Native Rust callers use `Dataset::run_mapping_with` with `guard::WriteOptions` for
controls that cover conversion and the final load. The complete write options retain
all cancellation sources, the deadline, graph permissions, preconditions, preview
settings and commit metadata. Checks run at conversion boundaries and existing reader
checkpoints, and the loader applies the same controls while acquiring the writer and
before publication. A refusal leaves committed data unchanged. A stop requested after
the loader publishes does not retract the successful receipt.

The existing `Dataset::run_mapping` signature and `mapping::RunOptions` fields remain
unchanged. `RunOptions.check` remains a conversion-only callback and is also honored
by the controlled variant. Use the structured write options for cancellation or a
deadline that must reach the final writer wait.

The [API compatibility policy](../DEVELOPMENT.md#api-compatibility) identifies the
covered package roots and version rules. HTTP and standard CLI development-baseline
checks are available. Public Rust and binding release comparisons remain under
roadmap item 60. Native libraries and JVM JARs must still have matching versions.

## Known gaps

These limits name the roadmap items that address them. Current priorities and
dependencies are recorded in the [central roadmap](../ROADMAP.md).

### Rust HTTP client

The remaining administration routes use `call_json` where no typed method exists.
Typed change feeds, clones, merges, search, reasoning, validation and settings remain
under roadmap item 47. Proactive rate-limit pacing and isolated integration tests for
environment and saved credentials remain under the same item.

### Bindings and packaging

* Public library and binding release compatibility checks remain. Roadmap item 60.
* There is no WebAssembly build of the engine. Roadmap item 44.
* The Python package is not on PyPI and has no PyPy or free-threaded wheels. The JVM
  library is not on Maven Central, and the JavaScript packages are not on npm. Release
  workflows build for Linux on x86_64 and arm64 and for macOS on Apple silicon only, with
  no Windows or Intel macOS packages, and Linux is the platform tested locally. The JVM
  library has no musl build. Roadmap item 58.
* Python async administration and bulk operations remain under roadmap item 58.
  The available async surface covers queries, updates, bounded cursors and transactions.
* JVM functions registered in Java run through ARQ. Roadmap item 25.
* The server and CLI need the host's system CA certificates, even for loopback commands
  such as `sparkles ping`, because the HTTP client loads them when it starts. Minimal
  images such as bare Debian leave them out. Roadmap item 59.

## Configuration and usage

The following guides describe configuration and supported UI, command-line, HTTP
or library workflows. Exact HTTP contracts remain in the API reference.

* Read [Embedding the library](../USAGE.md#embedding-the-library).
* Read [Python](../USAGE.md#python) and [async datasets and cursors](../USAGE.md#async-datasets-and-cursors).
* Read [JVM (Apache Jena)](../USAGE.md#jvm-apache-jena).
* Read [JavaScript and TypeScript](../USAGE.md#javascript-and-typescript).
* Read [Rust client](../USAGE.md#rust-client).

For example, `dataset.schema_typed(None).await?` returns
`sparkles_client::admin_types::SchemaSummary`, and
`dataset.stats_typed(None).await?` returns `DatasetStats`. The blocking client exposes
the same methods. `put_stored_query_typed` accepts a `StoredQuery` model and returns
the stored definition. Generated objects retain unknown members in `extra`, and open
objects retain JSON maps. String enums remain strings so new server values remain
readable. Integer fields use `i128` to retain signed and unsigned 64-bit JSON integers.
Models represent response shapes and do not validate schema bounds or enum values.
`BackupList` uses an untagged enum for pages and dataset listings, with the dataset
listing tried first.

## Roadmap

The remaining work described above refers to [item 25](../ROADMAP.md), [item 44](../ROADMAP.md), [item 47](../ROADMAP.md), [item 58](../ROADMAP.md) and [item 60](../ROADMAP.md).
The central roadmap owns priority and dependencies. Its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs)
link partially complete specs to their remaining work and acceptance criteria.
