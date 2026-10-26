# Features

Sparkles provides an RDF store, query engine, server and embedded libraries. The
category guides below describe current capabilities, configuration, usage and limits.
The [capability matrix](#capability-status) distinguishes complete, partially complete
and planned work. The [roadmap](ROADMAP.md) gives the path to completing the remaining
phases, and the [design specs](specs/README.md) explain their contracts and decisions.

* [Querying](#querying)
* [Storage and history](#storage-and-history)
* [Search and graph analysis](#search-and-graph-analysis)
* [Reasoning and validation](#reasoning-validation-and-schema)
* [Server and security](#server-and-security)
* [AI and agents](#ai-and-agents)
* [Visualization and editing](#visualization-and-editing)
* [Libraries and integrations](#libraries-and-integrations)
* [Deployment and operations](#deployment-and-operations)
* [Capability status](#capability-status)
* [Compatibility boundaries](#compatibility-boundaries)
* [Known gaps](#known-gaps)

## Feature categories

<a id="standards-conformance"></a>
<a id="sparql-queries-and-updates"></a>
<a id="query-and-update-language"></a>
<a id="jena-arq-extensions"></a>
<a id="federated-queries"></a>
<a id="planning-and-execution"></a>
<a id="explaining-queries"></a>
<a id="incremental-views-and-live-queries"></a>
<a id="command-line"></a>
<a id="formatter-and-editor-support"></a>

### Querying

Use [SPARQL Query and Update](querying/README.md#sparql-queries-and-updates),
including supported ARQ extensions, federation, query plans and incremental views.
The guide also covers [standards conformance](querying/README.md#standards-conformance),
the [command line](querying/README.md#command-line) and
[formatting and editor support](querying/README.md#formatter-and-editor-support).

<a id="storage-and-indexes"></a>
<a id="indexes-and-vocabulary"></a>
<a id="cold-reads"></a>
<a id="loading-data"></a>
<a id="transactions-and-durability"></a>
<a id="compaction-clones-and-integrity"></a>
<a id="versioning-and-history"></a>
<a id="commits"></a>
<a id="time-travel"></a>
<a id="diffs-change-feed-and-history-queries"></a>
<a id="branches-and-merges"></a>
<a id="rdf-patch"></a>

### Storage and history

Load RDF into [compressed indexes](storage/README.md#storage-and-indexes) with
durable transactions, compaction, clones and integrity checks.
[History and branches](storage/README.md#versioning-and-history) provide retained
commits, time travel, diffs, change feeds, RDF Patch and merge review.

<a id="search"></a>
<a id="full-text-search"></a>
<a id="vector-search-and-embeddings"></a>
<a id="path-search"></a>
<a id="graph-algorithms"></a>
<a id="geosparql"></a>
<a id="geometry-literals-and-functions"></a>
<a id="spatial-index-and-queries"></a>

### Search and graph analysis

Combine [full-text, vector and hybrid search](search/README.md#search) with SPARQL
patterns, path search and graph algorithms. [GeoSPARQL](search/README.md#geosparql)
adds geometry functions, spatial indexes, joins and nearest-neighbour ordering within
the documented geometry and query limits.

<a id="reasoning"></a>
<a id="shacl"></a>
<a id="shex"></a>
<a id="write-time-validation"></a>
<a id="schema-discovery-and-drafted-shapes"></a>

### Reasoning, validation and schema

Derive facts with [RDFS, OWL 2 RL and Jena rules](reasoning/README.md#reasoning),
and inspect explanations. Validate data with [SHACL](reasoning/README.md#shacl),
[ShEx](reasoning/README.md#shex) and write guards, or
[discover schema and draft shapes](reasoning/README.md#schema-discovery-and-drafted-shapes)
from existing data.

<a id="server-and-protocols"></a>
<a id="sparql-protocol-and-fuseki-compatibility"></a>
<a id="graphql"></a>
<a id="stored-queries"></a>
<a id="access-control-and-security"></a>

### Server and security

Serve [SPARQL and Graph Store protocols](server/README.md#server-and-protocols),
GraphQL and stored queries, with supported Fuseki administration and configuration.
[Access controls](server/README.md#access-control-and-security) cover datasets,
graphs and triples, with OIDC and API-token authentication.

<a id="mcp-server"></a>
<a id="questions-in-plain-language"></a>
<a id="agent-memory"></a>
<a id="document-ingestion"></a>
<a id="community-reports-graphrag"></a>

### AI and agents

Connect agents through [MCP](ai/README.md#mcp-server), turn
[questions into checked queries](ai/README.md#questions-in-plain-language), and keep
[cited memory](ai/README.md#agent-memory). Review
[document ingestion](ai/README.md#document-ingestion) and build
[GraphRAG community reports](ai/README.md#community-reports-graphrag).

[Ask stage diagnostics](ai/README.md#ask-stage-diagnostics) separate grounding, tool
and execution timings while the remaining accuracy and latency work stays open.

<a id="web-ui"></a>
<a id="query-page"></a>
<a id="explore"></a>
<a id="maps"></a>
<a id="history-and-time-travel"></a>
<a id="datasets-validation-and-editing"></a>

### Visualization and editing

Use the [web UI](visualization/README.md#web-ui) to edit queries and inspect result
tables, graphs and plans. [Explore](visualization/README.md#explore),
[maps](visualization/README.md#maps), history views and validation tools support
connected graph inspection and editing.

<a id="libraries-and-bindings"></a>

### Libraries and integrations

Embed the same engine through [Rust, Python, JVM and Node.js APIs](integrations/README.md#libraries-and-bindings),
or connect through the Rust HTTP client. The guide describes Jena integration and the
administration, callback and platform coverage that differs between bindings.

<a id="operations"></a>
<a id="deployment"></a>
<a id="settings-and-notifications"></a>
<a id="memory-and-resource-limits"></a>
<a id="observability"></a>
<a id="backups"></a>
<a id="upgrades-and-on-disk-formats"></a>

### Deployment and operations

Deploy through [NixOS, Docker or the server binary](operations/README.md#deployment).
Manage settings, resource limits, notifications and observability, and use
[backups](operations/README.md#backups), integrity checks and format migration to
operate datasets.

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
[BENCHMARKS.md](BENCHMARKS.md#where-sparkles-loses) covers performance. The summaries below link to the full limits in each category guide. Those guides name
the [roadmap](ROADMAP.md) items that address each gap, and the roadmap orders the work
and links each item to its design spec.

| Area | Remaining scope |
|---|---|
| <a id="storage-and-execution"></a>[Storage and execution](storage/README.md#storage-and-execution) | Compaction controls, larger-scale measurements and storage configuration remain. |
| <a id="versioning-and-history-1"></a>[Versioning and history](storage/README.md#versioning-and-history-1) | Historical search and further branch administration remain. |
| <a id="sparql-and-formats"></a>[SPARQL and formats](querying/README.md#sparql-and-formats) | Additional query surfaces, execution coverage, formats and editor support remain. |
| <a id="search-and-graph-algorithms"></a>[Search and graph algorithms](search/README.md#search-and-graph-algorithms) | Multi-field search, quantization and further algorithm surfaces remain. |
| <a id="geosparql-1"></a>[GeoSPARQL](search/README.md#geosparql-1) | Per-row nearest-neighbour geometry and further geometry and CRS support remain. |
| <a id="reasoning-and-validation"></a>[Reasoning and validation](reasoning/README.md#reasoning-and-validation) | Automatic reasoning selection, a separate derived layer and remaining validation and form work remain. |
| <a id="server-access-control-and-operations"></a>[Server, access control and operations](server/README.md#server-access-control-and-operations) | Further Fuseki integration, derived visibility, audit coverage and operational controls remain. |
| <a id="ai-and-agents-1"></a>[AI and agents](ai/README.md#ai-and-agents-1) | Ask accuracy, contextual linking, ingestion formats and configurable reports remain. |
| <a id="web-ui-1"></a>[Web UI](visualization/README.md#web-ui-1) | Live profiling, additional query-builder blocks and larger graph rendering remain. |
| <a id="bindings-and-packaging"></a>[Bindings and packaging](integrations/README.md#bindings-and-packaging) | Callback completion, administration parity and additional release platforms remain. |
