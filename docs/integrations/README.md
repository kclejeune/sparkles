# Libraries and integrations

Sparkles embeds directly inside Rust, Python, JVM (Apache Jena), and Node.js applications without a separate server, alongside native Rust (`sparkles-client`) and TypeScript (`@sparkles-rdf/client`) HTTP clients for remote endpoints. The [capability matrix](../FEATURES.md#capability-status) records delivery status for each area.

| Surface | Package / crate | Highlights |
|---|---|---|
| Rust embedded | `sparkles` | Full engine, query builder, catalog, typed handles, and `Control` cancellation. |
| Rust HTTP client | `sparkles-client` | Async (Tokio) and blocking SPARQL, Graph Store, and typed Sparkles admin APIs. |
| Python | `sparkles` (`crates/sparkles-py`) | GIL-releasing sync and `sparkles.aio` async APIs, `rdflib` store plugin, and abi3 wheels. |
| JVM (Apache Jena) | `sparkles-jena` | Jena `DatasetGraph`, transactions, Fuseki assembler, TDB2 import, and `jena-ontapi` bridge. |
| Node.js / TypeScript | `@sparkles-rdf/engine`, `@sparkles-rdf/client` | Node-API native addon with RDF/JS terms, Comunica source, and Fetch HTTP client. |

## Libraries and bindings

All four embedded bindings run the same Rust storage and query engine in process. Thread teardown preserves query callback family checks and the visibility limits of captured callback views.

### Language APIs

* **Rust embedded library (`sparkles`).** `sparkles::Dataset` exposes the full engine alongside `sparkles::querybuilder` ([USAGE.md](../USAGE.md#embedding-the-library), [P06](../specs/P06-library-admin-api.md)). Opening a dataset automatically installs its write-time validation, RDFS on read, stored queries, and GraphQL schema just as the server does. Sub-handles manage snapshots and history, indexes, settings, schema reports, stored queries, reasoning, validation, GraphQL, and backups under one `Control` for cancellation and progress. A catalog manages multiple datasets, clones, and restores. Optional capabilities (reasoner, validators, backups, GraphQL) are cargo features off by default and enabled together via `full`.
* **Python (`sparkles`, `crates/sparkles-py`).** Ships abi3 wheels for CPython 3.10+ on Linux and macOS ([USAGE.md](../USAGE.md#python), [P01](../specs/P01-python-bindings.md)). Covers datasets, streaming file-object bulk loads without temporary copies, SPARQL, transactions, history, search indexes, reasoning, validation, an `rdflib` store plugin, and the query builder. Calls release the GIL and honor `Ctrl-C`. `sparkles.aio` provides async queries, bounded cursors, and transactions with owned cancellation and explicit async cleanup. Synchronous branch `get`, `update`, and `delete` refuse with a conflict if the calling Python thread holds a write transaction on that named branch, leaving the transaction usable.
* **JVM / Apache Jena (`sparkles-jena`).** Provides a Jena `DatasetGraph` with transactions, native query and update execution (falling back to ARQ when needed), bulk loads, historical reads, dumps, typed administration, a Fuseki assembler, and TDB2 import ([USAGE.md](../USAGE.md#jvm-apache-jena)). Passes Jena's contract tests for datasets, graphs, transactions, updates, and prefixes. `DatasetGraphSparkles.algorithms()` manages named graph-algorithm projections, runs queries, returns statistics, and writes results in batched commits (note that a failure mid-write can leave earlier batches committed; callers close returned query executions; roadmap [item 8](../ROADMAP.md)).
* **Node.js and TypeScript (`@sparkles-rdf/*`).** `@sparkles-rdf/engine` embeds Sparkles through Node-API with RDF/JS terms, bounded async results, streams, transactions, administration, and a Comunica source; repeated quad operations reuse cached term handles and result rows decode RDF/JS terms lazily ([USAGE.md](../USAGE.md#javascript-and-typescript)). `@sparkles-rdf/client` provides streaming SPARQL, Graph Store operations, and typed HTTP administration over Fetch. Both share terms, results, and errors via `@sparkles-rdf/common`.
* **Rust HTTP client (`crates/sparkles-client`).** Provides async (Tokio) and blocking clients speaking SPARQL 1.1 and Graph Store protocols to Sparkles, Fuseki, QLever, Oxigraph, or Wikidata, streaming results as `oxrdf` terms ([USAGE.md](../USAGE.md#rust-client), [P02](../specs/P02-rust-client.md)). Against Sparkles servers it adds point-in-time reads, stored queries, uploads, batches, commits, datasets, tasks, backups, statistics, and schema reports. Both forms support per-call cancellation and deadlines, honor `Retry-After` and `RateLimit` headers, and never retry writes with unknown outcomes. Typed methods such as `dataset.schema_typed(None)`, `dataset.stats_typed(None)`, `put_stored_query_typed`, and typed history/snapshot/branch/diff methods (including force and reparent branch deletion) return generated models (`SchemaSummary`, `DatasetStats`, `StoredQuery`, `BackupList`). Models retain unknown fields in `extra`, keep enums as strings for forward compatibility, use `i128` for JSON integers, and do not enforce schema bounds; legacy untyped JSON methods remain unchanged.

### Shared cross-binding capabilities

* **Batch quad membership.** Every embedded binding tests many quads against one snapshot in a single call, returning results in input order: `Dataset::contains_all` (Rust), `Dataset.contains_all` (Python), `DatasetGraphSparkles.containsAll` (JVM), and `Dataset.hasAll` (Node.js) ([USAGE.md](../USAGE.md#python)).
* **R2RML, RML, and YARRRML mappings.** Rust `Dataset::run_mapping_with` and `StoredMappings::run` accept `guard::WriteOptions` so cancellation, deadlines, graph permissions, preconditions, previews, and commit metadata apply across both conversion and final loader publication (while `Dataset::run_mapping` and `RunOptions.check` remain unchanged for conversion-only checks). JVM `DatasetGraphSparkles.mappings()` and Node.js `Dataset.mappings` manage stored mappings and run inline or stored mappings with cancellation and deadlines through conversion and loading (default JVM and Node.js builds exclude SQLite and Parquet sources). Python `Dataset.run_mapping` and `Dataset.mappings.run` do not accept cancellation tokens, timeouts, or `Ctrl-C`.
* **OWL 2 ontology handles and `jena-ontapi`.** Python (`Dataset.ontology`), Node.js (`dataset.ontology`), and JVM (`DatasetGraphSparkles.ontology()`) expose ontology summaries, axiom listings, profile checks, edits, entity removal, and commit diffs in the `/$/ontology` JSON format ([API.md](../API.md#ontology)). Python wraps axioms in frozen hashable `Axiom` objects; Node.js streams paged axioms as an async iterable pinned to the first page's commit; all three accept native cancellation and timeouts (`cancel`/`timeout`, operation options, or `SparklesOperation`). On the JVM, `SparklesOnt.model(dataset, graphIri, spec)` copies a graph in one native call into a Jena `OntModel` (requires the caller to add `org.apache.jena:jena-ontapi` matching its Jena version) so writes commit atomically inside the thread's Sparkles write transaction and roll back on abort. Like standard Jena in-memory models, `OntModel` instances require external locking for concurrent threads and do not observe external commits made through other handles.

### Runtime ownership, locking, and recovery

* **Process lock and branch handles.** Persistent stores and their `BranchSet` share ownership of `sparkles.lock` at the dataset root. Dropping the main store keeps the lock held until every retained, removed, or retired branch `Store` and `BranchSet` is destroyed; release all handles before reopening the directory. In-memory branches share runtime ownership without a file lock. Store shutdown completes pending embedding record output even if a caller retains an `Embedder`, and a panicking validation callback still revokes retained writers and flushes output before unwinding.
* **Catalog disposal and `Catalog::defer`.** Catalog deletions, persistent renames, and backup replacements require exclusive runtime ownership. Any retained strong or weak alias to a `DatasetState`, branch `Store`, or `BranchSet` causes disposal to fail safely without altering files, while in-place restores retry until their drain window expires. Built-in workers and cached branches use revocable links so idle branches close cleanly. Close callbacks run after catalog lookup locks drop and may look up datasets; re-entrant management calls (`create`, `delete`, `rename`) on the disposing thread fail with `catalog-disposing`, whereas `Catalog::defer` queues idempotent in-memory callbacks to run on the same thread right after disposal finishes.
* **Failure recovery and UUID resolution.** If an I/O error occurs after runtime close commits, recovery verifies the dataset's UUID and `CURRENT` before reopening. Managed persistent deletes record a versioned `delete.json` intent and synchronize the staged root before removing its UUID from `config.json`. Catalog startup validates the journal before recovery and requires the dataset process lock before cleaning up a committed delete. It preserves failed rollbacks and warns about retained bytes. The [operations guide](../operations/README.md#interrupted-dataset-deletes) describes the recovery and refusal conditions. `Catalog::open` isolates datasets with missing directories, missing `CURRENT`, or unreadable formats while serving all healthy datasets ([API.md](../API.md#startup)). `sparkles::evidence::resolve::Resolution` locates datasets and branches by UUID without opening cold branch stores; callers enforce access checks on current names first, and durable source holds (not live handles) keep referenced commits retained.
* **API compatibility.** Covered package roots follow the [API compatibility policy](../DEVELOPMENT.md#api-compatibility). HTTP and standard CLI baselines are checked in development; public Rust and binding release comparisons remain planned (roadmap [item 60](../ROADMAP.md)). Native libraries and JVM JARs must match versions.

## Configuration and usage

| Topic | Reference |
|---|---|
| Embedded Rust library | [Embedding the library](../USAGE.md#embedding-the-library) |
| Python sync and async APIs | [Python](../USAGE.md#python), [Async datasets and cursors](../USAGE.md#async-datasets-and-cursors) |
| JVM and Apache Jena | [JVM (Apache Jena)](../USAGE.md#jvm-apache-jena) |
| JavaScript and TypeScript | [JavaScript and TypeScript](../USAGE.md#javascript-and-typescript) |
| Rust HTTP client | [Rust client](../USAGE.md#rust-client) |

## Known gaps

Each limit below cites the [central roadmap](../ROADMAP.md) item tracking it.

### Rust HTTP client

Administration routes without dedicated typed methods still use `call_json`. Typed change feeds, clones, merges, search, reasoning, validation, and settings methods, proactive rate-limit pacing, and isolated integration tests for environment and saved credentials remain planned (roadmap [item 47](../ROADMAP.md)).

### Bindings and packaging

* **Package publication and platforms.** None of the Python (PyPI), JVM (Maven Central), or Node.js (npm) packages are published to public registries. Release workflows target Linux (`x86_64`, `arm64`) and Apple silicon macOS only, with local testing on Linux and no Windows, Intel macOS, JVM musl, Python PyPy, or free-threaded Python wheels (roadmap [item 58](../ROADMAP.md)). No WebAssembly build of the database engine exists (roadmap [item 44](../ROADMAP.md)). Public Rust and binding release compatibility checks remain planned (roadmap [item 60](../ROADMAP.md)).
* **System CA certificates.** The server and CLI load host system CA certificates on startup even for loopback commands such as `sparkles ping`, which fails in minimal images without `ca-certificates` (roadmap [item 59](../ROADMAP.md)).
* **Python gaps.** Python mapping runs (`run_mapping`, `mappings.run`) lack cancellation tokens, timeouts, and `Ctrl-C` support (roadmap [item 404](../ROADMAP.md)). `branches.update(name, note=None)` leaves existing notes unchanged rather than clearing them (roadmap [item 755](../ROADMAP.md)). Async administration and bulk operations remain planned beyond queries, updates, bounded cursors, and transactions (roadmap [item 58](../ROADMAP.md)).
* **JVM and branch-ownership gaps.** Java functions registered through Jena run in ARQ rather than natively in Sparkles (roadmap [item 25](../ROADMAP.md)). Logical caller ownership checks for branch administration remain incomplete for Node.js manual transactions, raw FFI calls, nested upstream dependencies, Python/Node.js branch listings, and deletion cleanup beside unrelated transaction workers (roadmap [item 754](../ROADMAP.md)).

## Roadmap

Core design specifications for this area are [P01](../specs/P01-python-bindings.md), [P02](../specs/P02-rust-client.md), [P03](../specs/P03-query-extensions.md), [P04](../specs/P04-jvm-bindings/SPEC.md), [P05](../specs/P05-node-bindings.md), and [P06](../specs/P06-library-admin-api.md). Binding measurements live in [docs/benchmarks/bindings/RESULTS.md](../benchmarks/bindings/RESULTS.md).

The central [roadmap](../ROADMAP.md) owns priorities and dependencies, and its [completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs) cover items 8, 25, 44, 47, 58, 59, 60, 404, 754, and 755.
