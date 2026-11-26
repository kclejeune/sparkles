# Features

Sparkles provides an RDF store, query engine, server and embedded libraries. The
category guides below describe current capabilities, configuration, usage and limits.
The standard server includes local embeddings, OCR support and encrypted repositories.
Models, OCR libraries and encrypted repository keys require runtime configuration.
The [capability matrix](#capability-status) grades each area on a five-tier maturity
scale from planned through complete. The [roadmap](ROADMAP.md) gives the path to
completing the remaining phases, and the [design specs](specs/README.md) explain their
contracts and decisions.

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

### Querying

Use [SPARQL Query and Update](querying/README.md#sparql-queries-and-updates),
including supported ARQ extensions, federation, query plans and incremental views.
[Standards conformance](querying/README.md#standards-conformance) describes the reference
evaluator and its differential comparisons. The guide also covers the
[command line](querying/README.md#command-line) and
[formatting and editor support](querying/README.md#formatter-and-editor-support).
How MIN, MAX, AVG and GROUP_CONCAT treat errors is a setting with Jena's answers as its
default, described under [aggregate errors](querying/README.md#aggregate-errors).
Eager and streaming query operators apply memory limits to vocabulary decoding and
preserve a refused decode's original error.


### Storage and history

Load RDF into [compressed indexes](storage/README.md#storage-and-indexes) with durable
transactions, compaction, clones and integrity checks.
[History and branches](storage/README.md#versioning-and-history) provide retained
commits, time travel, diffs, change feeds, RDF Patch and merge review, including nested
triple terms in HTTP history and diff filters. Rust branch administration refuses with a
conflict when the calling thread already holds a needed write transaction, leaving that
transaction usable.

### Search and graph analysis

Combine [full-text, vector and hybrid search](search/README.md#search) with SPARQL
patterns, path search and graph algorithms. [GeoSPARQL](search/README.md#geosparql)
adds geometry functions, spatial indexes, joins and nearest-neighbour ordering within
the documented geometry and query limits.

### Reasoning, validation and schema

Derive facts with [RDFS, OWL 2 RL, OWL 2 QL, Jena rules and Datalog clauses](reasoning/README.md#reasoning)
or [SHACL and SPIN rules](reasoning/README.md#shacl-rules), inspect explanations, and
read, check, edit and classify [OWL 2 ontologies](reasoning/README.md#owl-2-ontologies)
(including the OWL form of [OML models](reasoning/README.md#oml-and-opencaesar-models)).
Validate data with [SHACL](reasoning/README.md#shacl),
[ShEx](reasoning/README.md#shex) and write guards, or
[discover schema and draft shapes](reasoning/README.md#schema-discovery-and-drafted-shapes)
from existing data. ShEx write guards verify copied schemas before installation or reuse,
and legacy configurations without a copy digest validate the head in full at install.

### Server and security

Serve [SPARQL and Graph Store protocols](server/README.md#server-and-protocols),
GraphQL and stored queries, with supported Fuseki administration and configuration.
[Access controls](server/README.md#access-control-and-security) cover datasets,
graphs and triples, with OIDC and API-token authentication.

### AI and agents

Connect agents through [MCP](ai/README.md#mcp-server) with scoped tool profiles
(`explore`, `memory`, `admin`), semantic search (`search_semantic`) and bounded entity
linking. Turn [plain-language questions into checked queries](ai/README.md#questions-in-plain-language)
grounded in entity predicate usage, keep [cited agent memory](ai/README.md#agent-memory),
ingest Markdown, HTML, PDF and Office documents through
[document ingestion](ai/README.md#document-ingestion), and build
[GraphRAG community reports](ai/README.md#community-reports-graphrag).

Anthropic providers cache stable prompt prefixes by default with separate cache pricing,
and [Ask stage diagnostics](ai/README.md#ask-stage-diagnostics) break down grounding,
tool and execution timings while surfacing ambiguous mentions early. The web UI runs
questions in an [Ask thread](ai/README.md#the-ask-thread) at `/ui/ask`
([screenshot](images/ask-answer.png)) or as a `Shift+A` drawer beside any page
([screenshot](images/ask-drawer.png)), with folded queries, rows, sources, follow-ups
and saved thread history.

### Visualization and editing

Use the [web UI](visualization/README.md#web-ui) to write queries, inspect result tables,
graphs and plans, and explore connected data with [Explore](visualization/README.md#explore),
[maps](visualization/README.md#maps), history views and validation overlays. Every page
shares one [app shell](visualization/README.md#app-shell) with WCAG AA light and dark
tokens, a responsive navigation rail and a `Ctrl+K` / `Cmd+K` command palette
(`>`, `@`, `#`, `?` filter commands, entities, datasets and saved queries).

For schema and vocabulary authoring, the [ontology editor](visualization/README.md#ontology-editor)
edits OWL classes, properties, individuals and annotations as previewed commits beside
asserted and inferred views, with DL Query, per-entity commit history, Web Annotation
discussion threads, entity and subtree watches (with optional email notifications), bulk
edits up to 1,000 changes per commit and an Imports closure manager with OASIS XML
catalog support. The [ontology browser](visualization/README.md#ontology-browser) gives
each entity a dedicated page with linked Manchester prose and inline edit mode, the
[thesaurus editor](visualization/README.md#thesaurus-editor) manages SKOS and SKOS-XL
hierarchies against SKOS integrity rules, the
[visual SHACL shape editor](visualization/README.md#visual-shacl-shape-editor) authors
shapes in trees and forms, and [SHACL-driven forms](visualization/README.md#nested-forms-lists-and-alternatives)
edit nested blank nodes, ordered lists and value alternatives in one previewed commit.
See the [visualization guide](visualization/README.md#web-ui) for screenshots and
walkthroughs of each view.

### Libraries and integrations

Embed the same engine through [Rust, Python, JVM and Node.js APIs](integrations/README.md#libraries-and-bindings),
or connect through the Rust HTTP client. Python branch updates clear notes on explicit
`note=None` while preserving them when `note` is omitted, and the guide describes Jena
integration and the administration, callback and platform coverage that differs between
bindings.

### Deployment and operations

Deploy through [NixOS, Docker or the server binary](operations/README.md#deployment).
Manage settings, resource limits, notifications and observability, and use
[backups](operations/README.md#backups), integrity checks and format migration to
operate datasets. The server listens before its datasets open, reports each dataset as
opening, open or failed in its readiness check, and serves the others when one fails to
open.

## Capability status

Status grades each capability's named scope on a five-tier maturity scale. Writing a spec
or running validation does not make a planned capability available, and benchmark coverage
is tracked separately from implementation status.

* **Complete.** The documented scope is shipped.
* **Core complete.** Primary workflows and capabilities are shipped, while secondary
  extensions or edge cases remain.
* **Partial.** A usable core is shipped while major planned phases remain.
* **Early.** Foundational primitives or an initial phase are shipped, while primary
  end-to-end workflows remain ahead.
* **Planned.** The design is specified without an implementation.

The [spec index](specs/README.md) records the phase boundaries, and the
[roadmap](ROADMAP.md) describes the remaining work. Benchmark results qualify the recorded
host and executable. A pinned
[instruction-count diagnostic](benchmarks/synthetic/RESULTS.md#query-layout-after-the-classification-batch)
records query timing differences between executables with unchanged plans, answers and
instruction counts, and the limits of attributing them to layout.

### Querying and storage

| Capability | Status | Current scope and remaining work |
|---|---|---|
| [SPARQL Query, Update and Graph Store protocols](querying/README.md#standards-conformance) | Complete | W3C SPARQL 1.0, 1.1 Query, 1.1 Update, and 1.2 conformance suites pass, checked against a reference algebra evaluator, with strict XSD 1.1 float and double lexical forms, XPath string casts and regular expressions, and bounded arbitrary-precision `xsd:integer` and `xsd:decimal` evaluation. String functions enforce SPARQL 1.2 language-tag and base-direction compatibility and preserve directional string results across scalar evaluation and key filters. Unsupported Jena extensions and Java plugins remain outside scope. |
| [Durable commits and dataset identity](storage/README.md#storage-and-indexes) | Complete | Transactions produce durable receipts, gap-free commit sequences, change digests, ETags and synchronized acknowledgements. |
| [Storage, loading and compaction](storage/README.md#storage-and-indexes) | Core complete | Compressed permutations, unified bulk loading (including large single-operation SPARQL `LOAD`), group commit, online compaction, optional FSST and startup prewarm are available. Column frame, builder run and pattern readers reject overflowing tenth-byte varints as corruption. Adaptive compaction, per-dataset codecs, remaining X16 consolidation and larger-scale measurements remain. |
| [History, snapshots and branches](storage/README.md#versioning-and-history) | Core complete | Retained commits, time travel, snapshots, diffs, change feeds, branches and conflict-checked merges are available. Historical full-text search, pin rebasing and remote or archive cloning remain. |
| [Join estimation](internals/OPTIMIZATIONS.md#further-scan-and-filter-optimizations) | Partial (Phase 1) | Characteristic sets and key probes estimate star, object, chain and multi-key joins. Estimated plans use the request's initial bindings. Characteristic pairs, large-input sampling and the remaining X15 join-algorithm phases stay under item 454. |
| [Streaming execution and disk spill](querying/README.md#sparql-queries-and-updates) | Partial | All built-in plan forms run through native cursors with default disk spill, shared join and grouping kernels, arbitrary-depth nested `EXISTS`, and resumable readers for `SERVICE`, text, history, spatial, path search, views and leaf property functions. Late-head non-`SILENT` `SERVICE` responses remain under item 1125, and ordered spill costs and executor unification remain under item 453. |
| [Incremental views and live queries](querying/README.md#sparql-queries-and-updates) | Partial (Phase 1) | Maintained table views with paths, aggregates, `HAVING`, correlated `EXISTS` and windows support streamed reads and SSE live queries. Graph views, commit-time maintenance, branch views and per-fact provenance remain planned. |

### Search and graph analysis

| Capability | Status | Current scope and remaining work |
|---|---|---|
| [Full-text and vector search](search/README.md#search) | Core complete | BM25 full-text search, HNSW vector indexes, hybrid ranking and local or remote embeddings on write are available. Historical search, multi-field documents, more analyzers and vector quantization remain planned. |
| [Paths and graph algorithms](search/README.md#search) | Core complete | Shortest, k-shortest and bounded path search run beside community detection, centrality, similarity and graph embeddings. Weighted length bounds, background progress and Graphalytics acceptance remain under items 8, 275 and 276. |
| [GeoSPARQL](search/README.md#geosparql) | Core complete | OGC GeoSPARQL 1.1 functions, spatial indexes, joins, constant-geometry nearest-neighbour ordering, custom CRSs and map views are available. Per-row nearest-neighbour joins, polygon approximations, datum shifts and extra syntax remain under item 24. |

### Reasoning and validation

| Capability | Status | Current scope and remaining work |
|---|---|---|
| [Reasoning](reasoning/README.md#reasoning) | Partial | Materialized RDFS, OWL 2 RL, Jena rules and Datalog programs update incrementally through the shared maintenance core with operator-maintained closures, per-list tracking for ordinary explicit RDF list edits and cost-estimated deletion bounds. Query-time RDFS on read, demand derivation, `reasoning=auto` and OWL 2 QL rewriting are also available. A separate derived layer, derivation records, stratified `noValue` maintenance, canonical equality, restricted chase and per-pattern `auto` selection remain planned. |
| [SHACL rules](reasoning/README.md#shacl-rules) | Core complete | SHACL-AF triple and SPARQL rules plus `sp:text` SPIN rules materialize under the `shacl-af` profile. Incremental updates remain in item 958, and RDF-only SPIN syntax and templates remain in item 959. |
| [Explanations](reasoning/README.md#reasoning-validation-and-schema) | Partial (Phases 1–2) | Derivation trees, bounded why-not hypotheses, entailment diffs, minimal supports and optional bounded witnesses are available. Provenance-based inferred visibility, witness-aware review guards and unbound why-not abduction remain planned. |
| [Validation and forms](reasoning/README.md#reasoning-validation-and-schema) | Core complete | SHACL, SHACLC and ShEx validation, write guards and nested shape-driven forms commit edits in one previewed transaction. Node-level alternatives, complex nested paths, combined guards and some target producers remain. |
| [Schema discovery](reasoning/README.md#reasoning-validation-and-schema) | Complete | Class and predicate profiles, VoID exports, schema diffs and constrained SHACL and ShEx shape drafts are available. Drafts retain the limits in the compatibility boundaries. |
| [OWL 2 ontology API](reasoning/README.md#owl-2-ontologies) | Core complete | Axioms, syntax mappings, profile checks and diffs span libraries, HTTP, MCP and CLI. Fetched imports, read budgets, XML literal semantics and MCP or CLI inferred reads remain in items 539, 559, 565, 578 and 663. |
| [OWL 2 classification](reasoning/README.md#owl-2-ontologies) | Core complete (EL complete) | OWL 2 EL classification is complete with ROBOT export and published [results](benchmarks/el-classification/RESULTS.md), alongside ALCHI and Horn classification with individuals, positive object minima above one, sound partial answers across every tier, and Workbench partial-classification controls and display. Opt-in rooted rewriting can retain a Horn or EL result. Remaining DL rules, default counting, rooted runs and binary constant-equality splitting, external reasoner bridging and justifications remain open. |
| [Ontology editor](visualization/README.md#ontology-editor) | Core complete | Ontology editing includes paged entity trees, DL Query, history revert, discussions, watches, bulk edits, pattern-based axiom generation, import resolution and session branches. Editable schema diagrams and general classifier-backed inferred frames remain in the visualization guide. |
| [Thesaurus editor](visualization/README.md#thesaurus-editor) | Complete | The Thesaurus editor manages SKOS schemes, concepts, collections, SKOS-XL labels, hierarchies and mappings as previewed commits with integrity checks. Larger trees, foreign mapping labels, label relations and graph-scoped forms remain in items 638 to 642. |
| [Visual SHACL shape editor](visualization/README.md#visual-shacl-shape-editor) | Complete | Trees and forms author SHACL Core shapes, nested paths and severities as previewed graph commits with sample validation. Unrecognized RDF is preserved beside the Turtle and SHACLC text validator. |
| [Kept classification reads](reasoning/README.md#owl-2-ontologies) | Partial | Libraries and HTTP expose revision-pinned named-class classification pages, while RDFS or OWL RL materializations back property and individual reads. General classifier-backed reads, full entailments and justifications remain in items 588, 622, 609 and F24. |
| [Ontology browser](visualization/README.md#ontology-browser) | Core complete | Dedicated entity and ontology pages provide linked Manchester axioms, hierarchies, related terms, inline editing and typo-tolerant [entity search](search/README.md#entity-search). General classifier-backed entailments and explanation links remain in items 588, 622, 609 and F24. |

### Server and security

| Capability | Status | Current scope and remaining work |
|---|---|---|
| [Server and Fuseki integration](server/README.md#server-and-protocols) | Core complete | Fuseki protocols, config conversion, write previews, stored queries, read-only GraphQL and OpenAPI 3.1 are available. GraphQL Phase 2, OTLP metrics and Fuseki or ARQ extensions remain under items 37, 38, 39, 51, 76 and 342. |
| [Access control and authentication](server/README.md#access-control-and-security) | Core complete | Dataset, graph, branch, endpoint and triple guards pair with Basic, OIDC, API-token, trusted-proxy and TLS authentication. Provenance-based derived visibility, guarded view preparation and triple-restricted change feeds or validation remain planned. |
| [Settings and declared prefixes](server/README.md#server-and-protocols) | Complete | Layered server and dataset settings, operator locks, runtime administration and declared prefix completion span HTTP, the CLI, LSP and the web UI. |
| [Resource catalog](server/README.md#resource-catalog) | Early (Phase 1) | DCAT 3 resource records, SPDX licence tracking, NOTICE generation, warning licence gates and vocabulary namespaces are available for administrators. Remote fetching, content-addressed storage, recipes, mirrors and export enforcement remain under items 458, 525 and 526. |

### Agents and interfaces

| Capability | Status | Current scope and remaining work |
|---|---|---|
| [Ask, ingestion and agent memory](ai/README.md#ai-and-agents) | Partial | Scoped MCP profiles, checked Ask threads, document ingestion review and cited agent memory are available. Contextual entity linking, multimodal ingestion, approval queues, extra platform controls and broader evaluations remain open. |
| [GraphRAG reports](ai/README.md#ai-and-agents) | Core complete | Hierarchical Leiden community reports, global Ask, memory retrieval and budgeted change-feed refresh are available. View-based refresh, broader corpus evaluation and per-audience report configurations remain planned. |
| [Web graph, map and editing tools](visualization/README.md#web-ui) | Partial | Explore, WebGL graph rendering, the time slider, path finder, maps, derivation trees and visual query builder are available. Live profiling, data-quality timelines, server-cached layouts and rendering items 737 to 739 remain. |
| [App shell and design system](visualization/README.md#app-shell) | Complete | Every page shares a top bar with a `Ctrl+K` / `Cmd+K` command palette, a responsive navigation rail and shared components. Light and dark WCAG AA tokens cover desktop through phone layouts. |
| [Rust, Python, JVM and Node.js APIs](integrations/README.md#libraries-and-bindings) | Core complete | In-process Rust, Python, Node.js and Jena-compatible JVM bindings ship beside native Rust and TypeScript HTTP clients. Foreign query callbacks, reusable plans, admin parity, wider platforms and package publication remain. |
| [Formatting, linting and command-line tools](querying/README.md#formatter-and-editor-support) | Complete | Formatting, linting, language-server completion, shell completions and Jena-compatible CLI utilities are available. The [querying guide](querying/README.md#sparql-and-formats) lists embedded-query limits and remaining linter follow-ups. |
| [Backups and operations](operations/README.md#operations) | Partial | Incremental filesystem and S3 backups with optional encryption, integrity checks, migrations, interrupted-delete recovery, degraded startup and notifications are available. Whole-server recovery, live encryption at rest, replication and published release artifacts remain open. |

### Evidence and planned extensions

| Capability | Status | Current scope and remaining work |
|---|---|---|
| [Shared evidence and review records](ai/README.md#evidence-profile) | Partial | Bounded evidence resolution, proposals, reviews, withdrawals, identity maps and dependent lookups span Rust, HTTP and MCP, integrated with Ask, ingestion and reports. Proposals hold referenced commits across catalog datasets, and deployments can declare and lock evidence settings. Live snapshots and persistent history-cache hits charge a 4 KiB delta handle under the 64 MiB allowance, with size-based charges for in-memory historical deltas, private replays and non-live generations. Binding handles, proposition indexing, support-aware visibility and maintained analyses remain under items 81 and 340. |
| [Shared entity representation](ai/README.md#shared-entity-representation) | Early | Versioned profile resources, acceptance fixtures and a bounded offline resolver are available. Managed integration with the evidence, temporal and analysis runtimes remains under item 396. |
| [Durable jobs and delivery](ai/README.md#durable-jobs) | Early (Phase 1) | Idempotent submission, fenced leases, atomic `quads` publication and crash recovery are available over Rust and HTTP. The transactional outbox, recurring jobs, maintenance jobs, bindings and UI controls remain under item 339. |
| [Maintained analysis results](specs/F22-analysis-lifecycle.md) | Planned | Item 340 specifies reproducible runs, dependency checks, freshness and guarded publication. |
| [Valid-time queries and event history](ai/README.md#valid-time-profile) | Early (Phase 1) | Versioned RDF resources, SHACL shapes, temporal query templates and non-backdated claim validity records are available. The temporal projection helper, event-time UI and expiry integration remain under item 341. |
| [Reports for distinct audiences](specs/C22b-report-configurations.md) | Planned | Item 100 specifies multiple guarded GraphRAG configurations and shared budget enforcement. |
| [ChatGPT-plan model access](specs/C28-chatgpt-model-access.md) | Planned | Item 344 specifies personal sign-in, protected connections and Responses streaming. Existing OpenAI-compatible providers use API keys. |
| [Batched updates and triggers](specs/C23-batched-updates-and-triggers.md) | Planned | Item 36 specifies batched work, commit-trigger authoring and the writer that graph algorithms use. |
| [Additional query languages](specs/C24-json-query-languages.md) | Planned | JSON node-pattern and Datalog query surfaces remain item 72, and openCypher with graph algorithm procedures remains item 20. |
| [Query by example and notebooks](#known-gaps) | Planned | Items 28 and 30 specify these interfaces beside the existing visual query builder. |
| [Local-first engine](#known-gaps) | Planned | Item 44 specifies the WebAssembly engine over browser OPFS and edge runtimes. |

## Compatibility boundaries

These are documented scope and compatibility choices. They do not mark a completed
roadmap item as unfinished or promise another implementation.

* Predicate patterns answer counts of distinct subjects, rather than `COUNT(*)` per
  predicate. Queries that count triples use the ordinary permutations. Encoded IRIs and
  alternative block codecs are not available because their measured savings did not
  meet the adoption gates in [X11](specs/X11-index-compression-and-predicate-patterns.md#phases-not-built).
* Databases and backups written before the first release are refused with a message
  that names `sparkles dump` from the release that wrote them and `sparkles load`, as
  [X16](specs/X16-storage-consolidation/SPEC.md#phase-1-first-release-format-cutoff-item-479)
  describes. There is no upgrade step for them.
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
| [Storage and execution](storage/README.md#storage-and-execution) | Compaction controls, larger-scale measurements and storage configuration remain. |
| [Versioning and history](storage/README.md#versioning-and-history-1) | Historical search and further branch administration remain. |
| [SPARQL and formats](querying/README.md#sparql-and-formats) | Additional query surfaces, execution coverage, formats and editor support remain. |
| [Search and graph algorithms](search/README.md#search-and-graph-algorithms) | Multi-field search, quantization and further algorithm surfaces remain. |
| [GeoSPARQL](search/README.md#geosparql-1) | Item 24 adds per-row nearest-neighbour joins, spatial syntax, polygon approximations and further CRS support. |
| [Reasoning and validation](reasoning/README.md#reasoning-and-validation) | A separate derived layer, canonical equality, per-pattern `auto` selection, complete OWL 2 DL classification, MCP and CLI ontology entity and inferred reads, and remaining validation and form work remain. |
| [Server, access control and operations](server/README.md#server-access-control-and-operations) | Further Fuseki integration, derived visibility and operational controls remain. |
| [AI and agents](ai/README.md#ai-and-agents-1) | Ask accuracy, contextual linking, ingestion formats and configurable reports remain. |
| [Web UI](visualization/README.md#web-ui-1) | Live profiling, additional query-builder blocks and larger graph rendering remain. |
| [Bindings and packaging](integrations/README.md#bindings-and-packaging) | Callback completion, administration parity and additional release platforms remain. |
