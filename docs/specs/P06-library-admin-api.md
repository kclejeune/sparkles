# P06: Library administration API and parity

> **Status:** Phases 1, 2 and 3 implemented.
>
> **Phases:** The `sparkles` facade exposes `Dataset`, its administration handles and
> `Catalog`. The server uses the catalog for dataset registration and reservations,
> and the library owns cloning, backup capture, restore download and publication,
> repository persistence, policy execution and retention. Branches remain under each
> dataset and share lazily opened library state. The parity test maps every OpenAPI
> operation to a checked library call or an explicit server-only reason. `bindings.toml`
> records the shipped Python and JVM names and planned phases for missing bindings;
> the Python parity test resolves every concrete name through the extension stub and
> native module. Python exposes property handles, backups, GraphQL and branches.
> Dataset rename is available through HTTP, and the dataset CLI supports stopped
> catalogs and running servers. Startup measurements retain eager catalog opening.
>
> **User docs:** the "Embedding the library" section of [USAGE](../USAGE.md) describes
> what `Dataset::open` sets up, the handles and the query defaults with their override
> rule. Its "Stored queries" section and its notes on the local tools say that
> `sparkles queries run` and `sparkles query --loc` follow the dataset's RDFS on read,
> materialized inferences and DESCRIBE setting. The Python guide documents the new
> handles, and the API guide documents dataset rename. Existing file formats are
> unchanged.
>
> This is the design as written before implementation. The [Outcome](#outcome) section
> at the end records how it lands.

This spec draws on the Sparkles code, the checked-in OpenAPI description
(`docs/openapi.json`, from [X03](X03-openapi-and-completions.md)), the UI's API modules,
the specs of the Python bindings ([P01](P01-python-bindings.md)), the Rust client
([P02](P02-rust-client.md)) and the JVM bindings ([P04](P04-jvm-bindings.md)), the Rust
API Guidelines, the Cargo reference and the PyO3 guide. The [Sources](#14-sources)
section lists them.

The design depends on these parts of Sparkles:

* the embedded API in `crates/sparkles/src/dataset.rs` and the `Store` in
  `crates/sparkles-core/src/store.rs` with its modules in `store/`;
* the core modules `sparkles::history`, `sparkles::schema`, `sparkles::stored`,
  `sparkles::guard`, `sparkles::geo`, `sparkles::text` and `sparkles::vector`;
* the crates `sparkles-backup`, `sparkles-reasoner`, `sparkles-shacl`, `sparkles-shex`,
  `sparkles-graphql` and `sparkles-fmt`;
* the server's registry and task system in `crates/sparkles-server/src/state.rs`, and
  the handlers in `http.rs`, `http/`, `backup/`, `reasoning.rs`, `rdfs.rs`,
  `write_validation.rs`, `clone.rs`, `geo.rs`, `vector.rs` and `compaction.rs`;
* the route coverage test in `crates/sparkles-server/src/openapi/tests.rs` and the
  client's operation table in `crates/sparkles-client/src/routes.rs`;
* the Python bindings in `crates/sparkles-py`, their stub `_sparkles.pyi` and their
  `stubtest` check.

## 1. Summary, goals, non-goals

Sparkles has three ways in. The server offers 192 HTTP operations. The Rust library
offers `sparkles::Dataset`, which the crate documents as its entry point, and `Store`,
which `ds.store()` returns. The Python package wraps part of the library. The three
grew one feature at a time, and each feature landed in whichever layer its first user
needed.

The result is uneven. `Dataset` covers queries, updates, loads, dumps, transactions,
commits, compaction, an N-Quads backup and RDF Patch. `Store` has far more, including
named snapshots, retention, diffs, the change feed and change log, clones, compaction
settings, quotas, DESCRIBE settings, the write guard's hooks, the text, vector and
spatial indexes, and embeddings. Backups, reasoning, SHACL, ShEx and GraphQL live in
their own crates, and the schema reports and stored queries are free functions and
types in core modules. Some behaviour lives only in the server. That includes the
catalog of datasets, the reasoning record and its freshness, installing the write guard
when a dataset opens, RDFS on read, dataset statistics, the schema report cache, the
backup repository registry and running a backup policy.

Three consequences follow. A Rust program cannot do what the server does without
copying server code. A Rust program that opens a dataset whose `validation.json` asks for
a guard gets `GuardMissing` on every write, because only the server and the Python
binding install guards, and the Python binding does it with its own copy of the
server's code. The Python bindings expose about a third of the operations, and the JVM
and Node bindings are about to repeat the exercise with their own choices.

This spec makes `Dataset` and a small set of handle objects the supported embedding
API, adds a `Catalog` for several datasets in one data directory, moves the server onto
that API, and adds tests that keep the HTTP API, the library and the bindings in step.

**Goals**

* **One curated surface.** Every operation of the HTTP API that is not tied to HTTP,
  identities or the server's schedulers has a library call reached from `Dataset`, a
  handle of it, `Catalog`, or a named free function.
* **The server is a client of the library.** Each such handler parses the request,
  checks the caller's permissions, calls the library and renders the result. Behaviour
  that is not about HTTP moves out of the handlers.
* **Opening a dataset is the same everywhere.** `Dataset::open` installs the write
  guard, loads RDFS on read, and opens the stored queries, the GraphQL configuration and
  the reasoning record, as the server does today.
* **One cancellation and progress type.** Long-running calls take a `Control`, which
  the server's task system builds from a task and which the bindings build from their
  own cancel tokens and callbacks.
* **Parity is tested.** A test maps every OpenAPI operation to a library call or to a
  stated server-only reason. A second table maps every library call to its name in each
  binding, and the Python test checks the names against the stub. A failure names what
  to add.
* **The bindings inherit the surface.** The Python gaps close in Phase 2, and P04 and
  P05 bind the handles rather than inventing their own administration methods.

**Non-goals**

* **New behaviour.** Apart from dataset renames, which no layer has today, every call
  here exists somewhere already. The work moves and names it.
* **A remote client.** `sparkles-client` ([P02](P02-rust-client.md)) talks to servers.
  The handles run in-process.
* **Changing the HTTP API.** The refactoring keeps every route, body and status code,
  and the checked-in OpenAPI description does not change in Phase 1.
* **Stabilizing `Store`.** `Store` stays public for the server, the satellite crates and
  tests, and its interface may change in any release.
* **Access control in the library's handles.** The library enforces the graph views and
  protections it is given ([C12](C12-graph-access-control.md),
  [C12b](C12b-triple-access-control.md)), but deciding who may do what stays with the
  caller (§6.1).

## 2. Inventory

The inventory below was taken from `docs/openapi.json` at commit `3006e6f8`, which
describes 192 operations. The UI's API modules (`ui/src/lib/api.ts`,
`ui/src/lib/auth-api.ts` and `ui/src/lib/backups.ts`) call only operations from this
list, so the inventory covers the UI as well.

Each row names the operations by their `operationId`. The column *Today* gives the
library entry point that does the work now. *Partial* means a library call does the core
of the work and the handler adds state or behaviour that the library lacks. *Server only*
means no library call exists. The column *Python* gives the method of the Python
bindings that performs the operation, if any. The last column gives the library call
after this spec, or `server-only` with the reason in §6.

### 2.1 Datasets and the catalog

| Operations | Today | Python | After P06 |
|---|---|---|---|
| `listDatasets`, `getDataset` | Server only, in `AppState` | No | `Catalog::list`, `Catalog::info` |
| `createDataset` | Server only | No | `Catalog::create` |
| `deleteDataset` | Server only | No | `Catalog::delete` |
| `cloneDataset` | Partial. `Store::clone_to` copies the data, and the server writes `origin.json` and rebases the reasoning record. | Partial. `clone_to` writes a directory. | `Catalog::clone_dataset` over `Dataset::clone_to` |
| `setDatasetState` | Server only | No | server-only |
| `getDatasetStats`, `getDatasetStatsPost` | Server only. The handler computes the counts. | No | `Dataset::stats` |
| `clearCache` | `Store::result_cache` | No | `Dataset::clear_cache` |
| `compactDataset` | `Dataset::compact`, `Store::compact_with` | `compact` | `Dataset::compact_with` |
| `getCompaction`, `setCompaction`, `deleteCompaction` | `Store::compaction_settings` and `set_compaction_settings` | No | `ds.settings().compaction()` |
| `getQuota`, `setQuota`, `deleteQuota` | `Store::quota` and `set_quota` | No | `ds.settings().quota()` |
| `getAllPrefixes`, `getPrefixes`, `addPrefix`, `putPrefix` | `Dataset::prefixes` and `set_prefix` | `prefixes`, `set_prefix` | unchanged |
| `deletePrefix` | `Store::remove_prefix` | No | `Dataset::remove_prefix` |
| `listBackupFiles`, `listBackupFilesPost` | Server only | No | `Catalog::backup_files` |

### 2.2 SPARQL and the Graph Store Protocol

| Operations | Today | Python | After P06 |
|---|---|---|---|
| `queryGet`, `queryPost`, `sparqlQueryGet`, `sparqlQueryPost`, `datasetGet`, `datasetPost` | `Dataset::query_with` and `update_with` | `query`, `select`, `ask`, `construct` | unchanged |
| `update` | `Dataset::update_with` | `update` | unchanged |
| `explainGet`, `explainPost` | `sparkles::sparql::explain` on a snapshot | No | `Dataset::explain` |
| `getDescribe`, `setDescribe`, `deleteDescribe` | `Store::describe_settings` and `set_describe_settings` | Per query only, through `describe=` | `ds.settings().describe()` |
| `runStoredQuery`, `runStoredQueryPost` | `sparkles::stored::Catalog` and `Query::bind`, put together in the handler | No | `ds.queries().run` |
| `gspGet`, `gspReadGet`, `directGraphGet`, `gspHead`, `gspReadHead`, `directGraphHead`, `datasetHead` | `Dataset::dump` and `named_graph` | `dump(from_graph=…)` | `Dataset::dump_graph` |
| `gspPost`, `directGraphPost`, `upload` | `Dataset::load_sources_receipt` | `load(to_graph=…)` | unchanged |
| `gspPut`, `directGraphPut`, `datasetPut` | `Store::replace_as` | Partial. A clear and a load are two commits. | `Dataset::replace` |
| `gspDelete`, `directGraphDelete`, `datasetDelete` | `GraphView::clear` | `clear_graph`, `clear` | `GraphView::clear`, `Dataset::clear` |
| `patch`, `patchPost` | `Dataset::apply_patch` | `apply_patch` | unchanged |

### 2.3 History and snapshots

| Operations | Today | Python | After P06 |
|---|---|---|---|
| `listCommits` | `Dataset::commits` | `commits` | `ds.history().commits` |
| `getCommit` | `Store::commit` and `annotation` | No | `ds.history().commit` |
| `getHistory` | `Store::history` | `history` | `ds.history().status` |
| `setHistory` | `Store::set_retention`, `set_schedules`, `set_catalog_horizon` and `set_change_log_settings` | Partial. `set_retention` sets the retention window only. | `ds.settings().retention()`, `ds.settings().change_log()` |
| `listSnapshots`, `getSnapshot`, `createSnapshot`, `deleteSnapshot` | `Store::snapshots`, `named_snapshot`, `create_snapshot_opts` and `delete_snapshot` | `snapshots`, `create_snapshot`, `delete_snapshot`. There is no `get`. | `ds.snapshots()` |
| `diff` | `Store::diff` | No | `ds.history().diff` |
| `changes` | `Store::changes`. The long poll is in the handler. | No | `ds.history().changes`, `wait_for_commit` |
| `historyChanges` | `Store::history_changes` | No | `ds.history().query` |

### 2.4 Search indexes

| Operations | Today | Python | After P06 |
|---|---|---|---|
| `getTextIndex`, `setTextIndex`, `deleteTextIndex`, `rebuildTextIndex` | `Store::text_status`, `enable_text`, `disable_text` and `rebuild_text` | `text_status`, `enable_text`, `disable_text`, `rebuild_text` | `ds.indexes().text()` |
| `textSearch`, `textSearchPost` | Partial. The handler writes a `text:query` SPARQL query and formats the hits. | No | `ds.indexes().text().search` |
| `getVectorIndexes`, `getVectorIndex`, `putVectorIndex`, `deleteVectorIndex`, `rebuildVectorIndex`, `reembedVectorIndex` | `Store::vector_indexes`, `vector_index`, `create_vector_index`, `drop_vector_index`, `rebuild_vector_index` and `reembed` | `vector_indexes`, `vector_index`, `create_vector_index`, `drop_vector_index`, `rebuild_vector_index`, `reembed_vector_index` | `ds.indexes().vector()` |
| `measureVectorRecall` | `Store::vector_recall` | No | `ds.indexes().vector().recall` |
| `getGeoIndex`, `setGeoIndex`, `deleteGeoIndex`, `rebuildGeoIndex` | `Store::geo_status`, `enable_geo`, `disable_geo` and `rebuild_geo` | No | `ds.indexes().geo()` |
| `geoFeatures` | `sparkles::geo::map::features_in_box_of` | No | `ds.indexes().geo().features` |
| `convertGeometries` | `sparkles::geo::convert::convert` | No | unchanged, a free function |

### 2.5 Schema

| Operations | Today | Python | After P06 |
|---|---|---|---|
| `getSchema`, `listSchemaClasses`, `listSchemaPredicates` | Partial. `sparkles::schema::discover` computes the report. The server keeps the cache, the report maintained from the changes, and the cursors. | No | `ds.schema().report`, `classes`, `predicates` |
| `getSchemaConstraints` | Partial. `ConstraintsLayer` and `sparkles_shacl::constraints` exist, and the handler assembles the layer. | No | `ds.schema().constraints` |
| `getSchemaDiff` | Partial. `sparkles::schema::compare` compares two reports that the handler computes. | No | `ds.schema().diff` |
| `getClassProfiles` | `sparkles::schema::profile::profiles` | No | `ds.schema().profiles` |
| `draftShapes` | `sparkles::schema::draft::draft_shapes` | No | `ds.schema().draft_shapes` |

### 2.6 Stored queries and GraphQL

| Operations | Today | Python | After P06 |
|---|---|---|---|
| `listStoredQueries`, `getStoredQuery`, `putStoredQuery`, `deleteStoredQuery`, `listStoredQueryVersions` | `sparkles::stored::Catalog`, which the server opens for each dataset | No | `ds.queries()` |
| `getGraphqlConfig`, `putGraphqlConfig`, `deleteGraphqlConfig`, `listGraphqlVersions` | `sparkles_graphql::Catalog`, which the server opens for each dataset | No | `ds.graphql().config()`, `versions` |
| `draftGraphqlSchema` | `sparkles_graphql::draft` | No | `ds.graphql().draft` |
| `graphqlGet`, `graphqlPost`, `graphqlApiSchema` | `sparkles_graphql::execute` and the compiled schema | No | `ds.graphql().execute`, `sdl` |

### 2.7 Reasoning and validation

| Operations | Today | Python | After P06 |
|---|---|---|---|
| `reason` | Partial. `sparkles_reasoner` runs the rules in full or incrementally. The server keeps the closure cache, records the inputs and writes `reasoning.json`. | Partial. `reason` runs the rules and records nothing. | `ds.reasoning().run` |
| `getReasoning` | Server only. Freshness is computed in `reasoning.rs`. | No | `ds.reasoning().status` |
| `clearReasoning` | Partial. `sparkles_reasoner::clear` removes the inferences, and the server clears the record. | `clear_inferences` | `ds.reasoning().clear` |
| `reasoningDiagnostics` | Partial. `sparkles_reasoner::diagnostics` finds them, and the server assembles the answer. | No | `ds.reasoning().diagnostics` |
| `getRdfs`, `setRdfs`, `deleteRdfs` | Partial. `sparkles::sparql::rdfs::RdfsOnRead` exists, and the server persists and loads it. | No | `ds.reasoning().rdfs()` |
| `setAutoReasoning`, `clearAutoReasoning` | Server only | No | server-only |
| `getWriteValidation`, `setWriteValidation`, `deleteWriteValidation` | Partial. The guards are in `sparkles-shacl` and `sparkles-shex`. Installing them and their status are in the server, and the Python binding has its own copy. | `write_validation`, `set_write_validation` | `ds.validation().guard()` |
| `shacl` | `sparkles_shacl::validate` | `validate_shacl` | `ds.validation().shacl` |
| `shex` | `sparkles_shex::validate` | `validate_shex` | `ds.validation().shex` |

### 2.8 Backups

| Operations | Today | Python | After P06 |
|---|---|---|---|
| `backupNquads` | `Dataset::backup` | `backup` | unchanged |
| `listDatasetBackups`, `getBackup`, `deleteBackup` | `sparkles_backup::Repository::list`, `manifest` and `delete` | No | `ds.backups(&repo).list`, `get`, `delete` |
| `createBackup` | `Repository::create` over `Store::backup_capture` | No | `ds.backups(&repo).create` |
| `verifyBackup` | `Repository::verify` | No | `ds.backups(&repo).verify` |
| `restoreBackup` | Partial. `Repository::restore` writes a directory, and the server swaps it in or registers it. | No | `Catalog::restore` |
| `listRepositories`, `getRepository`, `createRepository`, `updateRepository`, `deleteRepository` | Server only. The registry is `backup/repositories.json`. | No | `catalog.repositories()` |
| `listRepositoryBackups`, `collectRepository`, `listRepositoryLocks`, `breakRepositoryLock`, `testRepository`, `verifyRepository` | `Repository::list`, `gc`, `locks`, `break_lock`, `test` and `verify` | No | `sparkles::backup::Repository`, with blocking forms (§4.8) |
| `previewSchedule` | `sparkles_backup::policy::next_runs` | No | unchanged, a free function |
| `runPolicy` | Server only | No | `Catalog::run_policy` |
| `applyRetention` | Partial. `policy::retention` plans the deletions, and the server makes them. | No | `Catalog::apply_retention` |
| `listPolicies`, `getPolicy`, `createPolicy`, `updatePolicy`, `deletePolicy`, `listPolicyRuns` | Server only | No | server-only |

### 2.9 Stateless utilities

| Operations | Today | Python | After P06 |
|---|---|---|---|
| `validateQuery`, `validateQueryPost` | `sparkles::sparql::parse_query` | No | unchanged |
| `validateUpdate`, `validateUpdatePost` | `sparkles::sparql::update::parse_update` | No | unchanged |
| `validateData`, `validateDataPost` | Server only. The handler has its own parser loop. | No | `sparkles::io::check_data` |
| `validateIri`, `validateIriPost`, `validateLangtag`, `validateLangtagPost` | Server only, in `tools::terms` | No | `sparkles::terms::check_iri`, `check_langtag` |
| `format`, `lint` | `sparkles_fmt` | No | `sparkles::fmt`, behind the `fmt` feature |

### 2.10 Server-only operations

| Operations | Reason (§6) |
|---|---|
| `login`, `logout`, `authConfig`, `whoami`, `tokenGrant`, `authorizeCli`, `deviceAuthorization`, `getDeviceLogin`, `approveDeviceLogin`, `denyDeviceLogin`, `oidcLogin`, `oidcCallback`, `oidcBackchannelLogout` | Identities and credentials (§6.1) |
| `listTokens`, `createToken`, `revokeToken`, `revokeOwnerTokens` | Identities and credentials (§6.1) |
| `listTasks`, `getTask`, `cancelTask` | The task queue (§6.4) |
| `listPolicies`, `getPolicy`, `createPolicy`, `updatePolicy`, `deletePolicy`, `listPolicyRuns` | Schedulers (§6.5) |
| `setAutoReasoning`, `clearAutoReasoning` | Schedulers (§6.5) |
| `setDatasetState` | HTTP routing state (§6.3) |
| `fusekiStats`, `fusekiStatsPost` | Process and HTTP state (§6.6) |
| `mcpPost`, `mcpStream`, `mcpEndSession` | The MCP transport (§6.7) |
| `ping`, `pingPost`, `getServer`, `getServerPost`, `metrics`, `ready`, `readyDataset` | Process and HTTP state (§6.6) |
| `openapiJson`, `openapiYaml` | Process and HTTP state (§6.6) |

### 2.11 Counts

| | Operations |
|---|---|
| Operations in `docs/openapi.json` | 192 |
| Server-only by design | 43 |
| Mapped to a library call after P06 | 149 |
| In the library today and complete | 109 |
| … through `Dataset` | 29 |
| … through `Store` | 39 |
| … through functions of core modules | 17 |
| … through other crates | 24 |
| In the library today in part | 19 |
| In the server only today | 21 |
| Complete in Python today | 49 |
| Partial in Python today | 6 |

Of the 149 operations that belong in the library, 40 need work beyond naming, and 94 are
missing from Python.

## 3. Crates and features

### 3.1 Why the engine splits

The handles need types from crates that depend on the engine. `ds.backups(&repo)` returns
`sparkles_backup` types, `ds.validation().shacl(…)` returns a
`sparkles_shacl::ValidationReport`, and `ds.graphql()` needs `sparkles_graphql`. Each of
those crates depends on `sparkles`, so `sparkles` cannot depend on them, and Rust allows
inherent methods only in the crate that defines the type.

The engine therefore moves to a new package, and the `sparkles` name becomes a facade
over it:

```
crates/sparkles-core/     the engine, moved unchanged from crates/sparkles
                          (package sparkles-core, lib name sparkles_core)
crates/sparkles/          the facade (package sparkles): Dataset, the handles,
                          Catalog, Control, the query builder, and re-exports
crates/sparkles-backup/   depends on sparkles-core
crates/sparkles-reasoner/ depends on sparkles-core
crates/sparkles-shacl/    depends on sparkles-core
crates/sparkles-shex/     depends on sparkles-core
crates/sparkles-graphql/  depends on sparkles-core
crates/sparkles-server/   depends on the facade
crates/sparkles-py/       depends on the facade
```

The facade re-exports every public module of the core under its present name
(`pub use sparkles_core::{access, annotations, builder, …, store, stored, …}`), so a path
such as `sparkles::store::Store` or `sparkles::sparql::QueryOptions` keeps working in the
server, the bindings and user code. The types are the same types, so the server can pass
a `&sparkles::store::Store` to `sparkles_reasoner::materialize`.

Three things move from the core to the facade. `dataset.rs` moves with `Dataset`,
`Transaction`, `GraphView`, `QuadIter` and `Solutions`. The `querybuilder` module moves
because its `execute` methods take a `&Dataset`. The core's tests that build a `Dataset`
(in `geo/`, `store/geo.rs` and the geometry file tests) switch to `Store` or move to the
facade's tests. The path `crates/sparkles/src/dataset.rs`, which P01 and P04 cite, is
the same after the move.

The satellite crates depend on `sparkles-core` and change their `sparkles::` paths to
`sparkles_core::` in one mechanical commit. A satellite that depended on the facade would
form a cycle. P04's planned `sparkles::embed` module, which holds the transaction worker
shared by the bindings, belongs in the facade as well.

Log targets keep their names too. A `tracing` event without a `target:` takes its module
path, which the move changed from `sparkles::store` to `sparkles_core::store`. That broke
filters such as `RUST_LOG=sparkles::store=debug` and renamed the targets in JSON logs.
Every event and span in the core now names its target, the module path it had in the
`sparkles` package. A test in `crates/sparkles-core/tests/log_targets.rs` fails on a new
event without one, and checks that a `sparkles::store` directive enables a store event.
Rewriting `sparkles::` directives when the server builds its filter would have fixed
filtering but not the names in JSON logs or OTLP records, and a crate-wide macro cannot
compute `sparkles::` plus the rest of `module_path!()` as the constant a target must be.

### 3.2 Cargo features

The facade's features name what they turn on. The core's features keep their names, and
the facade forwards them.

| Feature | Turns on | What it gates |
|---|---|---|
| `text` | `sparkles-core/text` | the full-text index. Without it the text methods exist and fail with `Unsupported`, as they do today. |
| `geo`, `geo-proj4`, `geo-epsg` | the core's features of the same names | the spatial index, with the same rule as `text` |
| `zstd`, `brotli` | the core's codecs | compressed inputs, dumps and backups |
| `reasoning` | `sparkles-reasoner` | `ds.reasoning().run`, `clear`, `diagnostics` and `status`. RDFS on read is in the core and needs no feature. |
| `shacl` | `sparkles-shacl` | `ds.validation().shacl`, SHACL guards and `ds.schema().constraints` from shapes graphs |
| `shex` | `sparkles-shex` | `ds.validation().shex` and ShEx guards |
| `backup` | `sparkles-backup` with `fs` | `ds.backups`, `Catalog::restore`, `run_policy`, `apply_retention`, `repositories` and `sparkles::backup` |
| `backup-s3`, `backup-gcs`, `backup-azure` | the matching features of `sparkles-backup` | object storage repositories |
| `graphql` | `sparkles-graphql` | `ds.graphql()` |
| `fmt` | `sparkles-fmt` | `sparkles::fmt::{format, lint}` |
| `full` | all of the above except `geo-epsg` and the cloud backends other than S3 | a convenience for programs that want what the server has |

The default stays empty, as the library's default is today. A method whose types come
from an optional crate does not exist without its feature, so a Rust program that calls
it fails to compile with an error that names the method. The bindings keep P01's rule
instead. They export every method and fail with `Unsupported` naming the feature, because
their users cannot recompile.

The server depends on the facade with the features it builds today, and its own features
(`reasoning`, `shacl`, `backup` and the others) forward to the facade's.

## 4. The curated surface

### 4.1 Conventions

`Dataset` is the supported embedding API. Its rustdoc and `docs/USAGE.md` lead with it,
its handles and `Catalog`. `Store` stays public and is documented as the engine's
low-level interface, which the server and the satellite crates use and which may change
between releases.

The handles follow these rules.

* **Handles are nouns.** `ds.snapshots()`, `ds.history()` and the others return a
  handle that holds a clone of the `Dataset`, which is an `Arc`. Handles are `Clone`,
  `Send`, `Sync` and `'static`, so a program can keep one or move it to another thread,
  and a binding can wrap one as an object.
* **The verbs are fixed.** `list` returns everything, `get` returns `Option` for a
  missing item, `create` fails with `Error::Conflict` when the item exists, `put` creates
  or replaces, and `delete` returns whether the item existed. Settings have `get`, `set`
  and `reset` (§4.10). Indexes have `status`, `enable`, `disable` and `rebuild`. An
  operation that runs a computation is `run`.
* **Long-running calls have two forms.** `op(…)` runs without cancellation or progress,
  and `op_with(…, &Control)` takes a [`Control`](#42-control-cancellation-and-progress).
  This follows the existing pairs `load` and `load_with`, and `compact` and
  `compact_with`.
* **Results are typed.** Results are structs and enums that derive `Debug`, `Clone` and
  `Serialize`. Their serde form is the camelCase JSON of the HTTP API, so a handler can
  serialize a result and add only its HTTP fields, and the bindings can turn a result into
  a dict or a JSON object with the shape `docs/API.md` describes. New result types are
  `#[non_exhaustive]`. Option structs implement `Default` and are not
  `#[non_exhaustive]`, because that would forbid `..Default::default()` outside the crate.
* **Calls block.** The library is synchronous, as it is today. An async program calls it
  from `spawn_blocking`, as the server does. The backup calls block too, although
  `sparkles-backup` is async inside (§4.8).
* **Views come from the caller.** A handle method that reads data takes the same
  `graphs: Option<Arc<GraphAccess>>` field that the store's option structs take today
  (`ChangesOptions`, `DiffOptions`, `HistoryQuery`, the schema options). The server
  computes the field from the caller's grants, and an embedder leaves it `None`.

### 4.2 Control: cancellation and progress

Four crates define the same progress callback type today (`sparkles::builder`,
`sparkles::store`, `sparkles_backup` and `sparkles_reasoner`), and cancellation is an
`Option<Arc<AtomicBool>>` in `CompactOptions`, `CloneOptions`, `ReasonOptions` and
`WriteOptions`, or `sparkles_backup::Ctl`. They become one module of the core,
re-exported as `sparkles::task`:

```rust
pub type ProgressFn = Arc<dyn Fn(f32, &str) + Send + Sync>;

#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);
impl Cancel {
    pub fn new() -> Cancel;
    pub fn from_flag(flag: Arc<AtomicBool>) -> Cancel;
    pub fn cancel(&self);
    pub fn is_cancelled(&self) -> bool;
    pub fn check(&self) -> Result<()>;          // Err(Error::Cancelled) once cancelled
}

#[derive(Clone, Default)]
pub struct Progress(Option<ProgressFn>);
impl Progress {
    pub fn new(f: impl Fn(f32, &str) + Send + Sync + 'static) -> Progress;
    pub fn report(&self, fraction: f32, message: &str);   // clamped to [0, 1]
    pub fn part(&self, from: f32, to: f32) -> Progress;   // a step of a larger operation
}

#[derive(Clone, Default)]
pub struct Control {
    pub cancel: Cancel,
    pub progress: Progress,
    pub deadline: Option<Instant>,
}
impl Control {
    pub fn none() -> Control;
    pub fn check(&self) -> Result<()>;          // Cancelled, or Timeout past the deadline
    pub fn part(&self, from: f32, to: f32) -> Control;
}
```

The rules for every long-running call are these.

* The fraction never decreases within one call and ends at 1.0 on success. A call made of
  steps gives each step a `part` of the range, so a restore that downloads and then checks
  reports 0 to 0.8 and then 0.8 to 1.
* Messages are short phrases in the present tense, such as `building POS` or `uploading 3
  of 12 files`, because the server shows them as the task's message.
* The callback runs on the thread doing the work. It must be cheap and must not call back
  into the same dataset's writer. The library does not throttle reports, and a binding
  that crosses into another runtime throttles them itself (§9.4).
* A cancelled call fails with `Error::Cancelled` and publishes nothing, as cancelled
  writes and compactions do today. Calls check at the points where they check today, which
  are between blocks of a build, every 65,536 quads of a clone, and between object
  requests and every 8 MiB of a backup.

In Phase 1 the existing option structs keep their `cancel` and `progress` fields, and
`Control` fills them, so the change does not touch every caller at once.
`sparkles_backup::Ctl` becomes a re-export of `Control`, and its `Code::Cancelled` error
converts to `Error::Cancelled`.

The server's `TaskHandle` gains `control(&self) -> Control`. Its `Cancel` wraps the task's
existing cancel flag, which `DELETE /$/tasks/{id}` sets, and its `Progress` writes the
task's `progress` and `message`. The task queue keeps its slots, ids and listing (§6.4),
and the work it runs is a library call with that `Control`.

### 4.3 Errors

Handle methods return `sparkles::Result<T>`, whose error is the core's `Error`. Errors from
the satellite crates convert at the handle's boundary:

* cancellation and budgets become `Error::Cancelled` and `Error::BudgetExceeded`, so a
  program has one way to recognize them;
* parse errors of shapes, ShEx schemas, rules and GraphQL documents become `RdfParse`,
  `Invalid` or `SparqlSyntax`, as their nature is;
* everything else becomes a new variant, `Error::Component(Box<ComponentError>)`. A
  `ComponentError` carries the component's name (`backup`, `reasoner`, `graphql`), a
  stable code such as `repository-locked` or `not-a-repository`, a message, and the
  original error as its `source`, which a program can downcast to `BackupError` or
  `PutError` for the details.

Two more changes make errors usable across the bindings. A directory lock held by another
process becomes `Error::Locked { path, pid }` instead of `Error::Invalid` with a message,
which P01's binding had to recognize by its text. Every variant gets `Error::code()`, a
stable kebab-case name (`cancelled`, `not-found`, `conflict`, `locked`, `guard-missing`,
and so on, with a `Component` error's own code). The server maps codes to HTTP statuses,
the Python binding maps them to its exception classes, and P04 and P05 map them to theirs.

### 4.4 Opening a dataset

`Dataset::open`, `open_with` and `from_store` do what the server's `AppState::dataset_of`
does today. They install the write guard from `validation.json`, load RDFS on read from
`rdfs.json`, open the stored queries (`queries.json`) and the GraphQL configuration
(`graphql.json`), read the reasoning record (`reasoning.json`) and the clone origin
(`origin.json`), and create an empty closure cache and schema cache. A stored query or
GraphQL file that cannot be read leaves that catalog read-only with its error, as the
server does now.

A guard that cannot be installed leaves the dataset refusing writes with
`Error::GuardMissing`, as the store's fail-closed rule requires. When the reason is a
missing feature, such as a SHACL `validation.json` in a build without `shacl`, the message
names the feature.

`Dataset` gains a private `Inner` that holds the store and this state:

```rust
pub struct Dataset { inner: Arc<Inner> }
struct Inner {
    store: Store,
    name: Option<String>,                         // set by a Catalog
    queries: stored::Catalog,
    #[cfg(feature = "graphql")] graphql: sparkles_graphql::Catalog,
    reasoning: RwLock<Option<ReasoningRecord>>,
    #[cfg(feature = "reasoning")] closure: sparkles_reasoner::Cache,
    rdfs: RwLock<Option<Arc<RdfsOnRead>>>,
    guard: RwLock<Option<Guard>>,
    schema_cache: Mutex<Option<SchemaCacheEntry>>,
    origin: Option<Origin>,
}
```

`ReasoningRecord` is today's `ReasoningInfo` from the server's `state.rs`, moved with
`RunInfo`, `RunChanges` and `AutoSetting`, and its serde form is unchanged so existing
`reasoning.json` files and registries still read. `open_with` takes
`impl Into<DatasetOptions>`. `DatasetOptions` holds the `StoreOptions` and the library's
own settings, such as the closure cache's size (`serve --reason-cache-triples`), and
`From<StoreOptions>` keeps every current call compiling.

### 4.5 `Dataset` itself

`Dataset` keeps its methods. It gains these, each moved from a handler or the store:

| Method | Source |
|---|---|
| `remove_prefix(prefix) -> Result<bool>` | `Store::remove_prefix` |
| `replace(target, sources) -> Result<Receipt>` | `Store::replace_as` |
| `clear() -> Result<u64>` | the Graph Store handler's dataset delete |
| `dump_graph(graph, w, format) -> Result<u64>` | the Graph Store handler's graph read |
| `explain(query, &QueryOptions) -> Result<(String, PlanInfo)>` | `sparkles::sparql::explain` |
| `stats(&StatsOptions) -> Result<DatasetStats>` | the counts of `GET /$/stats/{ds}` |
| `clear_cache()` | `Store::result_cache().clear()` |
| `compact_with(&CompactOptions, &Control) -> Result<CompactReport>` | `Store::compact_with` |
| `clone_to(dir, &CloneOptions)`, `clone_to_with(…, &Control)`, `clone_to_memory(…)` | `Store::clone_to` with the server's `origin.json` and the rebased reasoning record |
| `name() -> Option<&str>` | set when a `Catalog` opened the dataset |

`DatasetStats` holds the fields of today's statistics body that describe the data. Those
are the commit, the quads, the base and delta counts, the terms, the generation, the
graphs, the top predicates and classes, the disk bytes, the history summary, the block
and result cache counters, and the reasoning, spatial index and quota summaries. The
server adds Fuseki's request counters and the compaction scheduler's state, which are
its own (§6.5, §6.6).

### 4.6 Snapshots and history

```rust
impl Dataset {
    pub fn snapshots(&self) -> Snapshots;
    pub fn history(&self) -> History;
}

impl Snapshots {
    pub fn list(&self) -> Vec<NamedSnapshot>;
    pub fn get(&self, name: &str) -> Option<NamedSnapshot>;
    pub fn create(&self, name: &str, opts: &SnapshotOptions) -> Result<NamedSnapshot>;
    pub fn delete(&self, name: &str) -> Result<bool>;
}

impl History {
    pub fn status(&self) -> HistoryStatus;
    pub fn commit(&self, reference: &CommitRef) -> Result<Option<CommitDetail>>;
    pub fn commits(&self, range: CommitRange, limit: usize) -> CommitPage;
    pub fn diff(&self, from: &At, to: &At, opts: &DiffOptions) -> Result<Diff>;
    pub fn changes(&self, after: u64, opts: &ChangesOptions) -> Result<ChangePage>;
    pub fn wait_for_commit(&self, after: u64, timeout: Duration) -> Option<u64>;
    pub fn query(&self, q: &HistoryQuery) -> Result<HistoryResult>;
    pub fn prune(&self) -> Result<u64>;
    pub fn tick(&self) -> Result<TickReport>;
}
```

`CommitRef` is a commit number, `head`, or a commit IRI, as the `{reference}` of
`/$/commits/{ds}/{reference}` accepts. `CommitDetail` is a `CommitInfo` with its
annotation. `wait_for_commit` blocks on the store's commit subscription and returns the
new head, or `None` at the timeout. The server's long poll on `/{ds}/changes?wait=` stays
an HTTP feature that awaits the same subscription asynchronously (§6.3). `tick` takes the
named snapshots that the dataset's schedules make due and expires old ones. The server
calls it from its periodic loop, and an embedder calls it when it likes.

### 4.7 Indexes

```rust
impl Dataset { pub fn indexes(&self) -> Indexes; }
impl Indexes {
    pub fn text(&self) -> TextIndex;
    pub fn vector(&self) -> VectorIndexes;
    pub fn geo(&self) -> GeoIndex;
}

impl TextIndex {
    pub fn status(&self) -> Option<TextStatus>;
    pub fn enable(&self, cfg: TextConfig) -> Result<TextStatus>;
    pub fn enable_with(&self, cfg: TextConfig, ctl: &Control) -> Result<TextStatus>;
    pub fn disable(&self) -> Result<()>;
    pub fn rebuild(&self) -> Result<TextStatus>;
    pub fn rebuild_with(&self, ctl: &Control) -> Result<TextStatus>;
    pub fn search(&self, req: &TextSearch) -> Result<TextHits>;
}

impl VectorIndexes {
    pub fn list(&self) -> Vec<VectorIndexStatus>;
    pub fn get(&self, name: &str) -> Option<VectorIndexStatus>;
    pub fn put(&self, name: &str, cfg: VectorIndexConfig) -> Result<bool>;
    pub fn drop(&self, name: &str) -> Result<()>;
    pub fn rebuild(&self, name: &str) -> Result<()>;
    pub fn wait(&self, name: &str) -> Option<VectorIndexStatus>;
    pub fn recall(&self, name: &str, opts: &RecallOptions) -> Result<RecallReport>;
    pub fn reembed(&self, name: &str) -> Result<()>;
    pub fn embedding_status(&self, name: &str) -> Option<EmbeddingStatus>;
    pub fn embed_until_idle(&self, timeout: Duration) -> Result<()>;
}

impl GeoIndex {
    pub fn status(&self) -> Option<GeoStatus>;
    pub fn enable(&self, cfg: GeoConfig) -> Result<GeoStatus>;
    pub fn disable(&self) -> Result<()>;
    pub fn rebuild(&self) -> Result<GeoStatus>;
    pub fn wait(&self) -> Option<GeoStatus>;
    pub fn features(&self, q: &BoxQuery) -> Result<FeatureCollection>;
}
```

`TextIndex::search` takes the query text, the limit and the highlighting options of
`GET /{ds}/text`, runs the `text:query` the handler writes today, and returns the hits
with their scores, literals, graphs, predicates and highlighted fragments. The vector and
spatial index builds run in the store's background threads, as they do now, so their
`put`, `enable` and `rebuild` return once the build starts and `wait` blocks until it
ends.

### 4.8 Backups

```rust
#[cfg(feature = "backup")]
impl Dataset { pub fn backups<'r>(&self, repo: &'r Repository) -> Backups<'r>; }

impl Backups<'_> {
    pub fn list(&self, filter: &ListFilter) -> Result<Vec<BackupSummary>>;
    pub fn get(&self, name: &str) -> Result<Option<Manifest>>;
    pub fn create(&self, req: &BackupRequest) -> Result<BackupSummary>;
    pub fn create_with(&self, req: &BackupRequest, ctl: &Control) -> Result<BackupSummary>;
    pub fn delete(&self, name: &str) -> Result<bool>;
    pub fn verify_with(&self, name: &str, opts: &VerifyOptions, ctl: &Control)
        -> Result<VerifyReport>;
    pub fn restore_to_dir(&self, name: &str, dir: &Path, opts: &RestoreOptions,
        ctl: &Control) -> Result<RestoreReport>;
    pub fn run_policy(&self, policy: &PolicyConfig, ctl: &Control) -> Result<PolicyRun>;
}
```

`Backups` is the one handle that borrows, because the repository is the caller's. Its
`list` fills `ListFilter::dataset_id` with the dataset's id. Its `create` captures the
store with `Store::backup_capture`, adds the dataset's reasoning record as `reasoning.json`
the way the server does, and names the backup from the dataset's name and the time unless
`BackupRequest` names it. `restore_to_dir` writes a restored database into an empty
directory for a program without a catalog, and `Catalog::restore` (§5.3) restores into
the catalog.

`sparkles-backup` is async, and the library is not. The handle's methods block on a
Tokio runtime. They use `BackupRuntime::handle` from the options when given, which is how
the server passes its own runtime from its task threads, and otherwise a two-thread
runtime that the facade starts on first use and keeps for the process. A blocking call
made from inside an async task would panic in Tokio, so the handle detects it and fails
with `Error::Invalid`, naming the `Repository`'s async methods as the alternative.

`sparkles::backup` re-exports `sparkles_backup`. For repository-wide operations that have
no dataset, `sparkles::backup::blocking(&repo)` returns a wrapper with blocking `test`,
`stats`, `list`, `verify`, `gc`, `locks` and `break_lock` on the same runtime. The pure
functions of `sparkles_backup::policy` (`next_runs`, `describe`, `check_policy`) are
reachable as `sparkles::backup::policy`.

### 4.9 Schema, stored queries and GraphQL

```rust
impl Dataset { pub fn schema(&self) -> Schema; }
impl Schema {
    pub fn report(&self, opts: &SchemaOptions) -> Result<Arc<SchemaReport>>;
    pub fn classes(&self, opts: &SchemaOptions, page: &PageRequest)
        -> Result<Page<ClassEntry>>;
    pub fn predicates(&self, opts: &SchemaOptions, page: &PageRequest)
        -> Result<Page<PredicateEntry>>;
    pub fn profiles(&self, opts: &ProfileOptions) -> Result<ClassProfiles>;
    pub fn diff(&self, from: &At, to: &At, opts: &SchemaOptions) -> Result<SchemaDiff>;
    pub fn draft_shapes(&self, opts: &DraftOptions) -> Result<ShapesDraft>;
    pub fn void(&self, opts: &VoidOptions) -> Result<Vec<Triple>>;
    #[cfg(feature = "shacl")]
    pub fn constraints(&self, req: &ConstraintsRequest) -> Result<ConstraintsLayer>;
}
```

`report` keeps the server's caching rules. The last report is kept with the snapshot
identity and a hash of the selection, and a later request at a newer commit updates it
from the changes with `schema::maintain` when the changes are few enough, as
[C02](C02-schema-discovery.md) describes. The cursor format of `classes` and
`predicates` moves with it, so the cursors the server hands out do not change.

```rust
impl Dataset { pub fn queries(&self) -> StoredQueries; }
impl StoredQueries {
    pub fn list(&self) -> Vec<(String, Stored)>;
    pub fn get(&self, name: &str, version: Option<u64>) -> Option<Stored>;
    pub fn versions(&self, name: &str) -> Option<Vec<Version>>;
    pub fn put(&self, name: &str, def: Definition, change: Change) -> Result<Saved>;
    pub fn delete(&self, name: &str, if_version: Option<u64>) -> Result<bool>;
    pub fn run(&self, name: &str, params: &Params, opts: &QueryOptions)
        -> Result<QueryResult>;
}

#[cfg(feature = "graphql")]
impl Dataset { pub fn graphql(&self) -> GraphQl; }
impl GraphQl {
    pub fn config(&self) -> GraphQlConfig;    // get, set, reset (§4.10)
    pub fn versions(&self) -> Vec<Version>;
    pub fn draft(&self, opts: &DraftSchemaOptions) -> Result<String>;
    pub fn execute(&self, req: &Request, opts: &Options) -> Response;
    pub fn sdl(&self) -> Result<String>;
}
```

`StoredQueries::run` binds the parameters with `Query::bind`, checks their types, and runs
the query with the given options, as `/{ds}/queries/{name}` does after it parses the
request.

### 4.10 Settings

```rust
impl Dataset { pub fn settings(&self) -> Settings; }
impl Settings {
    pub fn compaction(&self) -> CompactionSetting;
    pub fn describe(&self) -> DescribeSetting;
    pub fn quota(&self) -> QuotaSetting;
    pub fn retention(&self) -> RetentionSetting;
    pub fn change_log(&self) -> ChangeLogSetting;
}
```

Every setting has the same three verbs, which match `GET`, `PUT` and `DELETE` on its
route. `get` returns the dataset's value. `set` stores a new one in the dataset's file and
applies it at once. `reset` removes the file, so the defaults apply again.

| Setting | `get` returns | `set` takes | File | Defaults from |
|---|---|---|---|---|
| `compaction` | `CompactionSettings`, with `effective(&base)` for the policy in force and `status()` for the measures, the verdict and any blocker | `CompactionSettings` | `compaction.json` | the caller's base policy (`serve --auto-compact-*`) |
| `describe` | `DescribeOptions` | `DescribeOptions` | `describe.json` | the engine's defaults |
| `quota` | `QuotaStatus` | a byte limit, 0 for none | `quota.json` | `StoreOptions::max_disk_bytes` |
| `retention` | `HistorySettings`, which holds the retention window, the snapshot schedules and the catalog horizon | `HistoryUpdate`, whose `None` fields keep their value, as `PUT /$/history/{ds}` does | the history files | `StoreOptions` |
| `change_log` | `ChangeLogSettings` | `ChangeLogSettings` | `changelog.json` | `StoreOptions::change_log` and its sizes |

Prefixes are not a setting of this kind, because they are a map with their own verbs.
They stay on `Dataset` as `prefixes`, `set_prefix` and `remove_prefix`. The GraphQL
configuration (`ds.graphql().config()`), RDFS on read (`ds.reasoning().rdfs()`) and the
write guard (`ds.validation().guard()`) use the same `get`, `set` and `reset` verbs on
their own handles.

### 4.11 Reasoning and validation

```rust
impl Dataset {
    pub fn reasoning(&self) -> Reasoning;
    pub fn validation(&self) -> Validation;
}

impl Reasoning {
    #[cfg(feature = "reasoning")]
    pub fn run(&self, req: &ReasonRequest) -> Result<ReasonOutcome>;
    #[cfg(feature = "reasoning")]
    pub fn run_with(&self, req: &ReasonRequest, ctl: &Control) -> Result<ReasonOutcome>;
    #[cfg(feature = "reasoning")]
    pub fn clear(&self) -> Result<u64>;
    #[cfg(feature = "reasoning")]
    pub fn diagnostics(&self, opts: &DiagnosticsOptions) -> Result<Diagnostics>;
    pub fn status(&self) -> Option<ReasoningStatus>;
    pub fn rdfs(&self) -> RdfsSetting;            // get, set, reset
}

impl Validation {
    pub fn guard(&self) -> GuardSetting;          // get, set, reset
    #[cfg(feature = "shacl")]
    pub fn shacl(&self, shapes: &ShapesSource, opts: &ValidateOptions)
        -> Result<ValidationReport>;
    #[cfg(feature = "shex")]
    pub fn shex(&self, req: &ShexRequest) -> Result<ShexReport>;
}
```

`run` does what `POST /$/reason/{ds}` does in its task today. It reads the inputs that
the request names, chooses an incremental run when the record and the closure cache allow
one, materializes, and writes the record with the commit, the inputs, the watched graphs
and the run's method. `status` returns the record with its freshness against the head,
which `reasoning::freshness` computes in the server today. A dataset's automatic re-run
setting stays in the record, which `status` reports, but the scheduler that acts on it is
the server's (§6.5).

`GuardSetting::get` returns the guard's configuration and status, the counts the server
shows at `GET /$/validation/{ds}`. `set` validates a configuration, writes
`validation.json` and installs the guard, and `reset` removes both. This replaces
`write_validation::install` in the server and `admin::install` in the Python binding,
which become calls to the library.

### 4.12 Free functions

Stateless operations stay free functions, and three move out of the server:

* `sparkles::sparql::parse_query` and `sparkles::sparql::update::parse_update`, as now;
* `sparkles::io::check_data(format, text) -> Vec<DataIssue>`, the parser loop of
  `/$/validate/data`, with line and column of each problem;
* `sparkles::terms::check_iri` and `check_langtag`, from the server's `tools::terms`,
  which `sparkles iri` and `sparkles langtag` also use;
* `sparkles::geo::convert::convert`, as now;
* `sparkles::fmt::format` and `sparkles::fmt::lint`, re-exported from `sparkles-fmt`
  behind the `fmt` feature;
* `sparkles::backup::policy::next_runs`, as now.

## 5. The catalog

### 5.1 Purpose and API

A program that serves several datasets from one directory, such as a desktop tool or a
test harness, needs what the server's `AppState` does with its registry today. `Catalog`
is that logic as a library type, and the server's `AppState` holds a `Catalog` instead of
its own map.

```rust
pub struct Catalog { /* Arc inside; Clone, Send, Sync */ }

impl Catalog {
    pub fn open(dir: impl AsRef<Path>, opts: CatalogOptions) -> Result<Catalog>;
    pub fn memory(opts: CatalogOptions) -> Catalog;
    pub fn inspect(dir: impl AsRef<Path>) -> Result<Vec<DatasetInfo>>;

    pub fn list(&self) -> Vec<DatasetInfo>;
    pub fn info(&self, name: &str) -> Option<DatasetInfo>;
    pub fn get(&self, name: &str) -> Option<Dataset>;
    pub fn create(&self, name: &str, req: &CreateDataset) -> Result<Dataset>;
    pub fn attach(&self, name: &str, source: Attach) -> Result<Dataset>;
    pub fn delete(&self, name: &str) -> Result<bool>;
    pub fn rename(&self, from: &str, to: &str) -> Result<Dataset>;
    pub fn reserve(&self, name: &str, kind: ReservationKind, holder: &str)
        -> Result<Reservation>;
    pub fn clone_dataset(&self, src: &str, dst: &str, req: &CloneRequest, ctl: &Control)
        -> Result<Dataset>;
    pub fn backup_files(&self) -> Result<Vec<BackupFile>>;
    pub fn dir(&self) -> Option<&Path>;

    #[cfg(feature = "backup")]
    pub fn repositories(&self) -> Repositories;
    #[cfg(feature = "backup")]
    pub fn restore(&self, repo: &Repository, backup: &str, req: &RestoreRequest,
        ctl: &Control) -> Result<Dataset>;
    #[cfg(feature = "backup")]
    pub fn run_policy(&self, policy: &PolicyConfig, ctl: &Control) -> Result<PolicyRun>;
    #[cfg(feature = "backup")]
    pub fn apply_retention(&self, policy: &PolicyConfig, dry_run: bool)
        -> Result<RetentionReport>;
}

pub struct DatasetInfo {
    pub name: String,
    pub id: Uuid,
    pub kind: DatasetKind,            // Persistent or Memory
    pub path: Option<PathBuf>,
    pub attached: bool,               // not in the registry (serve --loc, --mem)
    pub reserved_by: Option<String>,  // a clone or restore is creating it
}
```

`Catalog::open` creates `<dir>/databases` when it is missing, takes the catalog lock
(§5.4), removes `.clone-*` directories left by a stopped server, as `AppState::new` does
now, and opens every dataset in `config.json`. `Catalog::memory` has no directory and
holds in-memory and attached datasets only, as `AppState::standalone` does for
`sparkles mcp`. `inspect` reads `config.json` without the lock, for a tool that lists the
datasets of a running server's directory.

`create` takes the kind and the index configurations that `POST /$/datasets` accepts
today, such as `geo`. `attach` registers a directory or a new in-memory store without
writing the registry, for `serve --loc` and `--mem`. `delete` removes the entry from the
registry first and then the directory, and puts the entry back if the registry write
fails, as the server does.

`reserve` holds a name for a dataset that a long task will create. Clones and restores
reserve their target, and a create, rename or second reservation of that name fails with
`Error::Conflict` naming the holder. The server's `reserved` and `restoring` maps become
reservations of kind `Clone` and `Restore`. The server still answers `503` for a dataset
being restored in place, but it asks the catalog which datasets those are.

`clone_dataset` builds a persistent clone in a hidden `databases/.clone-*` directory and
renames it into place, or builds an in-memory clone, then registers it, which is the
server's `clone.rs` with `adopt` and `adopt_memory`. `restore` creates a new dataset from a
backup, or replaces an existing one in place, which closes it, swaps the directories and
reopens it, as the server's `backup/swap.rs` does. It supplies the backup crate's
`id_in_use` check from the catalog's datasets, so `identity: auto` keeps a backup's id only
when no dataset in the catalog has it.

`repositories()` manages `<dir>/backup/repositories.json` with `list`, `get`, `add`,
`update` and `remove`, and opens a `Repository` by name. The server adds the read-only
entries of `--backup-config` with `Repositories::with_fixed(entries)`, and the shared
namespace rule of today's registry stays. `run_policy` makes one backup of each dataset a
policy names and then applies its retention, which is `POST
/$/backup-policies/{policy}/run` without the scheduler. `apply_retention` plans with
`sparkles_backup::policy::retention` and deletes what the plan selects.

### 5.2 Names and ids

The name rule is the server's `valid_name`, moved to `sparkles::catalog::valid_name`. A
name has 1 to 64 ASCII letters, digits, `_`, `-` and `.`, does not start with `.`, and is
neither `ui` nor `$`. The last two exist for the server's URL space, and the catalog keeps
them so that a directory an embedder made can be served. The leading-dot rule keeps the
catalog's own temporary directories out of the namespace.

A dataset's id is the store's UUID from [CI](CI-commit-identity.md), and its name is an
alias. Receipts, commit IRIs and backup manifests carry the id, so they survive a rename.
`DatasetInfo::id` reports it, and `Catalog::get_by_id` finds a dataset by it.

`rename` is new. The HTTP API has no route for it before Phase 3. For a persistent dataset
it requires that no other `Dataset` handle for it is alive, because the store keeps its
path, and it fails with `Error::Conflict` otherwise. It then closes the store, renames
`databases/<from>` to `databases/<to>`, writes the registry and reopens the dataset. An
in-memory dataset only changes its key. A rename on a server also affects grants, which
name datasets, so the HTTP route waits for the decision in open question 3.

### 5.3 Layout

The catalog keeps the server's directory layout, so a server's data directory opens as a
catalog and the other way round:

```
<dir>/catalog.lock                      the catalog lock (new, §5.4)
<dir>/config.json                       the registry: name, type, legacy reasoning
<dir>/databases/<name>/                 one store per persistent dataset
<dir>/databases/.clone-*                clones being built, removed at open
<dir>/backups/                          N-Quads backups (Fuseki's backups-list)
<dir>/backup/repositories.json          backup repositories (feature backup)
<dir>/backup/policies.json              backup policies, read by the server only
```

`config.json` is written as today, to a temporary file that is synced and renamed over
the old one, followed by a sync of the directory. An in-process mutex orders the writes,
as the server's `manage` mutex does.

### 5.4 Locking between processes

Each store already holds an exclusive OS lock on `<store>/sparkles.lock` while it is open,
with the process id written into the file. A second process that opens the same store
fails with a message that names the holder's process id and suggests HTTP. Nothing locks
the data directory itself today. A second server or CLI that loaded `config.json` would
fail one store at a time, and two processes could each rewrite the registry.

The catalog adds `<dir>/catalog.lock`, taken with `File::try_lock`, the same exclusive
lock that the store uses, with the process id inside. The rules are these.

* **One process manages a directory.** `Catalog::open` takes the catalog lock before it
  opens any store, and holds it for the catalog's lifetime. A second `Catalog::open` of
  the same directory, from another process or the same one, fails with `Error::Locked {
  path, pid }`. The message says that the directory is in use by that process and that
  the program can stop it or talk to it over HTTP.
* **The store locks still apply.** The catalog lock adds to them and does not replace
  them. A server holds the catalog lock and every store lock of its datasets.
* **A CLI next to a running server.** Commands that open one database with `--loc`, such
  as `sparkles quota --loc` or `sparkles queries put`, fail on the store's lock as they do
  today, and their `--server` form goes over HTTP. Catalog commands (Phase 3) fail on the
  catalog lock and offer `--server` in the same way. `Catalog::inspect` reads the
  registry without a lock, which is safe because the registry is replaced atomically.
* **A CLI while the server is stopped.** A command may open one store under the
  directory without the catalog, which takes that store's lock only. If the server starts
  meanwhile, it fails to open that dataset with the store's message, as it does today.
* **Local file systems only.** `flock` and `LockFileEx` are reliable on local file
  systems. On network file systems their behaviour varies, and the documentation says
  that a data directory belongs on a local disk, as it already says for stores.

## 6. Server-only by design

Each server-only operation has one of the reasons below. The parity test (§7) requires
each server-only entry to name one of them.

### 6.1 Identities and credentials

Logins, sessions, API tokens, OIDC flows, device logins, CLI authorization and `whoami`
exist because an HTTP server meets callers it does not know. An embedding program is its
own caller. It already decides which code may call which method, and it has no
credentials to check. The engine's enforcement of what a caller may see stays in the
library. Graph views ([C12](C12-graph-access-control.md)) and triple protections
([C12b](C12b-triple-access-control.md)) apply through the `QueryOptions` and
`WriteOptions` fields the server passes, and an embedder may pass the same fields. Only the
step from a credential to those fields is the server's.

### 6.2 Rate and concurrency limits

Rate limits share the server among clients that compete for it. An embedder controls its
own concurrency, and the library's budgets ([C01](C01-observability-and-budgets.md)),
such as timeouts, memory, rows and result size, are already options of each call.

### 6.3 HTTP features

Long polls, server-sent and streamed responses, content negotiation, compression,
conditional requests, `Retry-After` and CORS are properties of HTTP. The library offers
what they are built on. That is `History::wait_for_commit` for the change feed's long
poll, `Precondition` for `If-Match`, and streaming iterators for large results. Taking a
dataset offline with `setDatasetState` makes its HTTP services answer `503` and is not
persisted, so it is routing state of the server and has no meaning in a library.

### 6.4 The task queue

The queue assigns task ids, keeps the slots of `--max-tasks` and the per-kind limits,
lists running and finished tasks, keeps the last 200 finished ones, and turns `DELETE
/$/tasks/{id}` into a cancellation. An embedder runs a long call on a thread of its own
choosing. The library offers `Control`, which is what the queue gives each task (§4.2),
so the work is the same and only its scheduling differs.

### 6.5 Schedulers

Backup policies, automatic re-materialization of stale inferences, automatic compaction
and the periodic history tick run on the server's clock. Their registries
(`policies.json`), their run history and their timing are scheduler state. The library
offers each step a scheduler takes. `Catalog::run_policy` and `apply_retention` run a
policy once, `sparkles::backup::policy::next_runs` previews its schedule,
`Reasoning::run` re-materializes, `CompactionSetting::status` gives the verdict that
starts an automatic compaction, and `History::tick` takes the snapshots that are due. An
embedder that wants a schedule calls these from its own timer.

### 6.6 Process and HTTP state

Ping, readiness, server information, Prometheus metrics, Fuseki's request counters and
the OpenAPI description describe the server process and its HTTP traffic. The data behind
the dataset parts of these answers comes from library calls such as `Dataset::stats`,
`Catalog::list` and the index statuses.

### 6.7 The MCP transport

`/$/mcp` carries the Model Context Protocol over HTTP. Its tools call the same library
functions as the HTTP handlers, so the protocol needs no library call of its own. Open
question 7 asks whether the parity test should also cover the MCP tools.

## 7. Parity tests

### 7.1 Operations to library calls

The facade gains an integration test, `crates/sparkles/tests/parity.rs`, compiled with
`--features full`. It holds one entry per OpenAPI operation:

```rust
op!("listSnapshots", "snapshots.list", |ds, _cat| { ds.snapshots().list(); });
op!("getSchemaDiff", "schema.diff",
    |ds, _cat| { ds.schema().diff(&At::Head, &At::Head, &Default::default()); });
op!("listDatasets", "catalog.list", |_ds, cat| { cat.list(); });
op!("setHistory", "settings.retention.set",
    |ds, _cat| { ds.settings().retention().set(Default::default());
                 ds.settings().change_log().set(Default::default()); });
server_only!("createToken", Reason::Credentials);
```

The first argument is the `operationId`, and the second is the surface key that §7.2
uses. The closure is type-checked and never called, so an entry compiles only when the
library call exists with arguments of that shape. `Reason` is a closed enum whose
variants are the subsections of §6.

The test reads `docs/openapi.json`, as the client's test does, and checks four things.

1. Every operation has exactly one entry. A missing one fails with a message that names
   it and says what to add, for example *operation `getSchemaDiff` (`GET
   /$/schema/{ds}/diff`) has no entry in `crates/sparkles/tests/parity.rs`. Map it with
   `op!` to the library call that performs it, or mark it `server_only!` with a reason
   from §6 of spec P06.*
2. Every entry names an operation that the description has. A stale one fails with *`parity.rs`
   maps `fooBar`, which `docs/openapi.json` does not describe. Remove the entry or rename
   it.*
3. No `operationId` appears twice.
4. During Phase 1 an entry may be `pending!("operationId", "phase 1 step 4")`. The test
   prints the pending entries, and the last commit of Phase 1 removes the macro, so from
   then on every operation must map.

X03's route test keeps the description equal to the route table, so a new route adds an
operation, and this test then requires its library call or its reason. The chain runs
from the router through the description to the library.

### 7.2 Library calls to bindings

The surface keys of the `op!` entries, together with a short list `EXTRA_KEYS` in
`parity.rs` for library calls that no HTTP operation reaches (such as `catalog.rename`,
`history.wait_for_commit` and `indexes.vector.embed_until_idle`), make up the binding
surface. `crates/sparkles/bindings.toml` gives each key its name in each binding:

```toml
["snapshots.create"]
python = "Dataset.snapshots.create"
jvm = "planned: P04 phase 3"
node = "planned: P05"

["fmt.format"]
python = "skip: the formatter is an editor tool, and Python programs run sparkles fmt"
jvm = "skip: as for Python"
node = "planned: P05"
```

A value is a dotted name in the binding, `planned: <spec and phase>`, or `skip: <reason>`.

The Rust test checks that the keys of `bindings.toml` equal the keys of the table and
`EXTRA_KEYS`, and that each key has a `python`, `jvm` and `node` value. A missing key fails
with *surface key `schema.diff` has no entry in `crates/sparkles/bindings.toml`. Add its
python, jvm and node names, or planned or skip with a reason.* A stale key fails with the
opposite message.

The Python test, `crates/sparkles-py/tests/test_parity.py`, reads the file with `tomllib`
and parses `python/sparkles/_sparkles.pyi` with `ast`. It resolves each dotted name
through classes, properties and their return annotations. For `Dataset.snapshots.create`
it finds the class `Dataset`, the property `snapshots`, the class that the property
returns, and the method `create` on that class. A name that does not resolve fails with
*`bindings.toml` gives `snapshots.create` the Python name `Dataset.snapshots.create`, and
`_sparkles.pyi` has no method `create` on `Snapshots`. Add it to `crates/sparkles-py` and
the stub, or mark it planned or skip.* P01's `stubtest` already checks the stub against
the compiled module, so checking the stub is enough. After Phase 2 the test refuses
`planned` in the `python` column.

P04 and P05 each add the same check for their column when their bindings land. Until
then the Rust test only requires that the `jvm` and `node` columns have a value, so a new
library call forces a decision for every binding.

## 8. Refactoring the server

### 8.1 Steps

The steps keep the HTTP API, the checked-in OpenAPI description, the UI and every
existing test unchanged. Each step passes `cargo test --workspace`, `mise run py:test`
and the UI's end-to-end smoke tests before the next one starts. The server's handler
tests (`router_tests`, the `*_tests.rs` modules beside the handlers, and the backup,
compaction and schema tests) are the safety net, and none of them is edited to make a
step pass.

1. **Split the crates (§3.1).** Move the engine to `crates/sparkles-core`, create the
   facade with the re-exports, move `dataset.rs` and `querybuilder`, and point the
   satellites at `sparkles-core`. This is a move without behaviour changes.
2. **Add `Control` (§4.2).** Merge the progress types, add `TaskHandle::control`, and
   pass the `Control` into compaction, clone, reasoning and backup tasks through the
   existing option fields.
3. **Move per-dataset state into `Dataset` (§4.4).** Move the reasoning record, the
   closure cache, RDFS on read, the guard install, the stored query and GraphQL
   catalogs, the schema cache and the origin. The server's `state::Dataset` becomes a
   `sparkles::Dataset` with the server's own fields, which are the name, the offline
   flag and the validation metrics observer. The Python binding drops its copy of the
   guard install.
4. **Add the handles and free functions (§4.5 to §4.12).** Land the parity test with
   `pending!` entries for the operations not yet mapped.
5. **Move the handlers onto the handles, one area per commit.** The areas are history
   and snapshots, indexes, settings, schema, stored queries, reasoning, validation,
   GraphQL, statistics and the stateless utilities. Each handler keeps its parsing, its
   permission check and its rendering, and calls the handle for the rest. Logic that the
   step moves leaves the server in the same commit, so no behaviour is kept twice.
6. **Add the catalog (§5) and move `AppState` onto it.** The registry, `create`,
   `attach`, `delete`, `reserve`, `adopt`, `detach_for_swap` and `reattach` become
   catalog calls, then the repository registry, then restores, policy runs and retention.
7. **Move the local CLI commands onto the handles.** `sparkles quota`, `queries`,
   `compaction`, `describe`, `validation`, `geo-index`, `vector` and `clone` open a
   `Dataset` with `--loc` and call its handles, where they call the store or server
   modules today.
8. **Make the parity test strict.** Remove `pending!`, fill the `python` column of
   `bindings.toml` with today's names or `planned: P06 phase 2`, and rewrite the
   "Embedding the library" section of `docs/USAGE.md` around `Dataset`, the handles and
   `Catalog`.

### 8.2 Order relative to P04 and P05

P06 Phase 1 goes first. P04's `sparkles-ffi` and P05's Node crate then bind `Dataset` and
its handles as objects, and their administration phases bind the handles instead of the
JSON-string methods that P04 §4.5 lists for Phase 3. P04 Phase 1, which binds queries,
transactions and loads, can start once step 1 has landed, because those parts of
`Dataset` do not change. P06 Phase 2 can run beside P04 Phase 1, since it touches only
`crates/sparkles-py`. When P04 and P05 update their own specs, their administration
sections should point at this surface and at their columns of `bindings.toml`.

## 9. Phase 2: closing the Python gaps

### 9.1 Shape

The Python API keeps P01's conventions. Names are snake_case, options are keyword
arguments, configurations and reports that the HTTP API returns as JSON come back as
dicts of the same camelCase shape, and records that programs iterate (commits,
snapshots, backups and diff rows) are frozen classes. Handles are read-only properties of
`Dataset`, each a small class over the Rust handle.

```python
import sparkles

cat = sparkles.Catalog("/srv/sparkles")        # or sparkles.Catalog.memory()
ds = cat.create("wiki", kind="persistent", geo={"wktLiterals": True})
ds.load(path="wiki.ttl")

ds.snapshots.create("before-cleanup", note="weekly")
ds.history.diff(10, "head")                    # Diff: iterable of ("+" | "-", Quad)
ds.settings.quota.set(max_bytes=2 << 30)
ds.indexes.geo.enable({"wktLiterals": True})
ds.schema.profiles(classes=["http://schema.org/Person"])
```

Two names already exist as methods. `Dataset.snapshots()` returns a list and
`Dataset.history()` returns a dict. They become properties that return handles. The flat
methods (`create_snapshot`, `delete_snapshot`, `set_retention`, `enable_text`,
`rebuild_text`, `disable_text`, `text_status`, the vector index methods,
`set_write_validation`, `write_validation`, `reason`, `clear_inferences`,
`validate_shacl` and `validate_shex`) are removed in favour of the handle methods,
because nothing has been published to PyPI (§13, question 5). The tests, the stub and the
Python documentation change with them.

### 9.2 Additions

| Area | Python API |
|---|---|
| Catalog | `Catalog(path)`, `Catalog.memory()`, `Catalog.inspect(path)`, `list()`, `info(name)`, `get(name)` and `cat[name]`, `create`, `attach`, `delete`, `rename`, `clone_dataset`, `backup_files()`, and use as a context manager that closes every dataset |
| Snapshots and history | `ds.snapshots.list/get/create/delete`, `ds.history.status/commit/commits/diff/changes/wait_for_commit/query/prune/tick` |
| Settings | `ds.settings.compaction`, `describe`, `quota`, `retention` and `change_log`, each with `get()`, `set(…)` and `reset()`, and `compaction.status()` |
| Prefixes and maintenance | `ds.remove_prefix`, `ds.replace`, `ds.stats(at=None)`, `ds.explain(query)`, `ds.clear_cache()`, `ds.compact(cancel=…, progress=…)` |
| Indexes | `ds.indexes.text` with `search`, `ds.indexes.vector` with `recall`, `wait` and `embedding_status`, and `ds.indexes.geo` with `status/enable/disable/rebuild/wait/features(bbox=(w, s, e, n), limit=…)` |
| Schema | `ds.schema.report`, `classes`, `predicates`, `profiles`, `diff`, `draft_shapes` (Turtle, or a dict with `format="json"`), `void` and `constraints` |
| Stored queries | `ds.queries.list/get/versions/put/delete`, and `run(name, params, **query_kwargs)`, which returns what `query` returns |
| Reasoning | `ds.reasoning.run(profile="rdfs", rules=None, inputs=None, cancel=None, progress=None)`, `status()`, `clear()`, `diagnostics()`, and `ds.reasoning.rdfs` with `get/set/reset` |
| Validation | `ds.validation.guard` with `get/set/reset`, `ds.validation.shacl(…)` and `ds.validation.shex(…)` |
| Backups | `BackupRepository.open(url_or_config, cache_dir=None)`, `ds.backups(repo).create/list/get/delete/verify/restore_to_dir/run_policy`, `cat.restore(repo, backup, name=…, identity="auto", in_place=False)`, `cat.repositories`, `cat.run_policy`, `cat.apply_retention`, and on the repository `test`, `stats`, `backups`, `verify`, `gc`, `locks` and `break_lock` |
| GraphQL | `ds.graphql.config` with `get/set/reset`, `versions()`, `draft()`, `execute(query, variables=None, operation_name=None)`, which returns the response dict, and `sdl()` |
| Utilities | `sparkles.check_iri`, `sparkles.check_langtag`, `sparkles.check_data(text, format)` and `sparkles.preview_schedule(schedule, timezone, n=5)` |

The formatter and linter are marked `skip` for Python in `bindings.toml`, because Python
programs that want them run `sparkles fmt` and `sparkles lint`.

### 9.3 Features, wheels and errors

The crate gains the features `backup` and `graphql`, on by default with the existing
ones. The wheel's `backup` turns on the `fs` and `s3` backends of `sparkles-backup`. The
wheel then also contains `object_store` with its AWS client, which the server already
links, so the dependency is new to the wheel only. Phase 2 measures the wheel's size
before and after and records it in the Outcome. Open question 6 asks about the other
cloud backends.

`Error::code()` drives the exception mapping. `Error::Locked` keeps raising
`DatasetLockedError`, and a catalog lock raises its new subclass `CatalogLockedError`.
`Error::Component` with the component `backup` raises a new `BackupError` with the code
as `.code`. The other new codes map to the existing classes, so `not-found` raises
`NotFoundError` and `conflict` raises `ConflictError`.

### 9.4 Cancellation and progress from Python

Every long call takes `cancel: CancelToken | None` and `progress: Callable[[float, str],
None] | None`. The binding builds a `Control` from them. The work runs with the GIL
released, as P01's calls do, and the binding's progress function takes the GIL for each
report and drops reports that come less than 100 ms after the previous one, except the
last. An exception raised by the callback cancels the call and is raised again when the
call returns. Ctrl-C cancels as it does for queries.

## 10. Phasing

**Phase 1** is the Rust surface and the server's move onto it. It is about seven weeks of
work for one developer who knows the engine (size L).

* the crate split and the facade, about three days;
* `Control`, the merged progress types and `TaskHandle::control`, about three days;
* the per-dataset state in `Dataset` and the guard install at open, about a week and a
  half;
* the handles, the settings and the free functions, about a week and a half;
* the catalog, its lock and the move of `AppState`, about a week;
* the handlers and the local CLI commands on the handles, about a week and a half;
* the parity test, `bindings.toml` and the documentation, about four days.

**Phase 2** closes the Python gaps of §9. It is about three weeks (size M). Two thirds is
the bindings and stubs, and the rest is tests, the parity test's strict mode for Python,
and the wheel measurements.

**Phase 3** is about a week and a half (size S to M), and its items are independent.

* `POST /$/datasets/{ds}/rename`, once open question 3 is decided, with the grants
  updated or the rename refused;
* `sparkles dataset list|create|delete|rename|clone --data-dir`, which manage a stopped
  server's data directory through `Catalog` and take `--server` for a running one;
* lazy opening of datasets in `Catalog`, if open question 2 is decided for it.

## 11. Acceptance examples

* **A1.** In Rust, a program opens a catalog, creates a dataset, loads data and creates
  a snapshot, which it can then read back:

  ```rust
  let cat = Catalog::open(dir, CatalogOptions::default())?;
  let ds = cat.create("wiki", &CreateDataset::persistent())?;
  ds.load_str("<a:s> <a:p> 1 .", RdfFormat::NTriples)?;
  let s = ds.snapshots().create("before", &SnapshotOptions::default())?;
  assert_eq!(ds.snapshots().get("before").unwrap().seq, s.seq);
  assert_eq!(cat.list()[0].id, ds.dataset_id());
  ```

* **A2.** A server started on that directory lists `wiki`, and the body of `GET
  /$/snapshots/wiki` holds the serde form of `ds.snapshots().list()` from A1 field for
  field.
* **A3.** `ds.compact_with(&opts, &ctl)` on a dataset of 10⁶ quads reports progress
  that never decreases and ends at 1.0. With `ctl.cancel.cancel()` called from the
  progress callback at 0.5, the call fails with `Error::Cancelled`, the generation
  name is unchanged, and the next query sees every quad.
* **A4.** On a server, `DELETE /$/tasks/{id}` on a running `compact` task ends it as
  `cancelled`, through the `Control` that `TaskHandle::control` built. The existing task
  tests pass unchanged.
* **A5.** A dataset whose `validation.json` holds SHACL shapes requiring `ex:name` on
  every `ex:Person` is opened with `Dataset::open` in a build with `shacl`. An insert of a
  person without a name fails with `Error::Rejected` and commits nothing. Before P06 the
  same insert failed with `Error::GuardMissing`. In a build without `shacl`, it fails
  with `GuardMissing` whose message names the feature.
* **A6.** While one process holds `Catalog::open(dir)`, a second process's
  `Catalog::open(dir)` fails with `Error::Locked` whose `pid` is the first process's id,
  and `Catalog::inspect(dir)` in the second process lists the datasets.
* **A7.** `cat.rename("wiki", "wiki2")` fails with `Error::Conflict` while a `Dataset`
  handle for `wiki` is alive. After the handle is dropped it succeeds, and
  `cat.get("wiki2").unwrap().dataset_id()` equals the old id.
* **A8.** With the `backup` feature, `ds.backups(&repo).create(&BackupRequest::default())`
  into a file-system repository, then `cat.restore(&repo, &name, &RestoreRequest::new("copy"),
  &Control::none())`, gives a dataset `copy` with the same quads as `wiki`, a new id
  because `wiki` still exists, and the backup's commit in its `restored_from`.
* **A9.** Adding a route to the server and to the OpenAPI description without a
  `parity.rs` entry fails the parity test with the message of §7.1 that names the
  operation.
* **A10.** `ds.settings().quota().set(1 << 20)` makes a load that would exceed 1 MiB fail
  with `Error::BudgetExceeded`, and after `reset()` the quota is
  `StoreOptions::max_disk_bytes` again.
* **A11.** In Python, `ds.snapshots.list()` returns the dataset's snapshots,
  `ds.snapshots.get("missing")` returns `None`, and `ds.create_snapshot` no longer exists.
* **A12.** In Python, `ds.schema.diff(c1, c2)` between a commit before and a commit after
  adding `ex:Book` instances reports `ex:Book` as an added class, as `GET
  /$/schema/{ds}/diff?from=c1&to=c2` does.
* **A13.** In Python, after `ds.queries.put("by-author", "SELECT ?b WHERE { ?b
  ex:author ?a }", parameters={"a": {"type": "iri", "required": True}})`, the call
  `ds.queries.run("by-author", {"a": "http://ex.org/ann"})` returns Ann's books, and a
  missing `a` raises `InvalidInputError`.
* **A14.** In Python, `ds.backups(repo).create(progress=lambda f, m: seen.append(f))`
  fills `seen` with fractions that never decrease, and `cat.restore(repo, name, name="copy")` gives a
  dataset whose `len` equals the source's.
* **A15.** In Python, with a GraphQL configuration set from `ds.graphql.draft()`,
  `ds.graphql.execute("{ allPerson { nodes { name } } }")` returns the same `data` as `POST
  /{ds}/graphql` with that query.
* **A16.** Changing the `python` value of `schema.diff` in `bindings.toml` to
  `Dataset.schema.difference` fails `test_parity.py` with the message of §7.2.

## 12. Rejected alternatives

* **Generating the bindings from the OpenAPI description.** The description is about
  HTTP. Its shapes carry cursors, headers, long polls, status codes and multipart
  bodies, and its operations are named for routes. Generated code would be a remote
  client, which P02 already provides for Rust, and it would lose what embedding gives,
  such as transactions, iterators, terms as objects and no network hop. The description
  is still the right list of what the server can do, which is why the parity test reads
  it.
* **Keeping `Store` as the API.** `Store` works on ids, write transactions and
  snapshots, and it is shaped by the engine's needs. It cannot reach backups, reasoning,
  SHACL, ShEx or GraphQL without a dependency cycle, and the state that only the server
  keeps (the reasoning record, the guard install, the stored query catalog) is not on it.
  Binding it would tie every binding to engine internals.
* **A separate administration crate.** A `sparkles-admin` crate would give embedders two
  entry points, with queries on `Dataset` and administration somewhere else. Each program
  would wire the two together, and the per-dataset state would belong to neither. The
  facade gives the documented crate name the whole surface.
* **Extension traits in each satellite crate.** `use sparkles_backup::DatasetExt` would
  add `ds.backups()` without a crate split. The methods would appear only after five
  imports, the documentation would be scattered over five crates, and each binding would
  have to know which trait carries which method.
* **Registering the satellites in the core through trait objects.** The core already does
  this for write guards. For backups, schema constraints and GraphQL, the core would have
  to name the satellites' result types or reduce them to JSON, which gives up the typed
  results that the Rust API and the bindings want.
* **JSON in and JSON out.** P01 passes configurations as dicts in the HTTP API's shape,
  and P04 §4.5 planned JSON strings. That is convenient at a binding's edge and is kept
  there. In Rust it would lose the types, and every binding would parse the same JSON.
* **An async library API.** The engine's work is CPU-bound and blocking, and the server
  already calls it from blocking threads. Only the backup crate is async inside, and its
  handle hides that (§4.8).

## 13. Open questions

1. **Default features.** Decided on 2026-10-03: keep the facade's default empty, as the library's is
   today, and offer `full`. The alternative turns on `text`, `reasoning`, `shacl` and
   `shex` by default, as P01 does for Python.
2. **Lazy opening.** A catalog with many large datasets could open each on first use.
   The server opens all of them at start so that a broken one fails the start. Default:
   open all in Phase 1, and decide in Phase 3 with a measurement of open times.
3. **Renames and grants.** Grants in the auth configuration name datasets. A rename over
   HTTP could rewrite the grants, refuse while grants name the dataset, or keep the old
   name as an alias. Decided on 2026-10-03: refuse while a grant names the dataset, and say which grant.
4. **Read-only access to a running server's datasets.** `sparkles check --data-dir` and
   similar tools could read stores that the server holds if stores could be opened read
   only. Default: no, and the tools use `--server`.
5. **Python's flat names.** Decided on 2026-10-03: nothing has been published to PyPI,
   so Phase 2 renames them to the handle methods outright, with no deprecated aliases,
   and updates the tests and documentation in the same change (§9.1).
6. **Cloud backends in the wheel.** Decided on 2026-10-03: `fs` and `s3`. GCS and Azure add their
   clients to every wheel for few users, and a source build can turn them on.
7. **MCP tools in the parity test.** The MCP tools are a third surface over the library.
   Default: add a third table, tool name to surface key, when the tools next change.
8. **Error codes shared with HTTP.** The server's JSON errors carry their own codes.
   Default: make the server's codes the `Error::code()` values where they describe the
   same error, and document both lists in `docs/API.md`.
9. **The backup runtime.** Default: the facade's own two-thread runtime, started on
   first use. A program that already runs Tokio may prefer to pass its runtime's handle,
   which `BackupRuntime` allows.

## 14. Sources

* Sparkles code: `crates/sparkles/src/dataset.rs`, `store.rs` and the `store/` modules
  (`backup`, `changelog`, `changes`, `clone`, `compaction`, `describe`, `diff`, `embed`,
  `geo`, `history_query`, `quota`, `schedule`, `vector`), `history.rs`, `schema.rs` and
  `schema/`, `stored.rs`, `guard.rs` and `guard/config.rs`, `error.rs`, `lib.rs`, the
  query builder and `Cargo.toml`; `crates/sparkles-backup/src/lib.rs`, `repo.rs`,
  `policy.rs` and `types.rs`; the public items of `sparkles-reasoner`, `sparkles-shacl`,
  `sparkles-shex` and `sparkles-graphql`; in `crates/sparkles-server`, `state.rs`,
  `http.rs`, the modules in `http/` (schema, queries, history, changes, diff, describe,
  budgets, validation, the Fuseki handlers and validators), `backup/`, `reasoning.rs`,
  `rdfs.rs`, `write_validation.rs`, `clone.rs`, `geo.rs`, `vector.rs`, `compaction.rs`,
  `quota_cmd.rs`, `queries_cmd.rs` and `openapi/tests.rs`;
  `crates/sparkles-client/src/routes.rs`; and in `crates/sparkles-py`, `src/admin.rs`,
  `src/validate.rs`, `python/sparkles/_sparkles.pyi` and `_errors.py`.
* `docs/openapi.json` at commit `3006e6f8`, and the UI's `ui/src/lib/api.ts`,
  `auth-api.ts` and `backups.ts`.
* The specs P01, P02, P04, X03, CI, C01, C02, C10, C12, C12b, C13, C16, F05 and F06.
* The Rust API Guidelines, for naming, common traits, and the guidance on
  `#[non_exhaustive]` and builders.
* The Cargo Book, on features, optional dependencies, dependency renaming and the rules
  against dependency cycles.
* The Rust standard library's documentation of `File::try_lock`, and the Linux man page
  of `flock(2)` for its behaviour across open file descriptions and on network file
  systems.
* The Tokio documentation of `Runtime`, `Handle::block_on` and the panic when blocking
  inside a runtime.
* The PyO3 user guide, on properties, class methods and releasing the GIL, PEP 702
  (`warnings.deprecated`), and the Python documentation of `ast` and `tomllib`.

## Outcome

Phases 1, 2 and 3 are implemented. The notes below record the implementation
decisions and measurements. The catalog, Python handles, dataset CLI and HTTP rename
are available. Catalog opening remains eager after the startup measurements below.

### Implementation notes

* **`Ctl` stays a struct.** The tests of `sparkles-backup` build `Ctl` from its fields,
  so it is not a re-export of `Control`. It converts from a `Control` with `Ctl::from`
  and back with `Ctl::control`, which is how the server's backup tasks pass their
  task's `Control`.
* **The builder's callback is not merged.** `sparkles::builder` reported messages
  without a fraction, because a build does not know how far it is. Its callback type is
  now `builder::MessageFn`, and `Dataset::compact_with` turns its messages into reports
  of the `Control`'s progress. The store, the reasoner and the backup crate share
  `task::ProgressFn`.
* **The dataset's state is reachable for the server.** `Dataset` holds an `Arc` of a
  `DatasetState`, which `Dataset::state` returns and which is hidden from the
  documentation. The server's `state::Dataset` holds a `sparkles::Dataset` with its own
  name, kind, offline flag and validation metrics, and it dereferences to the library's
  state. That keeps `ds.store`, `ds.reasoning`, `ds.queries`, `ds.validation` and the
  other fields that the server's handlers and tests use. The guard's field keeps the
  server's name, `validation`.
* **`DatasetOptions` has two more fields.** Besides the store's options and the closure
  cache's size, it carries the name a server or catalog gives the dataset and the
  origin of an in-memory clone, which has no `origin.json`. The origin stays JSON in
  the form of `origin.json`.
* **A missing guard says why.** The store gained `set_guard_missing_reason`, so a write
  refused for want of a guard fails with `GuardMissing` whose message names the missing
  feature or the configuration error. `Dataset::guard_error` returns the same reason,
  and the Python binding warns with it as before.
* **Pending entries wait for the step that moves their logic.** Where the work of an
  operation still lives in a handler, step 4 did not copy it into a handle, because
  step 5 moves each handler's logic into its handle in one commit. The parity test marks
  these operations `pending!`: the dataset statistics, text search, the schema report,
  its classes, predicates, diff and constraints, the GraphQL configuration's `put`,
  draft, execution and SDL, the reasoning run, status and diagnostics, backup creation,
  and the stateless validators of data, IRIs and language tags. The catalog's
  operations, restores, the repository registry, policy runs and retention wait for
  step 6. `Dataset::clone_to`, `Dataset::stats`, `Schema::report`, `Schema::void`,
  `TextIndex::search`, `Reasoning::run` and `status`, `Backups::create`,
  `restore_to_dir` and `run_policy`, and the free functions `check_data`, `check_iri`
  and `check_langtag` come with those steps.
* **`Snapshots::create` takes the commit.** Its signature is `create(name, at, opts)`,
  as `POST /$/snapshots/{ds}` takes `at`, and it returns the snapshot with whether it
  was created, which the server needs for its status code.
* **Text index calls have no `_with` forms.** The store's text index builds take no
  cancel flag, so `TextIndex` has `enable` and `rebuild` only.
* **Backups use the library's runtime.** `sparkles::backup::open` and `blocking` give
  blocking forms of the repository's calls, and `Backups` has `list`, `get`, `delete`
  and `verify_with`. They run on a two-thread runtime that the library starts on first
  use. There is no option to pass another runtime's handle. The workspace's
  `sparkles-backup` dependency has its default features, so `backup` includes S3.
* **`Error::Locked` is not added yet.** `Error::code()` and `Error::Component` exist.
  The directory lock's error changes with the catalog lock in step 6.
* **RDFS on read was left to the caller in step 4.** `Dataset::query` read only the
  DESCRIBE setting, so a program had to pass `ds.reasoning().rdfs().get()` itself. Step
  5 replaced this with the dataset's query defaults, which the notes on step 5 describe.
* **The binding table of §7.2 comes with step 8.** `crates/sparkles/bindings.toml` and
  its checks land when the parity test becomes strict.
* **Performance.** The query and update paths take no new lock. A server handler that
  reads `ds.store` follows one more pointer, from the server's dataset to the library's
  state. The store reads its new guard reason only when it refuses a write.

The notes below record step 7, which moved the local commands onto the handles.

* **What each command calls.** `sparkles quota` calls `ds.settings().quota()`,
  `describe-settings` calls `ds.settings().describe()`, and `compaction` calls
  `ds.settings().compaction()` with its new `status`. `queries put`, `delete` and `run`
  call `ds.queries()`. `validation` calls `ds.validation().guard()` for `--status`,
  `--off` and setting a configuration. `geo-index` calls `ds.indexes().geo()`, and
  `vector` calls `ds.indexes().vector()`. Each opens the database with
  `Dataset::open_with` and the command's store options. Their flags, output and exit
  codes are unchanged, and their `--server` forms still call the HTTP API.
* **`clone` waits for the catalog.** `sparkles clone` depends on the catalog of step 6,
  so it moves after that step.
* **The handles grew where a command needed more.** `DescribeSetting::status` returns a
  `DescribeStatus`, the body of `GET /$/describe/{ds}`, which the server's
  describe handler serializes since step 5. `CompactionSetting::effective` applies a
  base policy, and `CompactionSetting::status` returns a `CompactionStatus` with the
  policy in force, the dataset's own settings, the measures in seconds, the state and
  the trigger. Since step 5 the server's compaction status takes those values from it
  and adds its scheduler's fields. `StoredQueries::run_version` runs an older version, and
  `StoredQueries::error` says why `queries.json` could not be read.
  `VectorIndexes::wait_all` waits for every index's build, and
  `VectorIndexes::set_embedding_environment` lets the embedding workers reach their
  providers for `sparkles vector embed` and `reembed`.
* **The command code left the server's modules.** `sparkles geo-index` moved from
  `geo.rs` to `geo_index_cmd.rs`, and `sparkles vector` moved from `vector.rs` to
  `vector_cmd.rs` with `parse_secrets`, which `serve --embedding-secret` also uses.
  The handler code of both modules did not change.
* **Stored query runs follow the dataset's settings.** `sparkles queries run` used to
  run a stored query with the engine's defaults. It now passes the dataset's RDFS on
  read and DESCRIBE setting, as `/{ds}/queries/{name}` does, so a stored DESCRIBE query
  follows `describe.json` and a query over a class matches its subclasses when
  `rdfs.json` is set. `crates/sparkles-server/tests/cli_queries.rs` tests both. Since
  step 5 the run takes the dataset's query defaults, so the materialized inferences are
  part of the default graph too, and the command no longer passes the settings itself.
* **Reading stored queries takes no lock.** `queries list`, `get` and `versions` read
  `queries.json` with `sparkles::stored::Catalog` instead of opening a `Dataset`.
  `Dataset::open` takes the directory's exclusive lock, and these commands have always
  worked beside a server that has the database open. A `queries.json` that cannot be
  read is still an error for every subcommand, and the writing ones check
  `StoredQueries::error` after they open the dataset.
* **Opening a dataset installs its write guard.** None of these commands commits data,
  so the guard decides no write. `validation` still opens with `unvalidated_writes`.
  `validation --status` fails with the guard's error, as it did when it installed the
  guard itself.
* **Broken files are logged.** `Dataset::open` logs an error for a `validation.json`,
  `rdfs.json`, `queries.json` or `graphql.json` it cannot use, so each of these
  commands now prints that line on stderr before its own output. The output and the
  exit code do not change.
* **`validation --off` needs a validator.** In a build without the `shacl` and `shex`
  features, `--off` used to print `validation off` and leave `validation.json` in place.
  It now fails with the handle's `Unsupported` error. The default build has both
  features.

The notes below record step 5, which moved the server's handlers onto the handles, and
the fix of the library's query defaults that came with it.

* **The query defaults live in `Dataset`.** `Dataset::query` applied the DESCRIBE
  setting but not RDFS on read, and `query_with`, a transaction's `query_with` and
  stored query runs passed the caller's options to the engine as they were. A dataset
  with RDFS on read set therefore answered `ASK { <urn:s> a <urn:parent> }` with false
  in the library and true over HTTP. `Dataset::query_options` now returns the dataset's
  defaults. They are RDFS on read when it is set, `urn:x-sparkles:inferred` in
  `default_graph_extra` while the dataset has a reasoning record, and the DESCRIBE
  setting. `Dataset::query`, `select`, `ask` and `construct` run with them.
* **An explicit field wins over a default.** `QueryOptions` gained `defaults_applied`.
  When it is false, as in `QueryOptions::default()`, `Dataset::with_query_defaults`
  fills each default field that the options leave at its `Default` value. A `None` in
  `rdfs`, an empty `default_graph_extra` and a default `describe` take the dataset's
  values, and a field the caller set wins. `query_options` returns options with the
  flag set, which a caller changes to turn a default off for one query, for example by
  clearing `default_graph_extra`. `query_with`, a transaction's `query_with`,
  `explain`, `StoredQueries::run`, `TextIndex::search` and `GraphQl::execute` go
  through `with_query_defaults`. The engine does not read the flag. The flag exists
  because the value types cannot say unset. `rdfs: None` cannot also mean off, and an
  empty `default_graph_extra` cannot also mean no inferences. Updates take their
  options as given, because the defaults change only what queries read.
* **Every query path starts from the defaults.** The server's `query_options` and the
  MCP tools' options build on `Dataset::query_options`, and `reasoning=false` clears
  `default_graph_extra`. `sparkles query --loc` opens a `Dataset` over the database,
  or over the branch that `--branch` names, and builds on its defaults, so it now
  follows `rdfs.json` and reads the inferences as the server does. Its `--rdfs` and
  `--rdfs-graph` still set RDFS on read for one query. `sparkles queries run` passes no
  settings of its own. The Python binding applies RDFS on read and the DESCRIBE
  setting from the defaults and keeps `include_inferred` for the inferences, which
  P01 makes opt-in. The JVM binding builds its own options over a snapshot and is
  unchanged. `crates/sparkles/tests/query_defaults.rs`, the router test
  `query_defaults` and `crates/sparkles-server/tests/cli_query_defaults.rs` check RDFS
  on read and the inferences across the library query, a stored query, a transaction
  query, the local query and HTTP.
* **The cost of the defaults.** A library query reads the RDFS setting and the
  reasoning record under their read locks and clones the options when it fills a
  default. Options that the server builds have `defaults_applied` set, so its queries
  clone nothing more.
* **The handlers call the handles.** The history, snapshot, change feed, diff and
  history query handlers call `ds.snapshots()` and `ds.history()`, and the periodic
  history upkeep calls `History::tick`. The full-text, vector and spatial index
  handlers call `ds.indexes()`. The compaction, DESCRIBE, quota, retention and change
  log handlers call `ds.settings()`. The schema handlers call `ds.schema()`, the stored
  query handlers `ds.queries()`, the reasoning and RDFS handlers `ds.reasoning()`, the
  write guard handlers and `serve --validate` `ds.validation().guard()`, and the
  GraphQL handlers `ds.graphql()`. The statistics handler calls `Dataset::stats_of`,
  and `/$/validate/data`, `iri` and `langtag` call `sparkles::io::check_data`,
  `sparkles::terms::check_iri` and `check_langtag`. The MCP tools for schema reports,
  constraints, GraphQL, stored queries and snapshots use the same handles.
* **The handles grew where a handler needed more.** `History::annotation` and
  `ChangeLogSetting::status` serve the commit listing and the history body. `TextIndex`
  and `GeoIndex` gained `enabled`. `TextIndex::search` takes a `TextSearch` and returns
  `TextHits`, with the HTML snippets the endpoint gives. `Schema::report` takes a
  `ReportRequest` with the selection, the state, the timeout, the page size and the
  cursor, and returns a `ReportOutcome` whose `classes`, `predicates` and `summary`
  give the pages, with how the report came about in `Computed`. `Schema::report_at`
  reads a state the caller opened, `diff`, `void`, `profiles_at`, `draft_shapes_at`,
  `constraints` and `constraints_at` complete it, and `ShapesRequest` moved from the
  server. `StoredQueries::bind` checks parameters for a caller that runs the query its
  own way, as the server's streaming endpoint does. `Reasoning` gained `status`,
  `freshness_at`, `run`, `run_with` and `diagnostics`, with `ReasonRequest`,
  `ReasonOutcome`, `ReasoningStatus`, `Freshness` and `Diagnostics` in
  `sparkles::reasoning`, and `ReasoningRecord::rerun` rebuilds a recorded run.
  `GraphQl` gained `put`, `draft`, `compiled`, `sdl`, `execute` and `execute_with`, and
  `sparkles::handles::graphql` holds `Backing`, `DraftRequest` and `draft_of`, which
  `sparkles graphql` uses too. `Dataset::stats` and `stats_of` return a `DatasetStats`.
* **Errors keep their HTTP statuses.** A schema discovery error that the engine has no
  variant for becomes a `schema` component error with the code `no-such-graph`,
  `timeout` or `too-many-entries` and the `SchemaError` as its source, which
  `handles::schema_error_of` returns. The server maps it to `404`, `408` and `413` as
  before, with the phase in the timeout's message. A GraphQL mapping schema that does
  not compile is a `graphql` component error with the code `invalid-schema`, whose
  source is the `PutError`, so the server still lists each error's line. A reasoning
  run that yields to a waiting write is a `reasoner` component error with the code
  `superseded`.
* **The yielding of automatic reasoning runs moved with the run.**
  `ReasonRequest::yield_to_writers` makes the run stop once a write waits for the lock
  the run holds. The server's automatic runs set it as before. The task still adds the
  `auto:` prefix to its messages, and still saves the registry after the run.
* **`DatasetStats` holds the data's counts only.** The statistics body's `quota`,
  `reasoning`, `geo`, `compaction` and Fuseki counters come from their own handles and
  the server, which add them to the serialized `DatasetStats`. This keeps the JSON of
  each in one place, since the quota body names the dataset and the reasoning status
  has the server's automatic re-run timing.
* **`check_data` returns the first error.** Parsing stops at the first syntax error,
  as the validator always did, so `sparkles::io::check_data` returns an
  `Option<DataIssue>` instead of a list. `DataSyntax::from_name` reads Jena's syntax
  names. The server's IRI and language tag checks moved from `tools::terms` to
  `sparkles::terms`, which the server re-exports for `sparkles iri`, `sparkles
  langtag` and `convert --check`.
* **What stays in the server.** Point-in-time reads still resolve `?at=` with the
  server's `snapshot_for`, because a branch's dataset falls back to its upstream for a
  commit the two share, and the library's `Dataset` does not know its branch. The SHACL
  and ShEx validation endpoints still call `sparkles-shacl` and `sparkles-shex` on
  that state, since `Validation::shacl` and `shex` read the head. The compaction task
  keeps its own progress messages. `sparkles infer` keeps its own run, because
  `--no-validate` opens the store without a guard, and it uses the library's record
  helpers. Backup creation goes with step 6.
* **`CommitRef` reads `commit:N`.** `/$/commits/{ds}/{reference}` took `commit:7`,
  which the library's `CommitRef` did not, so `CommitRef` now reads it. The route
  still refuses a commit IRI, as before.
* **The facade has new dependencies.** `base64` encodes the schema cursors, whose form
  did not change, and `oxiri` and `oxilangtag` serve `sparkles::terms`. The lock files
  of `crates/sparkles-py` and `crates/sparkles-ffi` list them.

### Phase 1 steps 6 and 8

`Catalog` owns the dataset registry and holds `catalog.lock` before opening stores or
recovering unfinished work. `inspect` reads the registry without store locks. Store
and catalog lock conflicts now return the structured `Error::Locked`; the Python
binding maps it to `DatasetLockedError` without inspecting an error message.

The catalog namespace contains datasets only. `Dataset::branch` keeps branch state
in a lazy library cache, preserving the parent's store options and closure-cache size.
The server retains HTTP routing objects, offline flags and validation observers, while
the library owns the underlying dataset objects and registry mutations.

Cloning records origin and rebases reasoning through the library. Backup capture
preserves reasoning at the captured commit and in-memory validation files. Restore
publication retains the two-rename rollback and startup-recovery behavior. HTTP
request draining and cancellation admission stay in the server around catalog calls.
The library policy engine accepts server effects for admission, active-backup claims,
clock and GC scheduling, so the server's scheduler history and task limits remain local.

The strict parity test has no pending entries. `EXTRA_KEYS` covers library-only calls,
and `bindings.toml` must contain exactly that union with three nonempty binding
columns. The Python parity test follows handle return annotations when resolving
dotted names and checks the native members. The Python column has no planned entries;
only the formatter and linter are exempt. The flat administration methods have been
replaced with native property handles.


### Phase 2: native Python handles

The Python package exposes native PyO3 handles for the catalog, snapshots, history,
settings, indexes, schema, stored queries, reasoning, validation, GraphQL and backups.
Branches remain under each dataset. Branch operations include previews, merges,
reverts, cherry-picks, protection, rename and merge-exempt predicates. No aliases remain
for the former flat administration names. Stubs and examples use the new surface;
Python parity requires the stub and native members and refuses planned entries.

Handles retain their Python dataset owner, so closing it invalidates every handle
without keeping another Rust store view alive. Catalog contexts track opened dataset
and branch views with weak references and close them, along with reservations. Dataset
aliases share the writer ownership marker to preserve the transaction deadlock check.
Commits, snapshots, backup records and change pages are immutable; history commits also
carry annotations. JSON reports use camelCase dictionaries.

Controlled calls release the GIL, check Python signals, throttle progress callbacks at
100 ms and always deliver completion. A callback exception cancels work, waits for the
worker and raises the original exception. GraphQL and file-system/S3 backups are enabled
in standard wheels; source builds can disable them. Backup errors retain their code and
catalog lock errors are subclasses of dataset lock errors.

### Phase 3: dataset management and startup

`POST /$/datasets/{ds}/rename` preserves dataset identity and needs server
administration. It refuses configured grants or active minted token scopes covering
either name, including wildcard and restricted dataset grants, and lists the blockers.
It also refuses live views and reservations, preserves the offline state and refreshes
routing and metric labels. The OpenAPI description includes this operation.

`sparkles dataset list|create|delete|rename|clone --data-dir` manages a stopped catalog.
The same commands accept `--server` and the existing remote-client credentials. Remote
clones wait for the task; both targets support JSON output. Integration tests use the
real CLI binary and a temporary running server.

Eager opening remains the default, preserving startup failure for corrupt datasets.
A local release-wheel benchmark used generated compacted persistent datasets, measured
one first open and five repeated opens, and closed every catalog between measurements.
The first measurement still had a warm filesystem cache after generation.

| Datasets | Quads per dataset | First open | Median repeated open | Maximum repeated open |
|---|---:|---:|---:|---:|
| 1 | 1,000 | 2.24 ms | 1.50 ms | 1.82 ms |
| 10 | 1,000 | 14.69 ms | 12.25 ms | 14.20 ms |
| 100 | 1,000 | 123.66 ms | 87.47 ms | 95.37 ms |
| 10 | 100,000 | 393.13 ms | 383.49 ms | 434.28 ms |

These measurements do not justify adding lazy opening and deferring corruption errors
to the first request. They measure this host and cached files, rather than cold storage
or catalogs with thousands of large datasets. An optional lazy mode would need
measurements of those workloads, including first-query latency and memory use.

### Wheel size

Both measurements used maturin 1.15.0, `--release --locked`, the same Rust release
profile (thin LTO, four codegen units), and the cp310 abi3 manylinux_2_38 x86_64 wheel.
The earlier wheel used the former default features; the completed wheel adds backup
with file-system and S3 support, GraphQL, the native handles, stubs and license notices.

| Artifact | Before | After | Change |
|---|---:|---:|---:|
| Compressed wheel | 13,842,818 bytes | 19,475,496 bytes | +40.7% |
| Native extension | 32,780,192 bytes | 47,492,968 bytes | +44.9% |

The larger default wheel is the expected cost of the requested AWS client and GraphQL
support. GCS and Azure remain source-build choices.

### Verification

The completed phases pass the full CI tasks, including 2,709 Rust tests, 124 Python
tests, 342 UI tests, JVM tests, feature checks, formatting, lint and license checks.
Nix checks pass on x86_64 Linux, including 177 packaged Python tests, 34 browser tests,
Jena clients and the NixOS VM tests. W3C, SHACL and ShEx conformance runs pass with
their existing documented exclusions. The checked-in OpenAPI description is current.


The JVM and Node packages now bind the primary catalog and dataset administration
handles. `bindings.toml` records concrete decisions for all 162 entries of the original
administration surface in Python, JVM and Node. Deferred helper and policy/utility
entries have been implemented. The newer explicit Rust branch-relink operation has
a separate binding follow-up entry. The corresponding binding specs describe
their supported controls, lifetime rules and deferred APIs.
