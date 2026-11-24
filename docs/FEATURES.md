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
The guide also covers [standards conformance](querying/README.md#standards-conformance),
the [command line](querying/README.md#command-line) and
[formatting and editor support](querying/README.md#formatter-and-editor-support).
How MIN, MAX, AVG and GROUP_CONCAT treat errors is a setting with Jena's answers as its
default, described under [aggregate errors](querying/README.md#aggregate-errors).

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
or connect through the Rust HTTP client. The guide describes Jena integration and the
administration, callback and platform coverage that differs between bindings.

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
| [SPARQL Query, Update and Graph Store protocols](querying/README.md#standards-conformance) | Complete | The listed W3C SPARQL 1.0, 1.1 Query, 1.1 Update and 1.2 conformance suites pass, and a test-only reference evaluator checks the optimizer and cursor tests against the SPARQL algebra. This scope excludes unsupported Jena extensions and Java plugin interfaces. |
| [Durable commits and dataset identity](storage/README.md#storage-and-indexes) | Complete | Transactions produce durable receipts, gap-free commit sequences, change digests, ETags and synchronized acknowledgements. |
| [Storage, loading and compaction](storage/README.md#storage-and-indexes) | Core complete | Compressed permutations, bulk imports, durable group commit, online compaction, optional FSST, startup query warm-up and optional restart prewarm are available. Adaptive compaction controls, dataset-local codec settings, remaining X16 storage consolidation and larger-scale measurements remain. |
| [History, snapshots and branches](storage/README.md#versioning-and-history) | Core complete | Retained commits, point-in-time reads, named snapshots, diffs, change feeds, branches, conflict-checked merges and relinking are available, with capped Store snapshots and trusted Dataset history handles preserving retained origins. Historical full-text search, pin rebasing and remote or archive cloning remain. |
| [Join estimation](internals/OPTIMIZATIONS.md#further-scan-and-filter-optimizations) | Partial (Phase 1) | Stars, object joins, chains and joins on several keys are estimated from characteristic sets with a remainder and from key probes. Characteristic pairs, large-input sampling and the remaining X15 join-algorithm phases stay under item 454. |
| [Streaming execution and disk spill](querying/README.md#sparql-queries-and-updates) | Partial | Eligible plans stream automatically while explicit cursors and disk spill are enabled by default, sharing one plan, join kernels, grouping operator and columnar result writers with resumable SERVICE, text and history readers. Remaining work covers eager fallbacks (SILENT SERVICE, view reads, unlimited path searches, vector ORDER BY, nearest-neighbour and some spatial forms), executor unification under item 453, ordered spill costs and automatic streaming of OPTIONAL conditions containing EXISTS. |
| [Incremental views and live queries](querying/README.md#sparql-queries-and-updates) | Partial (Phase 1) | Table views and SSE live query updates are available, including maintained property paths, the aggregates `MIN`, `MAX`, `GROUP_CONCAT` and `SAMPLE`, `HAVING`, correlated `EXISTS` and ordered windows with row positions. Graph views, commit-time maintenance, branch views, additional transports and per-fact provenance remain planned. |

### Search and graph analysis

| Capability | Status | Current scope and remaining work |
|---|---|---|
| [Full-text and vector search](search/README.md#search) | Core complete | BM25 full-text search, HNSW vector indexes, hybrid ranking and remote or local embeddings on write are available. Historical search, multi-field documents, additional language analyzers and vector quantization remain planned. |
| [Paths and graph algorithms](search/README.md#search) | Core complete | Shortest, all-shortest, k-shortest and bounded path search plus community detection, centrality, similarity and graph embeddings are available. Length-bounded weighted paths, background progress, Graphalytics acceptance and remaining API surfaces remain under items 8, 275 and 276. |
| [GeoSPARQL](search/README.md#geosparql) | Core complete | OGC GeoSPARQL 1.1 functions, spatial indexes, filters, joins, constant-geometry nearest-neighbour ordering, custom proj4 CRSs and map views are available. Per-row nearest-neighbour joins, polygon approximations, datum shifts and additional spatial syntax remain under item 24. |

### Reasoning and validation

| Capability | Status | Current scope and remaining work |
|---|---|---|
| [Reasoning](reasoning/README.md#reasoning) | Partial | RDFS, OWL 2 RL, Jena rules (including `@hybrid jena` backward rules) and Datalog clauses with relations and negation run through incremental materialization over a columnar derived tier (about 52 bytes per fact), closure operators, sampled-binding body planning, demand derivation with a budgeted table cache, `reasoning=auto` and OWL 2 QL query rewriting. Deletions that exceed an allowance fall back to a full run, and demand tables carry over across unaffected commits. A separate derived layer, canonical equality, the restricted chase, per-pattern cost selection for `auto` and shared incremental maintenance remain planned. |
| [SHACL rules](reasoning/README.md#shacl-rules) | Core complete | SHACL-AF triple and SPARQL rules with conditions, orders and fixed points, plus SPIN rules carrying `sp:text`, materialize through the `shacl-af` profile. Incremental updates remain in item 958, and RDF-only SPIN syntax and template calls remain in item 959. |
| [Explanations](reasoning/README.md#reasoning-validation-and-schema) | Partial (Phases 1–2) | Why derivation trees, bounded why-not hypotheses, entailment diffs and minimal supports are available, with optional bounded witnesses per derived fact. Provenance-based inferred visibility, review protections that read witnesses, result-cell actions and unbound why-not abduction remain planned. |
| [Validation and forms](reasoning/README.md#reasoning-validation-and-schema) | Core complete | SHACL, SHACLC and ShEx validation, write guards and shape-driven forms are available, editing nested children, ordered lists and value alternatives in one previewed commit while refusing incomplete discovery. Node-level alternatives, complex nested paths, combined guards and some target producers remain. |
| [Schema discovery](reasoning/README.md#reasoning-validation-and-schema) | Complete | Class and predicate profiles, VoID exports, schema diffs and constrained SHACL and ShEx shape drafts are available. Drafts retain the limits in the compatibility boundaries. |
| [OWL 2 ontology API](reasoning/README.md#owl-2-ontologies) | Core complete | The axiom model, RDF and functional-style mappings, profile checks, entity accessors and axiom diffs are available in Rust, Python, Node.js, the JVM (with `jena-ontapi` bridging), HTTP, MCP and the CLI, with inferred reads and the Manchester reader everywhere except MCP and the CLI. Literal checks cover XML namespace conformance, while full XML canonical lexical and value semantics remain in item 663. A read budget, fetched imports and inferred reads and entity reports over MCP and the CLI remain in items 539, 559, 565 and 578. |
| [OWL 2 classification](reasoning/README.md#owl-2-ontologies) | Core complete (EL complete) | OWL 2 EL classification is complete, including ROBOT-compatible inferred-axiom export, redundancy removal and existential superclass materialization, classifying GO, ChEBI lite and the largest ORE 2015 EL ontologies 3.1 to 20 times as fast as ELK and faster than Konclude on one and eight threads ([results](benchmarks/el-classification/RESULTS.md)). Incremental, concurrent classification also covers Horn ontologies (with individuals and OWL 2 DL datatypes and data cardinalities) and ALCHI ontologies in Rust, Python, Node.js, the JVM, HTTP and the CLI. Data case analysis, keys over checked properties, data properties in disjunctive rules, default activation of counting and rooted runs, external DL reasoner bridging and entailment justifications remain open. |
| [Ontology editor](visualization/README.md#ontology-editor) | Core complete | Ontology editing, paged class, property and individual trees, DL Query, entity history with revert, discussions, watches, bulk edits, import resolution (with import maps, XML catalogs and fetched imports), session branches, undo and on-demand metrics are available. Editable schema diagrams, classifier-backed inferred frames, pattern-based generation and remaining tree, IRI policy, grant and read-cost items are listed in the visualization guide. |
| [Thesaurus editor](visualization/README.md#thesaurus-editor) | Complete | The Workbench's Thesaurus editor edits SKOS concept schemes, concepts, collections, multilingual and SKOS-XL labels, hierarchy and mapping links as previewed commits, with drag to re-parent and built-in SKOS integrity checks. Larger trees and search, foreign mapping labels, label relations, combined guards and graph-scoped forms remain in items 638 to 642. |
| [Visual SHACL shape editor](visualization/README.md#visual-shacl-shape-editor) | Complete | Trees and forms author node and property shapes, Core targets and constraints, nested paths and severities as previewed graph commits up to 5,000 triples per shapes graph. Sample validation retains source identity, and unrecognized RDF is preserved beside the Turtle and SHACLC text validator. |
| [Kept classification reads](reasoning/README.md#owl-2-ontologies) | Partial | Rust, Python, Node.js, the JVM and HTTP expose bounded named-class hierarchy, equivalence, unsatisfiability and source pages pinned to exact kept results, while fresh RDFS or OWL RL materializations separately supply pinned property hierarchies, individual named types and derived frames. General classifier-backed entity and axiom reads, complete property and individual entailments and classification justifications remain in items 588, 622, 609 and F24. |
| [Ontology browser](visualization/README.md#ontology-browser) | Core complete | Browse gives entities and ontologies dedicated pages with linked Manchester axioms, definition disclosures, metadata cards, tree and graph hierarchies, vector-based related terms, previewed inline editing and typo-tolerant [entity search](search/README.md#entity-search) (under 8.3 ms at p99 on ChEBI lite). Named-class views read bounded kept classifications at the displayed commit, while property and individual views read fresh RDFS or OWL RL materializations with exact revision pins. General classifier-backed entailments and explanation links remain in items 588, 622, 609 and F24. |

### Server and security

| Capability | Status | Current scope and remaining work |
|---|---|---|
| [Server and Fuseki integration](server/README.md#server-and-protocols) | Core complete | Fuseki SPARQL, Graph Store and administration protocols, configuration conversion, write previews, stored queries, read-only GraphQL and OpenAPI 3.1 are available, and Linux servers keep request work on performance cores of Intel hybrid processors while they can hold it. GraphQL Phase 2 controls, OTLP metric parity and remaining Fuseki and ARQ extensions remain under items 37, 38, 39, 51, 76 and 342. |
| [Access control and authentication](server/README.md#access-control-and-security) | Core complete | Dataset, graph, branch, endpoint and triple protections pair with Basic, OIDC, API-token, trusted-proxy and native TLS authentication. Provenance-based derived visibility, guarded caller adoption of bounded view preparation and triple-restricted change-feed and validation support remain planned. |
| [Settings and declared prefixes](server/README.md#server-and-protocols) | Complete | Layered server and dataset settings, operator locks, runtime administration and declared prefix completion across HTTP, CLI, LSP and the web UI are available. |
| [Resource catalog](server/README.md#resource-catalog) | Early (Phase 1) | Versioned resource records, SPDX licence classes with propagated obligations, a NOTICE, a warning-by-default licence gate and vocabulary namespaces are available in DCAT 3 for server administrators. Remote fetching, content-addressed storage, recipes, load manifests, mirrors and export-route licence enforcement remain under items 458, 525 and 526. |

### Agents and interfaces

| Capability | Status | Current scope and remaining work |
|---|---|---|
| [Ask, ingestion and agent memory](ai/README.md#ai-and-agents) | Partial | Scoped MCP tool profiles, checked plain-language Ask queries with UI threads, Markdown, HTML, PDF and Office ingestion review and cited agent memory with fact cards and superseded timelines are available. Contextual entity linking, multimodal ingestion, approval queues, additional agent-platform controls and broader accuracy evaluations remain open. |
| [GraphRAG reports](ai/README.md#ai-and-agents) | Core complete | Hierarchical Leiden community reports (listed as a tree or treemap on dataset pages), global Ask, memory retrieval and change-feed refresh within a token budget are available. F13 view-based refresh, broader corpus evaluation and per-audience report configurations remain planned. |
| [Web graph, map and editing tools](visualization/README.md#web-ui) | Partial | Explore, the time slider, path finder, canvas lenses, derivation trees, maps, linked selections and the Pattern pane visual query builder are available, drawing up to 5,000 of 100,000 held nodes in WebGL with group nodes and filters. Live query profiling, data-quality timelines, server-cached layouts, additional query-builder blocks and rendering items 737 to 739 remain. |
| [App shell and design system](visualization/README.md#app-shell) | Complete | Every page shares a top bar with a `Ctrl+K` / `Cmd+K` command palette that searches entities, datasets, saved queries and pages, and a responsive navigation rail that narrows to icons and docks as a drawer on phones. Light and dark WCAG AA tokens and shared components keep layouts consistent across desktop, medium and phone widths. |
| [Rust, Python, JVM and Node.js APIs](integrations/README.md#libraries-and-bindings) | Core complete | Rust, Python, JVM (implementing Jena's `DatasetGraph`) and Node.js libraries embed the engine in process alongside native Rust and TypeScript HTTP clients. Foreign query-extension callbacks, reusable plans, binding administration parity, wider platform matrices and public package publication remain. |
| [Formatting, linting and command-line tools](querying/README.md#formatter-and-editor-support) | Complete | Formatting, linting, language-server completion, shell completions and Jena-compatible CLI utilities are available. The [querying guide](querying/README.md#sparql-and-formats) lists embedded-query limits and remaining linter follow-ups. |
| [Backups and operations](operations/README.md#operations) | Partial | Per-dataset incremental filesystem and S3 backups with optional repository encryption, integrity checks, format migration, degraded startup and webhook, ntfy and email notifications are available. Whole-server recovery, live-dataset encryption at rest, replication and published release artifacts remain open. |

### Evidence and planned extensions

| Capability | Status | Current scope and remaining work |
|---|---|---|
| [Shared evidence and review records](ai/README.md#evidence-profile) | Partial | Authenticated evidence proposals, reference resolution, review decisions with accepted projections, withdrawals, reversible identity resolution maps, assessment inputs and dependent lookups are available through Rust, HTTP and caller-scoped MCP tools, integrated with `recall`, Ask, claim-mode ingestion and community reports. Binding handles, proposition-term indexing, support-aware visibility and maintained analyses remain planned under items 81 and 340. |
| [Shared entity representation](ai/README.md#shared-entity-representation) | Early | Versioned profile resources, fixed acceptance fixtures and a bounded private offline resolver are available. Authenticated managed integration with the evidence, temporal and analysis runtimes remains under item 396. |
| [Durable jobs and delivery](ai/README.md#durable-jobs) | Early (Phase 1) | Idempotent submission, fenced worker leases, atomic publication of the `quads` kind, restart fencing and restored-work recovery are available through Rust and HTTP in the store's control keyspace. The transactional outbox, recurring definitions, registered maintenance jobs, bindings and UI controls remain under item 339. |
| [Maintained analysis results](specs/F22-analysis-lifecycle.md) | Planned | Item 340 specifies reproducible runs, dependency checks, freshness and guarded publication. |
| [Valid-time queries and event history](ai/README.md#valid-time-profile) | Early (Phase 1) | Versioned RDF resources, SHACL shapes, temporal stored-query templates, validity records on authenticated claims and server-owned availability that refuses backdating are available. The temporal projection helper, event-time UI and expiry integration remain under item 341. |
| [Reports for distinct audiences](specs/C22b-report-configurations.md) | Planned | Item 100 specifies multiple guarded GraphRAG configurations and shared budget enforcement. |
| [ChatGPT-plan model access](specs/C28-chatgpt-model-access.md) | Planned | Item 344 specifies personal sign-in, protected connections and Responses streaming. Existing OpenAI-compatible providers use API keys. |
| [Batched updates and triggers](specs/C23-batched-updates-and-triggers.md) | Planned | Item 36 specifies batched work, commit-trigger authoring and the writer that graph algorithms use. |
| [Additional query languages](specs/C24-json-query-languages.md) | Planned | JSON node-pattern and Datalog query surfaces remain item 72, and openCypher (including graph algorithm procedures) remains item 20. |
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
