# Features

This page lists what Sparkles can do, grouped by area, so that you can check whether it
supports a feature and find where that feature is documented. Each entry describes the
current behaviour. [USAGE.md](USAGE.md) documents the command line and the bindings,
[API.md](API.md) documents the HTTP interface, and the design specs in
[specs/](specs/README.md) explain why each larger feature is built the way it is.
[Known gaps](#known-gaps) lists what Sparkles does not do, and each gap names the
[roadmap](ROADMAP.md) item that addresses it. [COMPARISON.md](COMPARISON.md) compares
Sparkles with Jena and Fuseki, QLever, Fluree and Oxigraph, and
[BENCHMARKS.md](BENCHMARKS.md) has the performance figures.

* [Capability status](#capability-status)
* [Standards conformance](querying/README.md#standards-conformance)
* [Storage and indexes](storage/README.md#storage-and-indexes)
* [Versioning and history](storage/README.md#versioning-and-history)
* [SPARQL queries and updates](querying/README.md#sparql-queries-and-updates)
* [Search](search/README.md#search)
* [GeoSPARQL](search/README.md#geosparql)
* [Reasoning, validation and schema](reasoning/README.md#reasoning-validation-and-schema)
* [Server and protocols](server/README.md#server-and-protocols)
* [Access control and security](server/README.md#access-control-and-security)
* [AI and agents](ai/README.md#ai-and-agents)
* [Web UI](visualization/README.md#web-ui)
* [Libraries and bindings](integrations/README.md#libraries-and-bindings)
* [Command line](querying/README.md#command-line)
* [Formatter and editor support](querying/README.md#formatter-and-editor-support)
* [Operations](operations/README.md#operations)
* [Compatibility boundaries](#compatibility-boundaries)
* [Known gaps](#known-gaps)

## Feature categories

| Category | Documentation |
|---|---|
| Querying | [Capabilities, configuration, usage and roadmap](querying/README.md) |
| Storage and history | [Capabilities, configuration, usage and roadmap](storage/README.md) |
| Search and graph analysis | [Capabilities, configuration, usage and roadmap](search/README.md) |
| Reasoning and validation | [Capabilities, configuration, usage and roadmap](reasoning/README.md) |
| Server and security | [Capabilities, configuration, usage and roadmap](server/README.md) |
| AI and agents | [Capabilities, configuration, usage and roadmap](ai/README.md) |
| Visualization and editing | [Capabilities, configuration, usage and roadmap](visualization/README.md) |
| Libraries and integrations | [Capabilities, configuration, usage and roadmap](integrations/README.md) |
| Deployment and operations | [Capabilities, configuration, usage and roadmap](operations/README.md) |

## Capability status

Status describes the scope named in each row. **Complete** means that documented scope
is available. **Partially complete** means usable functionality is available while the
listed phases or extensions remain. **In progress** identifies active implementation
that has not shipped. **Planned** means the design exists without an implementation.
Writing a spec or running validation does not make a planned capability available.
The [spec index](specs/README.md) records the phase boundaries, and the
[roadmap](ROADMAP.md) describes the remaining work. Benchmark coverage is separate from
implementation status.

| Capability | Status | Current scope and remaining work |
|---|---|---|
| [SPARQL Query, Update and Graph Store protocols](querying/README.md#standards-conformance) | Complete within the documented conformance scope. | The listed SPARQL suites pass. This does not promise every Jena extension or plugin interface. |
| [Durable commits and dataset identity](storage/README.md#storage-and-indexes) | Complete within the documented commit contract. | Transactions, receipts and synchronized acknowledgements are available. Indexed group-commit performance follow-ups remain separate measurements. |
| [Storage, loading and compaction](storage/README.md#storage-and-indexes) | Partially complete. | Compressed indexes, bulk imports, online compaction and optional FSST are available. Finer compaction controls and larger-scale measurements remain. |
| [History, snapshots and branches](storage/README.md#versioning-and-history) | Partially complete. | Retained commits, diffs, branches and merge review are available. Historical full-text search and further branch administration remain. |
| [Streaming execution and disk spill](querying/README.md#sparql-queries-and-updates) | Partially complete. | Eligible plans stream automatically, with explicit cursors and opt-in spill. Eager fallbacks, spill costs and default decisions remain. |
| [Incremental views and live queries](querying/README.md#sparql-queries-and-updates) | Partially complete. | Table views, checkpoints and SSE result changes are available. Graph views, commit-time maintenance, branch views and per-fact provenance remain planned. |
| [Full-text and vector search](search/README.md#search) | Partially complete. | BM25, HNSW, hybrid ranking and embeddings on write are available. Historical search, multi-field documents and quantization remain planned. |
| [Paths and graph algorithms](search/README.md#search) | Partially complete. | Shortest paths, communities, centrality and graph embeddings are available. Some path kernels, algorithm surfaces and larger reference comparisons remain. |
| [GeoSPARQL](search/README.md#geosparql) | Partially complete. | Indexed spatial filters, joins and constant-geometry nearest-neighbour ordering are available. Per-row nearest-neighbour geometry and the listed geometry and CRS extensions remain. |
| [Reasoning](reasoning/README.md#reasoning-validation-and-schema) | Partially complete. | RDFS, OWL 2 RL, Jena rules, incremental materialization and demand derivation are available. Automatic cost selection, the separate derived layer and canonical equality remain planned. |
| [Explanations](reasoning/README.md#reasoning-validation-and-schema) | Partially complete. | Why, bounded why-not, entailment diffs and minimal supports are available. Provenance-aware inferred visibility and review protections remain planned. |
| [Validation and forms](reasoning/README.md#reasoning-validation-and-schema) | Partially complete. | SHACL, ShEx, write guards and generated forms are available. The compatibility boundaries and remaining editors are listed below. |
| [Schema discovery](reasoning/README.md#reasoning-validation-and-schema) | Complete within the documented discovery scope. | Class and predicate profiles, schema diffs and constrained shape drafts are available. Drafts retain the limits in the compatibility boundaries. |
| [Server and Fuseki integration](server/README.md#server-and-protocols) | Partially complete. | Supported Fuseki protocols, configuration conversion and administration are available. Remaining ARQ, enhancer and integration surfaces are listed below. |
| [Access control and authentication](server/README.md#access-control-and-security) | Partially complete. | Dataset, graph and triple protections, OIDC and API tokens are available. Supported derived visibility, further audit events and authentication extensions remain planned. |
| [Settings and declared prefixes](server/README.md#server-and-protocols) | Complete within the documented settings scope. | Layered settings, operator locks, runtime administration and prefix completion are available. |
| [Ask, ingestion and agent memory](ai/README.md#ai-and-agents) | Partially complete. | Checked queries, cited memory, ingestion review and source supersession are available. Accuracy, contextual linking and additional ingest formats remain open. |
| [GraphRAG reports](ai/README.md#ai-and-agents) | Partially complete. | Community reports, global Ask, memory retrieval and change-feed refresh are available. View-based refresh and reports for distinct audiences remain planned. |
| [Web graph, map and editing tools](visualization/README.md#web-ui) | Partially complete. | Explore, paths, lenses, derivation trees, maps, linked selections and the visual query builder are available. Live profiling, additional builder blocks and larger rendering work remain. |
| [Rust, Python, JVM and Node.js APIs](integrations/README.md#libraries-and-bindings) | Partially complete. | All four embed the engine, and the JVM integrates with Jena. Administration, callback and platform coverage differs by binding. |
| [Formatting, linting and command-line tools](querying/README.md#formatter-and-editor-support) | Partially complete. | SPARQL and RDF formatting, linting, completion and the documented CLI tools are available. Additional syntaxes and editor surfaces remain planned. |
| [Backups and operations](operations/README.md#operations) | Partially complete. | Dataset backups, policies, integrity checks and format migration are available. Whole-server recovery, nested view definitions and release packaging remain open. |
| [Shared evidence and review records](specs/C25-evidence-and-assessments.md) | Planned. | Item 81 specifies source revisions, unasserted claims and managed accepted projections. Existing memory writes retain their current scope. |
| [Durable jobs and delivery](specs/C26-durable-jobs-and-delivery.md) | Planned. | Item 339 specifies checkpointed jobs, fenced workers and a transactional outbox. Current tasks and notifications do not provide that recovery contract. |
| [Maintained analysis results](specs/F22-analysis-lifecycle.md) | Planned. | Item 340 specifies reproducible runs, dependency checks, freshness and guarded publication. |
| [Valid-time queries and event history](specs/C27-valid-time-profiles.md) | Planned. | Item 341 specifies uncertain validity intervals and availability cutoffs. Existing history selects recorded commits. |
| [Reports for distinct audiences](specs/C22b-report-configurations.md) | Planned. | Item 100 specifies multiple guarded GraphRAG configurations and shared budget enforcement. |
| [ChatGPT-plan model access](specs/C28-chatgpt-model-access.md) | Planned. | Item 344 specifies personal sign-in, protected connections and Responses streaming. Existing OpenAI-compatible providers use API keys. |
| [Batched updates and triggers](specs/C23-batched-updates-and-triggers.md) | Planned. | Item 36 specifies batched work and commit-trigger authoring. |
| [Additional query languages](specs/C24-json-query-languages.md) | Planned. | JSON node-pattern and Datalog query surfaces remain item 72. openCypher remains item 20. |
| [Query by example and notebooks](#known-gaps) | Planned. | Items 28 and 30 describe these interfaces. The existing visual query builder is available. |
| [Local-first engine and ontology object API](#known-gaps) | Planned. | Items 44 and 71 describe these extensions. Ordinary RDF ontology graphs already work. |

## Standards conformance

Read the [feature documentation](querying/README.md#standards-conformance).

## Storage and indexes

Read the [feature documentation](storage/README.md#storage-and-indexes).

### Indexes and vocabulary

Read the [feature documentation](storage/README.md#indexes-and-vocabulary).

### Cold reads

Read the [feature documentation](storage/README.md#cold-reads).

### Loading data

Read the [feature documentation](storage/README.md#loading-data).

### Transactions and durability

Read the [feature documentation](storage/README.md#transactions-and-durability).

### Compaction, clones and integrity

Read the [feature documentation](storage/README.md#compaction-clones-and-integrity).

## Versioning and history

Read the [feature documentation](storage/README.md#versioning-and-history).

### Commits

Read the [feature documentation](storage/README.md#commits).

### Time travel

Read the [feature documentation](storage/README.md#time-travel).

### Diffs, change feed and history queries

Read the [feature documentation](storage/README.md#diffs-change-feed-and-history-queries).

### Branches and merges

Read the [feature documentation](storage/README.md#branches-and-merges).

### RDF Patch

Read the [feature documentation](storage/README.md#rdf-patch).

## SPARQL queries and updates

Read the [feature documentation](querying/README.md#sparql-queries-and-updates).

### Query and update language

Read the [feature documentation](querying/README.md#query-and-update-language).

### Jena ARQ extensions

Read the [feature documentation](querying/README.md#jena-arq-extensions).

### Federated queries

Read the [feature documentation](querying/README.md#federated-queries).

### Planning and execution

Read the [feature documentation](querying/README.md#planning-and-execution).

### Explaining queries

Read the [feature documentation](querying/README.md#explaining-queries).

### Incremental views and live queries

Read the [feature documentation](querying/README.md#incremental-views-and-live-queries).

## Search

Read the [feature documentation](search/README.md#search).

### Full-text search

Read the [feature documentation](search/README.md#full-text-search).

### Vector search and embeddings

Read the [feature documentation](search/README.md#vector-search-and-embeddings).

### Path search

Read the [feature documentation](search/README.md#path-search).

### Graph algorithms

Read the [feature documentation](search/README.md#graph-algorithms).

## GeoSPARQL

Read the [feature documentation](search/README.md#geosparql).

### Geometry literals and functions

Read the [feature documentation](search/README.md#geometry-literals-and-functions).

### Spatial index and queries

Read the [feature documentation](search/README.md#spatial-index-and-queries).

## Reasoning, validation and schema

Read the [feature documentation](reasoning/README.md#reasoning-validation-and-schema).

### Reasoning

Read the [feature documentation](reasoning/README.md#reasoning).

### SHACL

Read the [feature documentation](reasoning/README.md#shacl).

### ShEx

Read the [feature documentation](reasoning/README.md#shex).

### Write-time validation

Read the [feature documentation](reasoning/README.md#write-time-validation).

### Schema discovery and drafted shapes

Read the [feature documentation](reasoning/README.md#schema-discovery-and-drafted-shapes).

## Server and protocols

Read the [feature documentation](server/README.md#server-and-protocols).

### SPARQL protocol and Fuseki compatibility

Read the [feature documentation](server/README.md#sparql-protocol-and-fuseki-compatibility).

### GraphQL

Read the [feature documentation](server/README.md#graphql).

### Stored queries

Read the [feature documentation](server/README.md#stored-queries).

## Access control and security

Read the [feature documentation](server/README.md#access-control-and-security).

## AI and agents

Read the [feature documentation](ai/README.md#ai-and-agents).

### MCP server

Read the [feature documentation](ai/README.md#mcp-server).

### Questions in plain language

Read the [feature documentation](ai/README.md#questions-in-plain-language).

### Agent memory

Read the [feature documentation](ai/README.md#agent-memory).

### Document ingestion

Read the [feature documentation](ai/README.md#document-ingestion).

### Community reports (GraphRAG)

Read the [feature documentation](ai/README.md#community-reports-graphrag).

## Web UI

Read the [feature documentation](visualization/README.md#web-ui).

### Query page

Read the [feature documentation](visualization/README.md#query-page).

### Explore

Read the [feature documentation](visualization/README.md#explore).

### Maps

Read the [feature documentation](visualization/README.md#maps).

### History and time travel

Read the [feature documentation](visualization/README.md#history-and-time-travel).

### Datasets, validation and editing

Read the [feature documentation](visualization/README.md#datasets-validation-and-editing).

## Libraries and bindings

Read the [feature documentation](integrations/README.md#libraries-and-bindings).

## Command line

Read the [feature documentation](querying/README.md#command-line).

## Formatter and editor support

Read the [feature documentation](querying/README.md#formatter-and-editor-support).

## Operations

Read the [feature documentation](operations/README.md#operations).

### Deployment

Read the [feature documentation](operations/README.md#deployment).

### Settings and notifications

Read the [feature documentation](operations/README.md#settings-and-notifications).

### Memory and resource limits

Read the [feature documentation](operations/README.md#memory-and-resource-limits).

### Observability

Read the [feature documentation](operations/README.md#observability).

### Backups

Read the [feature documentation](operations/README.md#backups).

### Upgrades and on-disk formats

Read the [feature documentation](operations/README.md#upgrades-and-on-disk-formats).

## Compatibility boundaries

These are documented scope and compatibility choices. They do not mark a completed
roadmap item as unfinished or promise another implementation.

* Predicate patterns answer counts of distinct subjects, rather than `COUNT(*)` per
  predicate. Queries that count triples use the ordinary permutations. Encoded IRIs and
  alternative block codecs are not available because their measured savings did not
  meet the adoption gates in [X11](specs/X11-index-compression-and-predicate-patterns.md#phases-not-built).
* A headerless WAL that has never been opened for writing by the current format keeps
  its legacy interpretation. A torn record left by an older build can therefore be
  read as a legacy commit or refused, as [X09](specs/X09-group-commit-by-default.md#phase-3)
  describes.
* ShEx 2.2 does not support `RESTRICTS` or `EXACTLY` reference syntax. SHACL 1.2 reifier
  constraints, `sh:uniqueValuesFor`, `sh:subsetOf`, `sh:someValue`, `sh:rootClass`,
  `sh:singleLine` and SHACL 1.2 custom functions are outside the supported scope. The
  [G02](specs/G02-shex/OUTCOME.md#roadmap-follow-ups-2026-10-12) and
  [G03](specs/G03-shaclc.md#roadmap-follow-ups-2026-10-12) Outcomes give the details.
* Drafted shapes link another class shape only at support 1, derive patterns only from
  all-string properties and ranges only from inline numbers and dates, and follow
  sequence paths one level deep. ShEx drafts leave sequence paths and date ranges as
  comments. [C02](specs/C02-schema-discovery.md#roadmap-follow-ups-2026-10-12) defines
  this drafting scope.
* SP²Bench `q4` at 10M produces 1.96 GB of TSV, exceeding the default 1 GiB response
  budget. It requires a larger `--max-result-mb`. This is a configurable result limit.

## Known gaps

These are implementation limits, missing controls, and features that other RDF stores
have and Sparkles does not. [COMPARISON.md](COMPARISON.md) covers each engine, and
[BENCHMARKS.md](BENCHMARKS.md#where-sparkles-loses) covers performance. Each gap names
the [roadmap](ROADMAP.md) items that address it, and the roadmap orders that work and
links each item to its design spec.

### Storage and execution

Read the [current feature limits](storage/README.md#storage-and-execution).

### Versioning and history

Read the [current feature limits](storage/README.md#versioning-and-history-1).

### SPARQL and formats

Read the [current feature limits](querying/README.md#sparql-and-formats).

### Search and graph algorithms

Read the [current feature limits](search/README.md#search-and-graph-algorithms).

### GeoSPARQL

Read the [current feature limits](search/README.md#geosparql-1).

### Reasoning and validation

Read the [current feature limits](reasoning/README.md#reasoning-and-validation).

### Server, access control and operations

Read the [current feature limits](server/README.md#server-access-control-and-operations).

### AI and agents

Read the [current feature limits](ai/README.md#ai-and-agents-1).

### Web UI

Read the [current feature limits](visualization/README.md#web-ui-1).

### Bindings and packaging

Read the [current feature limits](integrations/README.md#bindings-and-packaging).
